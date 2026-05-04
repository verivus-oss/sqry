pub mod daemon_fixture;
#[allow(unused_imports)]
pub use daemon_fixture::DaemonFixture;

use anyhow::Result;
use sqry_lsp::config::DocumentLimits;
use sqry_lsp::session::SessionManager;
use sqry_lsp::{LspOptions, SqryLanguageServer, build_test_service, handlers};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tower::Service;
use tower::ServiceExt;
use tower_lsp::LspService;
use tower_lsp::jsonrpc::{Request, Response};

/// Returns permissive document limits for testing (accepts very large files)
#[allow(dead_code)]
pub fn test_limits() -> DocumentLimits {
    DocumentLimits {
        source_max_bytes: usize::MAX,
        data_max_bytes: usize::MAX,
    }
}

static INDEX_BUILD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[allow(dead_code)]
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

#[allow(dead_code)]
pub fn fixture_path(relative: &str) -> PathBuf {
    let path = workspace_root().join(relative);
    // Canonicalize so Windows tests get \\?\ prefix matching handler output
    std::fs::canonicalize(&path).unwrap_or(path)
}

pub fn options_for(root: &Path) -> LspOptions {
    LspOptions {
        stdio: false,
        socket: None,
        index_root: Some(root.to_path_buf()),
        log_level: "warn".into(),
        config: None,
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
        workspace: None,
    }
}

pub fn ensure_index(root: &Path) -> Result<()> {
    let lock = INDEX_BUILD_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock.lock().expect("index build lock");
    let session = SessionManager::new(options_for(root));
    let reporter = sqry_core::progress::no_op_reporter();
    // Use force=true in tests to bypass any stale locks from previous test runs
    handlers::index::rebuild_index(&session, root, &reporter, true)?;

    // Also build CodeGraph for handlers that use execute_on_graph()
    build_code_graph(root)?;

    Ok(())
}

/// Build `CodeGraph` for handlers that use `execute_on_graph()`.
fn build_code_graph(root: &Path) -> Result<()> {
    use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
    use sqry_core::graph::unified::persistence::{GraphStorage, save_to_path};
    use sqry_plugin_registry::create_plugin_manager;

    let plugins = create_plugin_manager();
    let config = BuildConfig::default();
    let graph = build_unified_graph(root, &plugins, &config)?;

    // Save to .sqry/graph/snapshot.sqry
    let storage = GraphStorage::new(root);
    std::fs::create_dir_all(storage.graph_dir())?;
    save_to_path(&graph, storage.snapshot_path())?;

    Ok(())
}

#[allow(dead_code)]
pub struct TestServer {
    #[allow(dead_code)]
    pub session: SessionManager,
    pub service: LspService<SqryLanguageServer>,
}

#[allow(dead_code)]
impl TestServer {
    pub fn new(root: &Path) -> Self {
        let _ = ensure_index(root);
        let session = SessionManager::new(options_for(root));
        let service = build_test_service(&session);
        Self { session, service }
    }

    pub async fn send_request(&mut self, request: Request) -> Result<Option<Response>> {
        let response = self.service.ready().await?.call(request).await?;
        Ok(response)
    }
}

/// Helper to find the sqry binary for testing
///
/// Tries `CARGO_BIN_EXE_sqry` first, then falls back to looking in target/debug or target/release.
/// This makes tests work both in CI (where `CARGO_BIN_EXE_sqry` is set) and locally
/// (where it might not be set in workspace contexts).
#[allow(dead_code)]
pub fn sqry_bin() -> PathBuf {
    #[allow(clippy::map_unwrap_or)] // Test helper uses map/unwrap_or pattern
    std::env::var("CARGO_BIN_EXE_sqry")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            // Fallback: look in target/debug or target/release
            let manifest_dir = env!("CARGO_MANIFEST_DIR");
            let workspace_dir = PathBuf::from(manifest_dir).parent().unwrap().to_path_buf();

            let exe_suffix = std::env::consts::EXE_SUFFIX;
            let make_candidate = |base: &str| {
                if exe_suffix.is_empty() {
                    PathBuf::from(base)
                } else {
                    PathBuf::from(format!("{base}{exe_suffix}"))
                }
            };
            let debug_path = workspace_dir.join(make_candidate("target/debug/sqry"));
            let release_path = workspace_dir.join(make_candidate("target/release/sqry"));

            if debug_path.exists() {
                debug_path
            } else if release_path.exists() {
                release_path
            } else {
                panic!(
                    "Could not find sqry binary. Tried:\n  - CARGO_BIN_EXE_sqry environment variable\n  - {}\n  - {}\n\nRun `cargo build` first.",
                    debug_path.display(),
                    release_path.display()
                );
            }
        })
}
