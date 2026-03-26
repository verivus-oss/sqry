//! rust-analyzer integration bridge for Rust.
//!
//! This module provides integration with rust-analyzer for enhanced analysis:
//! - Type inference for trait method binding
//! - Elided lifetime resolution
//! - Macro expansion (HIR-based)
//!
//! # Version Pinning
//!
//! For CI stability, rust-analyzer version is pinned to a specific version
//! including commit hash. Use `verify_version` to check compatibility.
//!
//! # Security Note
//!
//! rust-analyzer performs build script execution and macro expansion,
//! which can execute arbitrary code. This module respects the
//! `enable_macro_expansion` configuration flag.

use crate::confidence::ConfidenceTracker;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// Errors that can occur during rust-analyzer bridge operations.
#[derive(Debug)]
pub enum RaBridgeError {
    /// rust-analyzer is not available in PATH
    NotFound,
    /// rust-analyzer version doesn't match expected
    VersionMismatch { expected: String, actual: String },
    /// Failed to execute rust-analyzer
    ExecutionFailed(String),
    /// Failed to parse rust-analyzer output
    ParseError(String),
    /// Workspace not initialized
    WorkspaceNotInitialized,
    /// Operation not supported in current configuration
    NotSupported(String),
}

impl std::fmt::Display for RaBridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "rust-analyzer not found in PATH"),
            Self::VersionMismatch { expected, actual } => {
                write!(
                    f,
                    "rust-analyzer version mismatch: expected '{expected}', got '{actual}'"
                )
            }
            Self::ExecutionFailed(msg) => write!(f, "rust-analyzer execution failed: {msg}"),
            Self::ParseError(msg) => write!(f, "failed to parse rust-analyzer output: {msg}"),
            Self::WorkspaceNotInitialized => write!(f, "workspace not initialized"),
            Self::NotSupported(msg) => write!(f, "operation not supported: {msg}"),
        }
    }
}

impl std::error::Error for RaBridgeError {}

/// rust-analyzer version information.
///
/// Used for version pinning and compatibility checking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaVersionInfo {
    /// Full version string from `rust-analyzer --version`
    /// e.g., "rust-analyzer 2024-01-15 7e74ef8"
    pub version_string: String,
    /// Date portion of the version
    pub date: String,
    /// Commit hash portion of the version
    pub commit_hash: String,
}

impl RaVersionInfo {
    /// Parse version info from a version string.
    ///
    /// Expected format: "rust-analyzer YYYY-MM-DD HASH"
    #[must_use]
    pub fn parse(version_string: &str) -> Option<Self> {
        let parts: Vec<&str> = version_string.split_whitespace().collect();
        if parts.len() >= 3 && parts[0] == "rust-analyzer" {
            Some(Self {
                version_string: version_string.to_string(),
                date: parts[1].to_string(),
                commit_hash: parts[2].to_string(),
            })
        } else {
            None
        }
    }

    /// Check if this version matches another (by full string comparison).
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        self.version_string == other.version_string
    }
}

/// Result of checking rust-analyzer availability.
///
/// This is the result of a single subprocess call to `rust-analyzer --version`.
/// It distinguishes between:
/// - RA not found (spawn failed) → `available = false`, no warning
/// - RA exits with error → `available = false`, warning with exit status
/// - RA available, version parsed → `available = true`, version present
/// - RA available, version parse failed → `available = true`, version None, warning present
#[derive(Debug, Clone)]
pub struct RaAvailabilityCheck {
    /// Whether rust-analyzer could be executed successfully
    pub available: bool,
    /// Parsed version info (None if parse failed but RA is available)
    pub version: Option<RaVersionInfo>,
    /// Warning if version couldn't be parsed or RA exited with error (RA may still be usable)
    pub version_warning: Option<String>,
}

/// Bridge state for rust-analyzer integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaBridgeState {
    /// rust-analyzer not initialized or available
    Unavailable,
    /// rust-analyzer available but version not verified
    Available,
    /// rust-analyzer available and version verified
    Verified,
}

/// Internal LSP client for rust-analyzer communication.
///
/// This client manages a rust-analyzer subprocess and communicates via
/// the Language Server Protocol over stdin/stdout.
struct LspClient {
    /// rust-analyzer child process
    child: Child,
    /// stdin pipe for sending requests
    stdin: ChildStdin,
    /// stdout pipe for receiving responses
    stdout: BufReader<ChildStdout>,
    /// Request ID counter for JSON-RPC correlation
    request_id: AtomicU64,
    /// Track which files have been opened with didOpen notifications
    opened_files: HashSet<PathBuf>,
}

impl LspClient {
    /// Create a new LSP client by spawning rust-analyzer.
    ///
    /// # Arguments
    ///
    /// * `workspace_root` - The workspace root directory for rust-analyzer
    ///
    /// # Errors
    ///
    /// Returns an error if rust-analyzer cannot be spawned or pipes cannot be created.
    fn new(workspace_root: &Path) -> Result<Self, RaBridgeError> {
        let mut child = Command::new("rust-analyzer")
            .current_dir(workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| RaBridgeError::NotFound)?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RaBridgeError::ExecutionFailed("failed to capture stdin".to_string()))?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| {
            RaBridgeError::ExecutionFailed("failed to capture stdout".to_string())
        })?);

        Ok(Self {
            child,
            stdin,
            stdout,
            request_id: AtomicU64::new(1),
            opened_files: HashSet::new(),
        })
    }

    /// Send a JSON-RPC request to rust-analyzer and wait for the response.
    ///
    /// # Arguments
    ///
    /// * `method` - The LSP method name (e.g., "textDocument/hover")
    /// * `params` - The parameters as a JSON value
    ///
    /// # Returns
    ///
    /// The response JSON value matching the request ID
    fn send_request(
        &mut self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, RaBridgeError> {
        let id = self.request_id.fetch_add(1, Ordering::SeqCst);
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        let request_str = serde_json::to_string(&request)
            .map_err(|e| RaBridgeError::ParseError(format!("failed to serialize request: {e}")))?;

        let header = format!("Content-Length: {}\r\n\r\n", request_str.len());

        self.stdin
            .write_all(header.as_bytes())
            .map_err(|e| RaBridgeError::ExecutionFailed(format!("failed to write header: {e}")))?;
        self.stdin
            .write_all(request_str.as_bytes())
            .map_err(|e| RaBridgeError::ExecutionFailed(format!("failed to write request: {e}")))?;
        self.stdin
            .flush()
            .map_err(|e| RaBridgeError::ExecutionFailed(format!("failed to flush: {e}")))?;

        // Read response with ID matching to skip notifications
        self.read_response_for_id(id)
    }

    /// Read response with ID matching to filter out notifications.
    ///
    /// This loops reading messages until we get a response with the matching ID.
    /// Notifications (which have no ID field) are skipped.
    ///
    /// # Arguments
    ///
    /// * `expected_id` - The request ID we're waiting for
    ///
    /// # Returns
    ///
    /// The JSON-RPC response with the matching ID
    fn read_response_for_id(
        &mut self,
        expected_id: u64,
    ) -> Result<serde_json::Value, RaBridgeError> {
        loop {
            let message = self.read_message()?;

            // Check if this is a notification (no id field)
            if message.get("id").is_none() {
                // This is a notification, skip it and read next message
                continue;
            }

            // Check if the response id matches our request
            if let Some(id) = message.get("id").and_then(serde_json::Value::as_u64) {
                if id == expected_id {
                    return Ok(message);
                }
                // Response for a different request - shouldn't happen in single-threaded
                // but skip it anyway
                continue;
            }

            // Invalid response format
            return Err(RaBridgeError::ParseError(
                "response missing valid id".to_string(),
            ));
        }
    }

    /// Read a single JSON-RPC message from rust-analyzer.
    ///
    /// This reads the LSP protocol headers and body for one message,
    /// which could be a response or a notification.
    fn read_message(&mut self) -> Result<serde_json::Value, RaBridgeError> {
        // Read headers
        let mut content_length = 0;
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).map_err(|e| {
                RaBridgeError::ExecutionFailed(format!("failed to read header line: {e}"))
            })?;

            if line == "\r\n" {
                break;
            }
            if let Some(len) = line.strip_prefix("Content-Length: ") {
                content_length = len.trim().parse().map_err(|_| {
                    RaBridgeError::ParseError(format!("invalid content length: {}", len.trim()))
                })?;
            }
        }

        if content_length == 0 {
            return Err(RaBridgeError::ParseError(
                "missing or zero content length".to_string(),
            ));
        }

        // Read body
        let mut body = vec![0u8; content_length];
        self.stdout
            .get_mut()
            .read_exact(&mut body)
            .map_err(|e| RaBridgeError::ExecutionFailed(format!("failed to read body: {e}")))?;

        serde_json::from_slice(&body)
            .map_err(|e| RaBridgeError::ParseError(format!("failed to parse message: {e}")))
    }

    /// Send a JSON-RPC notification to rust-analyzer.
    ///
    /// Notifications do not receive responses and have no ID field.
    ///
    /// # Arguments
    ///
    /// * `method` - The LSP method name (e.g., "textDocument/didOpen")
    /// * `params` - The parameters as a JSON value
    fn send_notification(
        &mut self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<(), RaBridgeError> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });

        let notif_str = serde_json::to_string(&notification).map_err(|e| {
            RaBridgeError::ParseError(format!("failed to serialize notification: {e}"))
        })?;
        let header = format!("Content-Length: {}\r\n\r\n", notif_str.len());

        self.stdin
            .write_all(header.as_bytes())
            .map_err(|e| RaBridgeError::ExecutionFailed(format!("failed to write header: {e}")))?;
        self.stdin.write_all(notif_str.as_bytes()).map_err(|e| {
            RaBridgeError::ExecutionFailed(format!("failed to write notification: {e}"))
        })?;
        self.stdin
            .flush()
            .map_err(|e| RaBridgeError::ExecutionFailed(format!("failed to flush: {e}")))?;

        Ok(())
    }

    /// Notify rust-analyzer that a document has been opened.
    ///
    /// This sends a textDocument/didOpen notification with the file contents.
    /// The file is tracked to avoid sending duplicate didOpen notifications.
    ///
    /// # Arguments
    ///
    /// * `file_path` - Path to the file being opened
    /// * `content` - The file content as a string
    fn send_did_open(&mut self, file_path: &Path, content: &str) -> Result<(), RaBridgeError> {
        let file_path_str = file_path
            .to_str()
            .ok_or_else(|| RaBridgeError::ExecutionFailed("invalid file path".to_string()))?;
        let uri_str = format!("file://{file_path_str}");

        let params = serde_json::json!({
            "textDocument": {
                "uri": uri_str,
                "languageId": "rust",
                "version": 1,
                "text": content
            }
        });

        self.send_notification("textDocument/didOpen", &params)?;
        self.opened_files.insert(file_path.to_path_buf());
        Ok(())
    }

    /// Ensure a file has been opened with didOpen notification.
    ///
    /// If the file hasn't been opened yet, reads it and sends didOpen.
    ///
    /// # Arguments
    ///
    /// * `file_path` - Path to the file
    fn ensure_file_opened(&mut self, file_path: &Path) -> Result<(), RaBridgeError> {
        if self.opened_files.contains(file_path) {
            // Already opened
            return Ok(());
        }

        // Read file content
        let content = std::fs::read_to_string(file_path)
            .map_err(|e| RaBridgeError::ExecutionFailed(format!("failed to read file: {e}")))?;

        self.send_did_open(file_path, &content)
    }

    /// Initialize the rust-analyzer LSP server.
    ///
    /// This sends the initialize request and initialized notification.
    fn initialize(&mut self, workspace_root: &Path) -> Result<(), RaBridgeError> {
        let workspace_path_str = workspace_root
            .to_str()
            .ok_or_else(|| RaBridgeError::ExecutionFailed("invalid workspace path".to_string()))?;
        let root_uri_str = format!("file://{workspace_path_str}");

        // Build InitializeParams manually since we need to serialize to JSON anyway
        let params = serde_json::json!({
            "workspaceFolders": [{
                "uri": root_uri_str,
                "name": "workspace"
            }]
        });

        self.send_request("initialize", &params)?;

        // Send initialized notification
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        });
        let notif_str = serde_json::to_string(&notification).map_err(|e| {
            RaBridgeError::ParseError(format!("failed to serialize notification: {e}"))
        })?;
        let header = format!("Content-Length: {}\r\n\r\n", notif_str.len());
        self.stdin.write_all(header.as_bytes()).ok();
        self.stdin.write_all(notif_str.as_bytes()).ok();
        self.stdin.flush().ok();

        Ok(())
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Attempt to shutdown gracefully
        let shutdown = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 999_999,
            "method": "shutdown",
            "params": null
        });
        if let Ok(shutdown_str) = serde_json::to_string(&shutdown) {
            let header = format!("Content-Length: {}\r\n\r\n", shutdown_str.len());
            let _ = self.stdin.write_all(header.as_bytes());
            let _ = self.stdin.write_all(shutdown_str.as_bytes());
            let _ = self.stdin.flush();
        }

        // Kill the child process
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Bridge to rust-analyzer for enhanced Rust analysis.
///
/// This bridge provides access to rust-analyzer's type inference
/// and semantic analysis capabilities when available.
pub struct RustAnalyzerBridge {
    /// Workspace root for the rust-analyzer instance
    workspace_root: PathBuf,
    /// Current bridge state
    state: RaBridgeState,
    /// Cached version info (if available)
    version_info: Option<RaVersionInfo>,
    /// Whether macro expansion is enabled (security guardrail)
    macro_expansion_enabled: bool,
    /// LSP client (lazily initialized)
    lsp_client: Option<LspClient>,
}

impl RustAnalyzerBridge {
    /// Create a new bridge for the given workspace.
    ///
    /// This does NOT start rust-analyzer - use `initialize()` for that.
    #[must_use]
    pub fn new(workspace_root: PathBuf, macro_expansion_enabled: bool) -> Self {
        Self {
            workspace_root,
            state: RaBridgeState::Unavailable,
            version_info: None,
            macro_expansion_enabled,
            lsp_client: None,
        }
    }

    /// Check if rust-analyzer is available in PATH.
    #[must_use]
    pub fn is_available() -> bool {
        Command::new("rust-analyzer")
            .arg("--version")
            .output()
            .is_ok()
    }

    /// Check availability and attempt to get version in a single subprocess call.
    ///
    /// This is the preferred method for checking rust-analyzer availability as it:
    /// 1. Spawns only ONE subprocess (vs `is_available()` + `initialize()` = 2)
    /// 2. Returns both availability status AND version info
    /// 3. Treats version parse failure as a warning, not an error
    ///
    /// # Returns
    ///
    /// `RaAvailabilityCheck` with:
    /// - `available = false` if spawn failed or exit status was non-zero
    /// - `available = true` if spawn succeeded and exit status was zero
    /// - `version` contains parsed version info if available
    /// - `version_warning` contains any warnings (exit status, parse failure)
    #[must_use]
    pub fn check_availability() -> RaAvailabilityCheck {
        let Ok(output) = Command::new("rust-analyzer").arg("--version").output() else {
            // Spawn failed - RA not found in PATH
            return RaAvailabilityCheck {
                available: false,
                version: None,
                version_warning: None,
            };
        };

        // RA must exit successfully to be considered available
        if !output.status.success() {
            let status = output.status;
            return RaAvailabilityCheck {
                available: false,
                version: None,
                version_warning: Some(format!("rust-analyzer exited with status {status}")),
            };
        }

        let version_string = String::from_utf8_lossy(&output.stdout).trim().to_string();

        match RaVersionInfo::parse(&version_string) {
            Some(version) => RaAvailabilityCheck {
                available: true,
                version: Some(version),
                version_warning: None,
            },
            None => RaAvailabilityCheck {
                available: true, // Still available, just can't parse version
                version: None,
                version_warning: Some(format!(
                    "rust-analyzer version parse failed: '{version_string}'; continuing without version verification"
                )),
            },
        }
    }

    /// Get the current bridge state.
    #[must_use]
    pub fn state(&self) -> RaBridgeState {
        self.state
    }

    /// Get the cached version info.
    #[must_use]
    pub fn version_info(&self) -> Option<&RaVersionInfo> {
        self.version_info.as_ref()
    }

    /// Get the workspace root.
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Initialize the bridge by checking rust-analyzer availability.
    ///
    /// This queries rust-analyzer version and stores it for later verification.
    ///
    /// # Errors
    ///
    /// Returns `RaBridgeError::NotFound` if rust-analyzer is unavailable.
    /// Returns `RaBridgeError::ExecutionFailed` if the version command fails.
    pub fn initialize(&mut self) -> Result<(), RaBridgeError> {
        let output = Command::new("rust-analyzer")
            .arg("--version")
            .output()
            .map_err(|_| RaBridgeError::NotFound)?;

        if !output.status.success() {
            return Err(RaBridgeError::ExecutionFailed(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        let version_string = String::from_utf8_lossy(&output.stdout).trim().to_string();
        self.version_info = RaVersionInfo::parse(&version_string);
        self.state = RaBridgeState::Available;

        Ok(())
    }

    /// Get the current rust-analyzer version info.
    ///
    /// Returns cached info if available, otherwise queries rust-analyzer.
    ///
    /// # Errors
    ///
    /// Returns `RaBridgeError` if rust-analyzer cannot be queried or parsed.
    pub fn get_version_info(&mut self) -> Result<RaVersionInfo, RaBridgeError> {
        if let Some(ref info) = self.version_info {
            return Ok(info.clone());
        }

        self.initialize()?;
        self.version_info.clone().ok_or(RaBridgeError::ParseError(
            "version info not available".to_string(),
        ))
    }

    /// Verify that rust-analyzer version matches the expected version.
    ///
    /// This is used for CI stability to ensure consistent analysis results.
    ///
    /// # Errors
    ///
    /// Returns `RaBridgeError` if the version does not match or cannot be queried.
    pub fn verify_version(&mut self, expected: &RaVersionInfo) -> Result<(), RaBridgeError> {
        let actual = self.get_version_info()?;

        if actual.version_string != expected.version_string {
            return Err(RaBridgeError::VersionMismatch {
                expected: expected.version_string.clone(),
                actual: actual.version_string,
            });
        }

        self.state = RaBridgeState::Verified;
        Ok(())
    }

    /// Ensure LSP client is initialized and return a mutable reference.
    ///
    /// This lazily initializes the LSP client on first use.
    fn ensure_client(&mut self) -> Result<&mut LspClient, RaBridgeError> {
        if self.lsp_client.is_none() {
            let mut client = LspClient::new(&self.workspace_root)?;
            client.initialize(&self.workspace_root)?;
            self.lsp_client = Some(client);
            self.state = RaBridgeState::Available;
        }
        self.lsp_client
            .as_mut()
            .ok_or(RaBridgeError::WorkspaceNotInitialized)
    }

    /// Extract type information from rust-analyzer hover contents.
    ///
    /// rust-analyzer returns markdown like: "```rust\nlet x: Type\n```"
    fn extract_type_from_hover(contents: &serde_json::Value) -> Option<String> {
        // Try to extract from MarkedString or MarkupContent
        if let Some(value) = contents.get("value").and_then(|v| v.as_str()) {
            // Parse out the type from the hover content
            for line in value.lines() {
                if line.contains(':')
                    && !line.starts_with("```")
                    && let Some(type_part) = line.split(':').nth(1)
                {
                    return Some(type_part.trim().to_string());
                }
            }
        } else if let Some(kind) = contents.get("kind") {
            // MarkupContent with kind
            if kind.as_str() == Some("markdown")
                && let Some(value) = contents.get("value").and_then(|v| v.as_str())
            {
                for line in value.lines() {
                    if line.contains(':')
                        && !line.starts_with("```")
                        && let Some(type_part) = line.split(':').nth(1)
                    {
                        return Some(type_part.trim().to_string());
                    }
                }
            }
        } else if let Some(arr) = contents.as_array() {
            // Array of MarkedString
            for item in arr {
                if let Some(value) = item.get("value").and_then(|v| v.as_str()) {
                    for line in value.lines() {
                        if line.contains(':')
                            && !line.starts_with("```")
                            && let Some(type_part) = line.split(':').nth(1)
                        {
                            return Some(type_part.trim().to_string());
                        }
                    }
                }
            }
        }
        None
    }

    /// Infer the type of an expression at a given position.
    ///
    /// This uses rust-analyzer's type inference to determine the type
    /// of a variable or expression, which is critical for trait method binding.
    ///
    /// # Arguments
    ///
    /// * `file_path` - Path to the Rust source file
    /// * `line` - Line number (0-indexed)
    /// * `column` - Column number (0-indexed)
    /// * `confidence` - Confidence tracker for recording limitations
    ///
    /// # Returns
    ///
    /// The inferred type name, or None if type cannot be inferred.
    ///
    /// # Errors
    ///
    /// Returns `RaBridgeError` if rust-analyzer queries fail.
    pub fn infer_type_at_position(
        &mut self,
        file_path: &Path,
        line: usize,
        column: usize,
        confidence: &mut ConfidenceTracker,
    ) -> Result<Option<String>, RaBridgeError> {
        if self.state == RaBridgeState::Unavailable {
            confidence.add_limitation("rust-analyzer not available for type inference");
            confidence.add_unavailable_feature("type_inference");
            return Ok(None);
        }

        // Ensure LSP client is initialized
        let client = self.ensure_client()?;

        // Ensure the file has been opened with didOpen
        client.ensure_file_opened(file_path)?;

        let file_path_str = file_path
            .to_str()
            .ok_or_else(|| RaBridgeError::ExecutionFailed("invalid file path".to_string()))?;
        let uri_str = format!("file://{file_path_str}");

        let params = serde_json::json!({
            "textDocument": { "uri": uri_str },
            "position": { "line": line, "character": column }
        });

        let response = client.send_request("textDocument/hover", &params)?;

        // Parse hover result for type information
        if let Some(result) = response.get("result")
            && !result.is_null()
            && let Some(contents) = result.get("contents")
            && let Some(type_str) = Self::extract_type_from_hover(contents)
        {
            return Ok(Some(type_str));
        }

        confidence.add_limitation("Could not infer type at position");
        Ok(None)
    }

    /// Resolve elided lifetimes in a function signature.
    ///
    /// Rust's lifetime elision rules allow omitting explicit lifetimes
    /// in many cases. This function uses rust-analyzer to determine
    /// the actual lifetime relationships.
    ///
    /// # Arguments
    ///
    /// * `file_path` - Path to the Rust source file
    /// * `function_name` - Name of the function to analyze
    /// * `confidence` - Confidence tracker for recording limitations
    ///
    /// # Returns
    ///
    /// A list of resolved lifetime constraints.
    ///
    /// # Errors
    ///
    /// Returns `RaBridgeError` if rust-analyzer queries fail.
    ///
    /// # Note
    ///
    /// This feature requires direct access to rust-analyzer's HIR (High-level IR),
    /// which is not exposed via the standard LSP protocol. This is a placeholder
    /// for potential future implementation using rust-analyzer's internal APIs.
    pub fn resolve_elided_lifetimes(
        &mut self,
        _file_path: &Path,
        _function_name: &str,
        confidence: &mut ConfidenceTracker,
    ) -> Result<Vec<ResolvedLifetime>, RaBridgeError> {
        if self.state == RaBridgeState::Unavailable {
            confidence.add_limitation("rust-analyzer not available for elided lifetime resolution");
            confidence.add_unavailable_feature("elided_lifetimes");
            return Ok(Vec::new());
        }

        // Note: Elided lifetime resolution requires access to rust-analyzer's HIR,
        // which is not exposed via standard LSP. This would require either:
        // 1. Using rust-analyzer as a library (heavy dependency)
        // 2. Adding a custom LSP extension to rust-analyzer
        // 3. Implementing basic lifetime elision rules ourselves (limited accuracy)
        //
        // For now, we document this limitation and return an empty result.
        confidence
            .add_limitation("Elided lifetime resolution requires HIR access not available via LSP");
        confidence.add_unavailable_feature("elided_lifetimes");
        Ok(Vec::new())
    }

    /// Expand a macro at a given position.
    ///
    /// # Security
    ///
    /// This function requires `macro_expansion_enabled` to be true.
    /// Macro expansion can execute arbitrary code (proc macros, build scripts).
    ///
    /// # Errors
    ///
    /// Returns `RaBridgeError` if rust-analyzer queries fail or expansion is disabled.
    pub fn expand_macro(
        &mut self,
        file_path: &Path,
        line: usize,
        column: usize,
        confidence: &mut ConfidenceTracker,
    ) -> Result<Option<String>, RaBridgeError> {
        if !self.macro_expansion_enabled {
            confidence.add_limitation("Macro expansion disabled for security");
            return Err(RaBridgeError::NotSupported(
                "macro expansion is disabled - use --enable-macro-expansion to enable".to_string(),
            ));
        }

        if self.state == RaBridgeState::Unavailable {
            confidence.add_limitation("rust-analyzer not available for macro expansion");
            confidence.add_unavailable_feature("macro_expansion");
            return Ok(None);
        }

        let client = self.ensure_client()?;

        // Ensure the file has been opened with didOpen
        client.ensure_file_opened(file_path)?;

        let file_path_str = file_path
            .to_str()
            .ok_or_else(|| RaBridgeError::ExecutionFailed("invalid file path".to_string()))?;
        let uri_str = format!("file://{file_path_str}");

        // rust-analyzer specific request: rust-analyzer/expandMacro
        let params = serde_json::json!({
            "textDocument": { "uri": uri_str },
            "position": { "line": line, "character": column }
        });

        let response = client.send_request("rust-analyzer/expandMacro", &params)?;

        if let Some(result) = response.get("result")
            && !result.is_null()
            && let Some(expansion) = result.get("expansion").and_then(|e| e.as_str())
        {
            return Ok(Some(expansion.to_string()));
        }

        confidence.add_limitation("Macro expansion returned no result");
        Ok(None)
    }
}

/// A resolved lifetime constraint from rust-analyzer.
#[derive(Debug, Clone)]
pub struct ResolvedLifetime {
    /// The lifetime name (e.g., "'a", "'b", "'static")
    pub name: String,
    /// The source of the lifetime (parameter, return, where clause)
    pub source: LifetimeSource,
    /// Related lifetimes (e.g., for outlives constraints)
    pub related_to: Vec<String>,
}

/// Source of a lifetime in a function signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifetimeSource {
    /// Lifetime from a function parameter
    Parameter,
    /// Lifetime from the return type
    Return,
    /// Lifetime from a where clause
    WhereClause,
    /// Lifetime from a type parameter bound
    TypeBound,
    /// Elided lifetime (inferred by rust-analyzer)
    Elided,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ra_version_info_parse() {
        let version = RaVersionInfo::parse("rust-analyzer 2024-01-15 7e74ef8");
        assert!(version.is_some());

        let info = version.unwrap();
        assert_eq!(info.date, "2024-01-15");
        assert_eq!(info.commit_hash, "7e74ef8");
    }

    #[test]
    fn test_ra_version_info_parse_invalid() {
        assert!(RaVersionInfo::parse("invalid").is_none());
        assert!(RaVersionInfo::parse("").is_none());
        assert!(RaVersionInfo::parse("rust-analyzer").is_none());
    }

    #[test]
    fn test_ra_version_info_matches() {
        let v1 = RaVersionInfo {
            version_string: "rust-analyzer 2024-01-15 7e74ef8".to_string(),
            date: "2024-01-15".to_string(),
            commit_hash: "7e74ef8".to_string(),
        };
        let v2 = v1.clone();
        let v3 = RaVersionInfo {
            version_string: "rust-analyzer 2024-01-16 abc1234".to_string(),
            date: "2024-01-16".to_string(),
            commit_hash: "abc1234".to_string(),
        };

        assert!(v1.matches(&v2));
        assert!(!v1.matches(&v3));
    }

    #[test]
    fn test_ra_bridge_new() {
        let bridge = RustAnalyzerBridge::new(PathBuf::from("/tmp"), false);
        assert_eq!(bridge.state(), RaBridgeState::Unavailable);
        assert!(bridge.version_info().is_none());
        assert_eq!(bridge.workspace_root(), Path::new("/tmp"));
    }

    #[test]
    fn test_ra_bridge_state() {
        let bridge = RustAnalyzerBridge::new(PathBuf::from("/tmp"), false);
        assert_eq!(bridge.state(), RaBridgeState::Unavailable);
    }

    #[test]
    fn test_ra_bridge_error_display() {
        let err = RaBridgeError::NotFound;
        assert!(err.to_string().contains("not found"));

        let err = RaBridgeError::VersionMismatch {
            expected: "v1".to_string(),
            actual: "v2".to_string(),
        };
        assert!(err.to_string().contains("mismatch"));

        let err = RaBridgeError::NotSupported("test".to_string());
        assert!(err.to_string().contains("not supported"));
    }

    #[test]
    fn test_resolved_lifetime() {
        let lifetime = ResolvedLifetime {
            name: "'a".to_string(),
            source: LifetimeSource::Parameter,
            related_to: vec!["'b".to_string()],
        };
        assert_eq!(lifetime.name, "'a");
        assert_eq!(lifetime.source, LifetimeSource::Parameter);
        assert_eq!(lifetime.related_to.len(), 1);
    }

    #[test]
    fn test_lifetime_source() {
        assert_eq!(LifetimeSource::Parameter, LifetimeSource::Parameter);
        assert_ne!(LifetimeSource::Parameter, LifetimeSource::Return);
    }

    #[test]
    fn test_expand_macro_requires_enabled() {
        let mut bridge = RustAnalyzerBridge::new(PathBuf::from("/tmp"), false);
        let mut confidence = ConfidenceTracker::default();

        let result = bridge.expand_macro(Path::new("test.rs"), 0, 0, &mut confidence);
        assert!(result.is_err());
        assert!(confidence.has_limitation("Macro expansion disabled for security"));
    }

    #[test]
    fn test_infer_type_unavailable() {
        let mut bridge = RustAnalyzerBridge::new(PathBuf::from("/tmp"), false);
        let mut confidence = ConfidenceTracker::default();

        let result = bridge.infer_type_at_position(Path::new("test.rs"), 0, 0, &mut confidence);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
        assert!(confidence.has_limitation("rust-analyzer not available for type inference"));
    }
}
