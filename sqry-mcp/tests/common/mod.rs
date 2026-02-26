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
pub struct McpTestClient {
    child: Child,
    pub stdin: ChildStdin,
    pub stdout: BufReader<ChildStdout>,
    pub stderr: Option<BufReader<ChildStderr>>,
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
        let mut command = if let Some(binary) = find_sqry_mcp_binary() {
            Command::new(binary)
        } else {
            let mut cmd = Command::new("cargo");
            cmd.args(["run", "-p", "sqry-mcp", "--quiet"]);
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
        })
    }

    /// Spawn a new MCP server process and complete the initialize handshake.
    #[allow(dead_code)]
    pub fn new_initialized() -> Result<Self> {
        let mut client = Self::new()?;
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
    pub fn new_with_env_and_stderr_mode(
        envs: &[(String, String)],
        stderr_mode: StderrMode,
    ) -> Result<Self> {
        // Use the pre-built binary directly instead of cargo run.
        // This avoids cargo lock contention when many tests spawn concurrently.
        let (program, args): (std::ffi::OsString, &[&str]) =
            if let Some(binary) = find_sqry_mcp_binary() {
                (binary.into(), &[])
            } else {
                ("cargo".into(), &["run", "-p", "sqry-mcp", "--quiet"])
            };

        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());

        // Set workspace root unless the caller explicitly overrides it
        let has_workspace_override = envs
            .iter()
            .any(|(k, _)| k == "SQRY_MCP_WORKSPACE_ROOT" || k == "SQRY_WORKSPACE_ROOT");
        if !has_workspace_override {
            command.env("SQRY_MCP_WORKSPACE_ROOT", workspace_root());
        }

        // Set generous timeout unless the caller explicitly overrides it
        let has_timeout_override = envs.iter().any(|(k, _)| k == "SQRY_MCP_TIMEOUT_MS");
        if !has_timeout_override {
            command.env("SQRY_MCP_TIMEOUT_MS", "600000"); // 10 min for e2e tests
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
        })
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

    /// Read a single JSON-RPC response from stdout with a 30-second timeout.
    ///
    /// The timeout prevents tests from hanging indefinitely when the MCP server
    /// blocks (e.g., building an index for a workspace without a pre-built graph).
    pub fn read_response(&mut self) -> Result<Value> {
        use std::os::unix::io::AsRawFd;

        // Set a read timeout on the raw fd so read_line won't block forever.
        // We use poll(2) to wait for data with a timeout.
        let fd = self.stdout.get_ref().as_raw_fd();
        let timeout_ms: i32 = 30_000; // 30 seconds
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };

        let ret = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
        if ret == 0 {
            // Timeout — kill the child to unblock
            let _ = self.child.kill();
            return Err(anyhow::anyhow!(
                "read_response timed out after 30 seconds — MCP server may be stuck"
            ));
        } else if ret < 0 {
            return Err(anyhow::anyhow!(
                "poll() failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        let mut line = String::new();
        self.stdout.read_line(&mut line)?;
        Ok(serde_json::from_str(&line)?)
    }

    /// Send a request and wait for response (common pattern)
    pub fn call(&mut self, method: &str, params: Value, id: i64) -> Result<Value> {
        self.send_request(method, params, id)?;
        self.read_response()
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

/// Ensure the unified graph snapshot exists for MCP tests.
///
/// Builds the graph for the provided workspace if needed.
#[allow(dead_code)]
pub fn ensure_graph_snapshot(root: &std::path::Path) -> Result<()> {
    use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
    use sqry_core::graph::unified::persistence::{GraphStorage, save_to_path};
    use sqry_plugin_registry::create_plugin_manager;

    let lock = GRAPH_BUILD_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock.lock().expect("graph build lock");

    let storage = GraphStorage::new(root);
    if storage.snapshot_exists() {
        return Ok(());
    }

    let plugins = create_plugin_manager();
    let config = BuildConfig::default();
    let graph = build_unified_graph(root, &plugins, &config)?;

    std::fs::create_dir_all(storage.graph_dir())?;
    save_to_path(&graph, storage.snapshot_path())?;

    Ok(())
}

fn default_initialize_params() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": { "name": "sqry-mcp-tests", "version": "0.0" }
    })
}
