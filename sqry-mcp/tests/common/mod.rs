pub mod daemon_fixture;
#[allow(unused_imports)]
pub use daemon_fixture::DaemonFixture;

use anyhow::Result;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, OnceLock};

/// Stderr handling mode for MCP test client
#[allow(dead_code)]
pub enum StderrMode {
    /// Discard stderr
    Null,
    /// Inherit stderr (print to parent's stderr)
    Inherit,
    /// Capture stderr for later reading (warning: may block)
    Capture,
}

/// Test harness for MCP stdio protocol
///
/// Spawns an MCP server process and provides methods for sending
/// JSON-RPC requests and reading responses.
/// How a spawned server redacts its responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Redaction {
    /// `SQRY_REDACTION_PRESET=none` unless the caller sets it: the tests see
    /// absolute paths.
    NoneForTests,
    /// No redaction variable at all: the server's default preset.
    ServerDefault,
}

/// Every variable `sqry-mcp-redaction` reads from the environment.
const REDACTION_ENV_VARS: &[&str] = &[
    "SQRY_REDACTION_PRESET",
    "SQRY_HASH_FILENAMES",
    "SQRY_HASH_SALT",
    "SQRY_PRESERVE_PATHS",
    "SQRY_REDACT_CODE",
    "SQRY_REDACT_DOCS",
    "SQRY_REDACTION_MAX_DEPTH",
    "SQRY_REDACT_PATHS",
    "SQRY_REDACT_PATTERNS",
    "SQRY_REDACT_URIS",
    "SQRY_REDACT_WORKSPACE",
    "SQRY_REVEAL_PATHS",
    "SQRY_WHITELIST_FIELDS",
];

pub struct McpTestClient {
    child: Child,
    pub stdin: ChildStdin,
    pub stdout: BufReader<ChildStdout>,
    pub stderr: Option<BufReader<ChildStderr>>,
    /// Client-side read timeout in milliseconds.
    read_timeout_ms: i32,
    roots: Vec<Value>,
    /// When set, the client answers the server's `roots/list` with this
    /// JSON-RPC error instead of its roots.
    roots_list_error: Option<Value>,
}

static GRAPH_BUILD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Return the Cargo workspace root directory.
///
/// Uses `CARGO_MANIFEST_DIR` (set by Cargo for tests) and walks up to the
/// workspace root. This ensures the MCP server discovers the top-level
/// `.sqry/graph/` index instead of any per-crate `.sqry/` directory.
fn workspace_root() -> std::path::PathBuf {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo");
    std::path::Path::new(&manifest_dir)
        .parent()
        .expect("sqry-mcp must have a parent directory")
        .to_path_buf()
}

/// Locate the pre-built `sqry-mcp` binary next to the test executable.
///
/// Test binaries live in `target/debug/deps/`, while the `sqry-mcp` binary
/// is at `target/debug/sqry-mcp`. We check both the parent directory
/// (`deps/`) and its parent (`debug/`) to find the binary.
fn find_sqry_mcp_binary() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("SQRY_E2E_SQRY_MCP_BIN") {
        let path = std::path::PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    let binary_name = format!("sqry-mcp{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().ok()?;
    // Check parent (target/debug/deps/)
    let parent = exe.parent()?;
    let candidate = parent.join(&binary_name);
    if candidate.is_file() {
        return Some(candidate);
    }
    // Check grandparent (target/debug/)
    let grandparent = parent.parent()?;
    let candidate = grandparent.join(&binary_name);
    if candidate.is_file() {
        return Some(candidate);
    }
    None
}

impl McpTestClient {
    /// Spawn a new MCP server process for testing.
    ///
    /// Sets `SQRY_MCP_WORKSPACE_ROOT` to the Cargo workspace root so the
    /// server discovers the correct `.sqry/graph/` index regardless of which
    /// directory `cargo test` runs from.
    pub fn new() -> Result<Self> {
        // Use the pre-built binary directly to avoid cargo lock contention
        // when many tests spawn server processes concurrently.
        // Test binaries live in target/debug/deps/, but the sqry-mcp binary
        // is at target/debug/sqry-mcp, so we check both parent and grandparent.
        //
        // Pass `--no-daemon` so a running sqryd on the host does not
        // hijack standalone-mode tests; see the analogous block in
        // `new_with_env_and_stderr_mode_internal`.
        let mut command = if let Some(binary) = find_sqry_mcp_binary() {
            let mut cmd = Command::new(binary);
            cmd.arg("--no-daemon");
            cmd
        } else {
            let mut cmd = Command::new("cargo");
            cmd.args(["run", "-p", "sqry-mcp", "--quiet", "--", "--no-daemon"]);
            cmd
        };
        // Use the mini-workspace fixture which has a pre-built graph index,
        // so semantic_search doesn't block trying to build one from scratch.
        let fixture_workspace = workspace_root().join("sqry-lsp/tests/fixtures/mini-workspace");
        let ws_root = if fixture_workspace.exists() {
            fixture_workspace
        } else {
            workspace_root()
        };
        command
            .env("SQRY_MCP_WORKSPACE_ROOT", ws_root)
            .env("SQRY_MCP_TIMEOUT_MS", "30000") // 30s timeout for protocol tests
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn()?;

        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());

        Ok(Self {
            child,
            stdin,
            stdout,
            stderr: None,
            read_timeout_ms: 30_000,
            roots: Vec::new(),
            roots_list_error: None,
        })
    }

    /// Spawn a new MCP server process pointing at the real workspace root.
    ///
    /// Unlike [`new`], this does **not** redirect to the mini-workspace fixture.
    /// Use this for end-to-end tests that query the full codebase graph.
    ///
    /// [`new`]: Self::new
    #[allow(dead_code)]
    pub fn new_for_workspace() -> Result<Self> {
        let mut client = Self::new_with_env_and_stderr_mode(
            &[(
                "SQRY_MCP_WORKSPACE_ROOT".to_string(),
                workspace_root().to_string_lossy().into_owned(),
            )],
            StderrMode::Null,
        )?;
        // The full workspace graph (~244 MB) takes much longer to load
        // than the mini-workspace fixture on first tool call.
        client.read_timeout_ms = 120_000;
        Ok(client)
    }

    /// Spawn a new MCP server process and complete the initialize handshake.
    #[allow(dead_code)]
    pub fn new_initialized() -> Result<Self> {
        let mut client = Self::new()?;
        let _ = client.initialize()?;
        Ok(client)
    }

    /// Spawn a new MCP server for the real workspace and complete the initialize handshake.
    #[allow(dead_code)]
    pub fn new_for_workspace_initialized() -> Result<Self> {
        let mut client = Self::new_for_workspace()?;
        let _ = client.initialize()?;
        Ok(client)
    }

    /// Send MCP initialize request and return the response.
    #[allow(dead_code)]
    pub fn initialize(&mut self) -> Result<Value> {
        let response = self.call("initialize", default_initialize_params(), 0)?;
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        writeln!(self.stdin, "{notification}")?;
        self.stdin.flush()?;
        Ok(response)
    }

    /// Spawn a new MCP server process with additional environment variables
    #[allow(dead_code)]
    pub fn new_with_env(envs: &[(String, String)]) -> Result<Self> {
        // Always inherit stderr for debugging during tests
        Self::new_with_env_and_stderr_mode(envs, StderrMode::Inherit)
    }

    /// Spawn a new MCP server process with additional environment variables
    /// and complete the initialize handshake.
    #[allow(dead_code)]
    pub fn new_with_env_initialized(envs: &[(String, String)]) -> Result<Self> {
        let mut client = Self::new_with_env(envs)?;
        let _ = client.initialize()?;
        Ok(client)
    }

    /// Spawn a new MCP server process with additional environment variables
    /// and optionally capture stderr for debugging.
    #[allow(dead_code)]
    pub fn new_with_env_and_stderr(
        envs: &[(String, String)],
        capture_stderr: bool,
    ) -> Result<Self> {
        let mode = if capture_stderr {
            StderrMode::Capture
        } else {
            StderrMode::Null
        };
        Self::new_with_env_and_stderr_mode(envs, mode)
    }

    /// Spawn a new MCP server process with additional environment variables
    /// and configurable stderr handling.
    #[allow(dead_code)]
    #[allow(clippy::needless_continue)] // Continue clarifies control flow
    #[allow(clippy::needless_pass_by_value)] // Convenience for callers
    pub fn new_with_env_and_stderr_mode(
        envs: &[(String, String)],
        #[allow(clippy::needless_pass_by_value)] // Test helper takes owned value for convenience
        stderr_mode: StderrMode,
    ) -> Result<Self> {
        Self::new_with_env_and_stderr_mode_internal(
            envs,
            stderr_mode,
            true,
            Redaction::NoneForTests,
        )
    }

    /// Spawn a new MCP server process that redacts with the server's own
    /// default preset: `SQRY_REDACTION_PRESET` and every fine-grained
    /// redaction variable are removed from the child's environment, so the
    /// preset is the one an operator who configured nothing gets
    /// (`minimal`). Every other constructor sets the preset to `none`.
    #[allow(dead_code)]
    pub fn new_with_default_redaction(
        envs: &[(String, String)],
        stderr_mode: StderrMode,
    ) -> Result<Self> {
        Self::new_with_env_and_stderr_mode_internal(
            envs,
            stderr_mode,
            true,
            Redaction::ServerDefault,
        )
    }

    /// Spawn a new MCP server process without injecting the default workspace env var.
    #[allow(dead_code)]
    pub fn new_without_workspace_env_and_stderr_mode(
        envs: &[(String, String)],
        stderr_mode: StderrMode,
    ) -> Result<Self> {
        Self::new_with_env_and_stderr_mode_internal(
            envs,
            stderr_mode,
            false,
            Redaction::NoneForTests,
        )
    }

    /// Spawn a server without the default workspace env var, running in
    /// `dir`, so a test controls what the server's working directory holds
    /// (and what is at or above it).
    #[allow(dead_code)]
    pub fn new_without_workspace_env_in_dir(
        dir: &std::path::Path,
        stderr_mode: StderrMode,
    ) -> Result<Self> {
        Self::spawn(&[], stderr_mode, false, Redaction::NoneForTests, Some(dir))
    }

    fn new_with_env_and_stderr_mode_internal(
        envs: &[(String, String)],
        stderr_mode: StderrMode,
        inject_default_workspace_root: bool,
        redaction: Redaction,
    ) -> Result<Self> {
        Self::spawn(
            envs,
            stderr_mode,
            inject_default_workspace_root,
            redaction,
            None,
        )
    }

    fn spawn(
        envs: &[(String, String)],
        stderr_mode: StderrMode,
        inject_default_workspace_root: bool,
        redaction: Redaction,
        dir: Option<&std::path::Path>,
    ) -> Result<Self> {
        // Use the pre-built binary directly instead of cargo run.
        // This avoids cargo lock contention when many tests spawn concurrently.
        //
        // Force `--no-daemon` so these standalone-mode tests don't silently
        // route through a running sqryd (whose `DAEMON_SUPPORTED_TOOL_NAMES`
        // is a 15-tool subset; tools like `list_files`, `get_index_status`,
        // `get_references` are not in it and would be rejected with
        // "unknown tool name"). Daemon-mode integration tests use
        // `DaemonFixture` in `sqry-mcp/tests/common/daemon_fixture.rs`.
        let (program, args): (std::ffi::OsString, &[&str]) =
            if let Some(binary) = find_sqry_mcp_binary() {
                (binary.into(), &["--no-daemon"])
            } else {
                (
                    "cargo".into(),
                    &["run", "-p", "sqry-mcp", "--quiet", "--", "--no-daemon"],
                )
            };

        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if let Some(dir) = dir {
            command.current_dir(dir);
        }

        // Set workspace root unless the caller explicitly overrides it
        let has_workspace_override = envs
            .iter()
            .any(|(k, _)| k == "SQRY_MCP_WORKSPACE_ROOT" || k == "SQRY_WORKSPACE_ROOT");
        if inject_default_workspace_root && !has_workspace_override {
            command.env("SQRY_MCP_WORKSPACE_ROOT", workspace_root());
        }

        // Set generous timeout unless the caller explicitly overrides it
        let has_timeout_override = envs.iter().any(|(k, _)| k == "SQRY_MCP_TIMEOUT_MS");
        if !has_timeout_override {
            command.env("SQRY_MCP_TIMEOUT_MS", "600000"); // 10 min for e2e tests
        }

        match redaction {
            Redaction::NoneForTests => {
                let has_redaction_override = envs.iter().any(|(k, _)| k == "SQRY_REDACTION_PRESET");
                if !has_redaction_override {
                    command.env("SQRY_REDACTION_PRESET", "none");
                }
            }
            Redaction::ServerDefault => {
                for key in REDACTION_ENV_VARS {
                    command.env_remove(key);
                }
            }
        }

        let capture_stderr = match stderr_mode {
            StderrMode::Null => {
                command.stderr(Stdio::null());
                false
            }
            StderrMode::Inherit => {
                command.stderr(Stdio::inherit());
                false
            }
            StderrMode::Capture => {
                command.stderr(Stdio::piped());
                true
            }
        };

        for (key, value) in envs {
            command.env(key, value);
        }

        let mut child = command.spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let stderr = if capture_stderr {
            child.stderr.take().map(BufReader::new)
        } else {
            None
        };

        Ok(Self {
            child,
            stdin,
            stdout,
            stderr,
            read_timeout_ms: 30_000,
            roots: Vec::new(),
            roots_list_error: None,
        })
    }

    #[allow(dead_code)]
    pub fn set_roots(&mut self, root_paths: &[std::path::PathBuf]) {
        self.roots = root_paths.iter().map(|path| root_entry(path)).collect();
    }

    /// Answer every later `roots/list` with these URIs exactly as given,
    /// so a test can list a root that is not a canonical directory URI (an
    /// unparsable URI, a missing directory, a file, another scheme).
    #[allow(dead_code)]
    pub fn set_root_uris(&mut self, uris: &[&str]) {
        self.roots = uris
            .iter()
            .map(|uri| json!({ "uri": uri, "name": "root" }))
            .collect();
    }

    /// Answer every later `roots/list` from the server with the JSON-RPC
    /// error `error` (`{"code": ..., "message": ...}`), as a client whose
    /// roots cannot be read does.
    #[allow(dead_code)]
    pub fn fail_roots_list(&mut self, error: Value) {
        self.roots_list_error = Some(error);
    }

    #[allow(dead_code)]
    pub fn initialize_with_capabilities(&mut self, capabilities: Value) -> Result<Value> {
        let response = self.call(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": capabilities,
                "clientInfo": { "name": "sqry-mcp-tests", "version": "0.0" }
            }),
            0,
        )?;
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        writeln!(self.stdin, "{notification}")?;
        self.stdin.flush()?;
        Ok(response)
    }

    #[allow(dead_code)]
    pub fn initialize_with_roots(&mut self, root_paths: &[std::path::PathBuf]) -> Result<Value> {
        self.set_roots(root_paths);
        self.initialize_with_capabilities(json!({
            "roots": {
                "listChanged": true
            }
        }))
    }

    #[allow(dead_code)]
    pub fn notify_roots_list_changed(&mut self) -> Result<()> {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/roots/list_changed",
            "params": {}
        });
        writeln!(self.stdin, "{notification}")?;
        self.stdin.flush()?;
        Ok(())
    }

    /// Read all available stderr output (for debugging).
    /// Returns an empty string if stderr wasn't captured.
    #[allow(dead_code)]
    pub fn read_stderr(&mut self) -> String {
        use std::io::Read;
        if let Some(ref mut stderr) = self.stderr {
            let mut buf = String::new();
            // Use non-blocking read by setting a small timeout
            // This is a best-effort read of available data
            let _ = stderr.read_to_string(&mut buf);
            buf
        } else {
            String::new()
        }
    }

    /// Send a JSON-RPC request without waiting for response
    #[allow(clippy::needless_pass_by_value)] // Convenience for callers
    pub fn send_request(&mut self, method: &str, params: Value, id: i64) -> Result<()> {
        let request = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id
        });
        writeln!(self.stdin, "{request}")?;
        self.stdin.flush()?;
        Ok(())
    }

    /// Read a single JSON-RPC response from stdout with a timeout.
    ///
    /// The timeout prevents tests from hanging indefinitely when the MCP server
    /// blocks (e.g., building an index for a workspace without a pre-built graph).
    /// Default is 30 s; workspace clients use 120 s for the initial graph load.
    pub fn read_response(&mut self) -> Result<Value> {
        loop {
            self.read_response_with_timeout()?;
            let mut line = String::new();
            self.stdout.read_line(&mut line)?;
            let message: Value = serde_json::from_str(&line)?;
            if self.handle_server_request(&message)? {
                continue;
            }
            return Ok(message);
        }
    }

    /// Platform-specific timeout: poll(2) on Unix, no-op on Windows.
    #[cfg(unix)]
    fn read_response_with_timeout(&mut self) -> Result<()> {
        use std::os::unix::io::AsRawFd;

        let fd = self.stdout.get_ref().as_raw_fd();
        let timeout_ms: i32 = self.read_timeout_ms;
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };

        let ret = unsafe { libc::poll(&raw mut pollfd, 1, timeout_ms) };
        if ret == 0 {
            let _ = self.child.kill();
            return Err(anyhow::anyhow!(
                "read_response timed out after {} seconds — MCP server may be stuck",
                timeout_ms / 1000
            ));
        } else if ret < 0 {
            return Err(anyhow::anyhow!(
                "poll() failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    /// On Windows, skip the poll-based timeout and rely on the child process
    /// completing. Tests on Windows CI use shorter workspaces to avoid hangs.
    #[cfg(not(unix))]
    fn read_response_with_timeout(&mut self) -> Result<()> {
        // No poll(2) on Windows — proceed directly to blocking read.
        // The test suite avoids workspaces that trigger long-running index builds.
        Ok(())
    }

    /// Send a request and wait for response (common pattern)
    pub fn call(&mut self, method: &str, params: Value, id: i64) -> Result<Value> {
        self.send_request(method, params, id)?;
        self.read_response()
    }

    fn handle_server_request(&mut self, message: &Value) -> Result<bool> {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Ok(false);
        };

        if method == "roots/list" {
            let request_id = message["id"].clone();
            let response = match &self.roots_list_error {
                Some(error) => json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": error,
                }),
                None => json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "roots": self.roots
                    }
                }),
            };
            // rmcp 1.7 registers stdio peer requests asynchronously; a short
            // yield keeps the manual test responder from racing that setup.
            std::thread::sleep(std::time::Duration::from_millis(5));
            writeln!(self.stdin, "{response}")?;
            self.stdin.flush()?;
            return Ok(true);
        }

        Ok(false)
    }
}

impl Drop for McpTestClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Helper to unwrap MCP content format from tool call responses.
///
/// MCP tool call responses wrap the actual result in a content array:
/// ```json
/// {
///   "result": {
///     "content": [{ "type": "text", "text": "{...actual JSON...}" }]
///   }
/// }
/// ```
///
/// This function extracts and parses the inner JSON from the content wrapper.
#[allow(dead_code)]
pub fn unwrap_mcp_content(response: &Value) -> Result<Value> {
    let result = response["result"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Missing result object"))?;

    if !result.contains_key("content") {
        return Ok(response["result"].clone());
    }

    let content = result["content"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Missing content array"))?;

    let first_content = content
        .first()
        .ok_or_else(|| anyhow::anyhow!("Empty content array"))?;

    let text = first_content["text"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing text in content"))?;

    let inner: Value = serde_json::from_str(text)?;
    Ok(inner)
}

/// Ensure a complete index (manifest and snapshot) exists for MCP tests.
///
/// Builds and persists the index for the provided workspace when either
/// file is missing, through the registry helper every persisting surface
/// uses. A snapshot alone is not an index: the MCP's graph acquisition
/// requires the manifest, so a helper that wrote only `snapshot.sqry` made
/// every test that needs the manifest pass or fail by which test binary
/// happened to index the fixture first.
#[allow(dead_code)]
pub fn ensure_graph_snapshot(root: &std::path::Path) -> Result<()> {
    use sqry_core::graph::unified::build::{BuildConfig, MacroOptionsRequest};
    use sqry_core::graph::unified::persistence::GraphStorage;
    use sqry_plugin_registry::{PluginSelectionConfig, UnreadableManifestPolicy};

    let lock = GRAPH_BUILD_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock.lock().expect("graph build lock");

    let storage = GraphStorage::new(root);
    if storage.exists() && storage.snapshot_exists() {
        return Ok(());
    }

    sqry_plugin_registry::build_and_persist_with_workspace_roster(
        root,
        &PluginSelectionConfig::default(),
        UnreadableManifestPolicy::FallBack,
        "test:ensure_graph_snapshot",
        &BuildConfig::default(),
        &MacroOptionsRequest::empty(),
        sqry_core::progress::no_op_reporter(),
    )?;

    Ok(())
}

fn default_initialize_params() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": { "name": "sqry-mcp-tests", "version": "0.0" }
    })
}

fn root_entry(path: &std::path::Path) -> Value {
    let canonical = path.canonicalize().expect("canonical root path");
    let uri = url::Url::from_file_path(&canonical)
        .expect("file URI")
        .to_string();
    let name = canonical
        .file_name()
        .map(|value| value.to_string_lossy().into_owned());

    json!({
        "uri": uri,
        "name": name
    })
}
