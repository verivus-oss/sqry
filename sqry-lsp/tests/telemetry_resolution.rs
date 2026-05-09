//! STEP_12 — LogicalWorkspace resolution telemetry tests.
//!
//! Asserts the per-DAG contract:
//!
//! - The LSP emits exactly one `tracing::info!` event with target
//!   `sqry::workspace` per LogicalWorkspace resolution. The event
//!   carries `workspace_id_short`, `source_root_count`, `member_count`,
//!   and `exclusion_count`.
//! - At INFO level, the FULL hex digest is **not** emitted.
//! - At DEBUG level, an additional `tracing::debug!` event with target
//!   `sqry::workspace` carries `workspace_id_full` (full 64-char hex).
//! - The resolution path emits ONE aggregate line — no per-folder spam
//!   that could regress to the original bug.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use tower::Service;
use tower::ServiceExt;
use tower::buffer::Buffer;
use tower_lsp::jsonrpc::Request;
use tower_lsp::lsp_types::InitializeParams;
use tracing_subscriber::EnvFilter;

mod common;

type TestLspFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = std::result::Result<
                    Option<tower_lsp::jsonrpc::Response>,
                    tower_lsp::ExitedError,
                >,
            > + Send,
    >,
>;
type TestLspBuffer = Buffer<Request, TestLspFuture>;

struct VecWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for VecWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct MakeVecWriter(Arc<Mutex<Vec<u8>>>);

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for MakeVecWriter {
    type Writer = VecWriter;

    fn make_writer(&'a self) -> Self::Writer {
        VecWriter(self.0.clone())
    }
}

async fn drive_initialize(buffered: &mut TestLspBuffer) -> Result<()> {
    let initialize = Request::build("initialize")
        .params(serde_json::to_value(InitializeParams::default())?)
        .id(0i64)
        .finish();
    buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .call(initialize)
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;

    let initialized = Request::build("initialized").finish();
    buffered
        .ready()
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .call(initialized)
        .await
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;
    Ok(())
}

fn capture_logs_with_filter<F: std::future::Future<Output = ()>>(
    filter: &str,
    body: impl FnOnce(Arc<Mutex<Vec<u8>>>) -> F,
) -> String {
    let logs: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let env_filter = EnvFilter::try_new(filter).expect("valid filter");
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .with_writer(MakeVecWriter(logs.clone()))
        .without_time()
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    // Drive the body inline. We construct a runtime here so the
    // subscriber stays "set_default" for the entire test (tasks spawned
    // inside still inherit it).
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(body(logs.clone()));

    let bytes = logs.lock().unwrap().clone();
    String::from_utf8(bytes).expect("utf8")
}

#[test]
fn info_emits_exactly_one_resolution_event_with_short_id_and_counts() {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    common::ensure_index(&root).expect("index built");

    let log_output = capture_logs_with_filter("sqry::workspace=info", |_logs| async move {
        let session = sqry_lsp::session::SessionManager::new(common::options_for(&root));
        let service = sqry_lsp::build_test_service(&session);
        let mut buffered = Buffer::new(service, 4);
        drive_initialize(&mut buffered).await.expect("init ok");
    });

    // Find every line whose target is `sqry::workspace`. The default
    // `tracing-subscriber::fmt` formatter prints the target as a token
    // before the message, so the substring search is reliable.
    let workspace_lines: Vec<&str> = log_output
        .lines()
        .filter(|l| l.contains("sqry::workspace"))
        .collect();

    assert_eq!(
        workspace_lines.len(),
        1,
        "STEP_12 — exactly ONE INFO-level sqry::workspace event must be emitted on resolution; \
         got {} lines: {:#?}",
        workspace_lines.len(),
        workspace_lines
    );
    let line = workspace_lines[0];
    assert!(
        line.contains("workspace_id_short="),
        "INFO event must carry workspace_id_short field; line: {line}"
    );
    assert!(
        line.contains("source_root_count="),
        "INFO event must carry source_root_count field; line: {line}"
    );
    assert!(
        line.contains("member_count="),
        "INFO event must carry member_count field; line: {line}"
    );
    assert!(
        line.contains("exclusion_count="),
        "INFO event must carry exclusion_count field; line: {line}"
    );
    assert!(
        !line.contains("workspace_id_full="),
        "INFO event must NOT carry workspace_id_full (DEBUG-only by spec); line: {line}"
    );
}

#[test]
fn debug_adds_workspace_id_full_event() {
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    common::ensure_index(&root).expect("index built");

    let log_output = capture_logs_with_filter("sqry::workspace=debug", |_logs| async move {
        let session = sqry_lsp::session::SessionManager::new(common::options_for(&root));
        let service = sqry_lsp::build_test_service(&session);
        let mut buffered = Buffer::new(service, 4);
        drive_initialize(&mut buffered).await.expect("init ok");
    });

    let workspace_lines: Vec<&str> = log_output
        .lines()
        .filter(|l| l.contains("sqry::workspace"))
        .collect();

    // At DEBUG we expect at least one INFO line + one DEBUG line.
    assert!(
        workspace_lines.len() >= 2,
        "DEBUG run must emit both the INFO and the DEBUG events; got {} lines: {:#?}",
        workspace_lines.len(),
        workspace_lines
    );

    let any_full = workspace_lines
        .iter()
        .any(|l| l.contains("workspace_id_full="));
    assert!(
        any_full,
        "DEBUG run must emit workspace_id_full somewhere; lines: {workspace_lines:#?}"
    );

    // Verify the full id is the canonical 64 hex chars (the BLAKE3
    // digest is fixed-width).
    let full_id_line = workspace_lines
        .iter()
        .find(|l| l.contains("workspace_id_full="))
        .expect("found a line containing workspace_id_full");
    // Parse out the hex token. The formatter prints the field as
    // `workspace_id_full=<hex>` (unquoted scalar for primitives).
    let after = full_id_line
        .split("workspace_id_full=")
        .nth(1)
        .expect("payload after =");
    let hex: String = after
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    assert_eq!(
        hex.len(),
        64,
        "workspace_id_full must serialize as 64 hex chars; got {} ({hex:?})",
        hex.len()
    );
}

#[test]
fn no_per_folder_resolution_lines_emitted() {
    // Acceptance #7 — telemetry test asserts no per-folder log lines
    // are emitted during workspace resolution. The contract is "ONE
    // aggregate log line per resolution"; if a future refactor ever
    // brings back per-folder loops emitting per-folder INFO lines under
    // target `sqry::workspace`, this test fails.
    let root = common::fixture_path("sqry-lsp/tests/fixtures/mini-workspace");
    common::ensure_index(&root).expect("index built");

    let log_output = capture_logs_with_filter("sqry::workspace=info", |_logs| async move {
        let session = sqry_lsp::session::SessionManager::new(common::options_for(&root));
        let service = sqry_lsp::build_test_service(&session);
        let mut buffered = Buffer::new(service, 4);
        drive_initialize(&mut buffered).await.expect("init ok");
    });

    let workspace_info_lines: Vec<&str> = log_output
        .lines()
        .filter(|l| l.contains("sqry::workspace"))
        .collect();

    // Exactly one — that's the aggregate.
    assert_eq!(
        workspace_info_lines.len(),
        1,
        "Per-folder spam regression guard — only ONE INFO line under \
         sqry::workspace is allowed per resolution; got {workspace_info_lines:#?}"
    );
}
