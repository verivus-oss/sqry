//! STEP_10 — wrapper deprecation warning emission.
//!
//! Asserts the per-DAG contract for the `--index-root` deprecation
//! signal:
//!
//! - When `LspOptions::index_root` is `Some` AND the LSP `initialize`
//!   request carries a non-`null`
//!   `initializationOptions.sqry.workspace` `LogicalWorkspace` payload,
//!   the server emits exactly ONE `tracing::warn!` event with target
//!   `sqry::workspace`. The event carries `index_root`, `migration_doc`,
//!   and a message pointing operators to
//!   `docs/cli/workspace-wrapper-migration.md`.
//! - When `LspOptions::index_root` is `Some` but the
//!   `initializationOptions.sqry.workspace` payload is absent (or
//!   `null`), the deprecation event is NOT emitted — legacy-only callers
//!   are not nagged.
//! - When the payload is present but `index_root` is `None`, the
//!   deprecation event is also NOT emitted — modern-only callers are not
//!   nagged.
//!
//! The flag continues to work end-to-end. This is a strictly
//! informational signal (per DAG critical_decisions / constraints).

use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{Value, json};
use sqry_core::workspace::LogicalWorkspace;
use sqry_lsp::{LspOptions, build_test_service, session::SessionManager};
use tempfile::TempDir;
use tower::Service;
use tower::ServiceExt;
use tower::buffer::Buffer;
use tower_lsp::jsonrpc::Request;
use tower_lsp::lsp_types::InitializeParams;
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------------------
// Tracing capture helpers (mirrors the pattern in telemetry_resolution.rs)
// ---------------------------------------------------------------------------

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

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(body(logs.clone()));

    let bytes = logs.lock().unwrap().clone();
    String::from_utf8(bytes).expect("utf8")
}

// ---------------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------------

fn options_with_index_root(root: Option<&std::path::Path>) -> LspOptions {
    LspOptions {
        stdio: false,
        socket: None,
        index_root: root.map(std::path::Path::to_path_buf),
        log_level: "warn".into(),
        config: None,
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
    }
}

/// Build a serialized `LogicalWorkspace` payload suitable for use as
/// `initializationOptions.sqry.workspace`.
fn build_workspace_payload(root: &std::path::Path) -> Value {
    let workspace = LogicalWorkspace::single_root(root.to_path_buf())
        .expect("single_root LogicalWorkspace constructs");
    serde_json::to_value(&workspace).expect("LogicalWorkspace serializes")
}

/// Send `initialize` + `initialized` with the supplied `initializationOptions`.
async fn drive_initialize_with_options(
    buffered: &mut Buffer<tower_lsp::LspService<sqry_lsp::SqryLanguageServer>, Request>,
    init_options: Option<Value>,
) -> Result<()> {
    let params = InitializeParams {
        initialization_options: init_options,
        ..Default::default()
    };
    let initialize = Request::build("initialize")
        .params(serde_json::to_value(params)?)
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

fn workspace_warning_lines(log_output: &str) -> Vec<&str> {
    log_output
        .lines()
        .filter(|line| line.contains("sqry::workspace"))
        .filter(|line| line.contains("WARN"))
        .filter(|line| line.contains("--index-root"))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn warns_when_index_root_and_workspace_payload_both_present() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().to_path_buf();

    let log_output = capture_logs_with_filter("sqry::workspace=warn", |_logs| {
        let root = root.clone();
        async move {
            let session = SessionManager::new(options_with_index_root(Some(&root)));
            let service = build_test_service(&session);
            let mut buffered = Buffer::new(service, 4);

            let payload = build_workspace_payload(&root);
            let init_options = json!({ "sqry": { "workspace": payload } });

            drive_initialize_with_options(&mut buffered, Some(init_options))
                .await
                .expect("init ok");
        }
    });

    let lines = workspace_warning_lines(&log_output);
    assert_eq!(
        lines.len(),
        1,
        "STEP_10 — exactly ONE WARN-level deprecation event must be emitted when \
         --index-root coexists with a LogicalWorkspace payload; got {} lines: {:#?}",
        lines.len(),
        lines
    );

    let line = lines[0];
    assert!(
        line.contains("index_root="),
        "deprecation event must carry index_root field; line: {line}"
    );
    assert!(
        line.contains("migration_doc=") && line.contains("workspace-wrapper-migration.md"),
        "deprecation event must carry migration_doc field pointing to the migration guide; \
         line: {line}"
    );
    assert!(
        line.contains("initializationOptions.sqry.workspace")
            || line.contains("sqry.indexRoot")
            || line.contains(".sqry-workspace"),
        "deprecation message must reference the in-band workspace payload (e.g. \
         initializationOptions.sqry.workspace, sqry.indexRoot, or the .sqry-workspace \
         registry) as the migration target; line: {line}"
    );
}

#[test]
fn no_warning_when_index_root_alone_without_payload() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().to_path_buf();

    let log_output = capture_logs_with_filter("sqry::workspace=warn", |_logs| {
        let root = root.clone();
        async move {
            let session = SessionManager::new(options_with_index_root(Some(&root)));
            let service = build_test_service(&session);
            let mut buffered = Buffer::new(service, 4);

            // No initializationOptions at all — legacy-only invocation.
            drive_initialize_with_options(&mut buffered, None)
                .await
                .expect("init ok");
        }
    });

    let lines = workspace_warning_lines(&log_output);
    assert!(
        lines.is_empty(),
        "STEP_10 — legacy-only --index-root callers must NOT see the deprecation event; \
         got: {lines:#?}"
    );
}

#[test]
fn no_warning_when_payload_alone_without_index_root() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().to_path_buf();

    let log_output = capture_logs_with_filter("sqry::workspace=warn", |_logs| {
        let root = root.clone();
        async move {
            // No --index-root on options.
            let session = SessionManager::new(options_with_index_root(None));
            let service = build_test_service(&session);
            let mut buffered = Buffer::new(service, 4);

            let payload = build_workspace_payload(&root);
            let init_options = json!({ "sqry": { "workspace": payload } });

            drive_initialize_with_options(&mut buffered, Some(init_options))
                .await
                .expect("init ok");
        }
    });

    let lines = workspace_warning_lines(&log_output);
    assert!(
        lines.is_empty(),
        "STEP_10 — modern-only callers (payload without --index-root) must NOT see the \
         deprecation event; got: {lines:#?}"
    );
}

#[test]
fn warns_when_index_root_and_inband_indexroot_both_present() {
    // STEP_10 iter3 — exercise the new in-band `sqry.indexRoot` wire-up.
    // When the CLI `--index-root` flag is combined with the in-band
    // `initializationOptions.sqry.indexRoot` value (forwarded from the
    // extension's `sqry.indexRoot` setting), the deprecation event MUST
    // fire so operators are nudged to drop the redundant CLI flag.
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().to_path_buf();

    let log_output = capture_logs_with_filter("sqry::workspace=warn", |_logs| {
        let root = root.clone();
        async move {
            let session = SessionManager::new(options_with_index_root(Some(&root)));
            let service = build_test_service(&session);
            let mut buffered = Buffer::new(service, 4);

            let init_options = json!({
                "sqry": { "indexRoot": root.display().to_string() }
            });

            drive_initialize_with_options(&mut buffered, Some(init_options))
                .await
                .expect("init ok");
        }
    });

    let lines = workspace_warning_lines(&log_output);
    assert_eq!(
        lines.len(),
        1,
        "STEP_10 iter3 — exactly ONE WARN-level deprecation event must be emitted \
         when --index-root coexists with initializationOptions.sqry.indexRoot; \
         got {} lines: {:#?}",
        lines.len(),
        lines
    );
    let line = lines[0];
    assert!(
        line.contains("indexRoot") || line.contains("sqry.indexRoot"),
        "deprecation message must reference initializationOptions.sqry.indexRoot \
         as the migration target; line: {line}"
    );
}

#[test]
fn no_warning_when_inband_indexroot_alone_without_cli_flag() {
    // STEP_10 iter3 — the in-band `sqry.indexRoot` wire-up is the
    // recommended steady state. When the CLI flag is absent and only
    // the in-band value is present, the deprecation event must NOT
    // fire — modern-only callers are not nagged.
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().to_path_buf();

    let log_output = capture_logs_with_filter("sqry::workspace=warn", |_logs| {
        let root = root.clone();
        async move {
            let session = SessionManager::new(options_with_index_root(None));
            let service = build_test_service(&session);
            let mut buffered = Buffer::new(service, 4);

            let init_options = json!({
                "sqry": { "indexRoot": root.display().to_string() }
            });

            drive_initialize_with_options(&mut buffered, Some(init_options))
                .await
                .expect("init ok");
        }
    });

    let lines = workspace_warning_lines(&log_output);
    assert!(
        lines.is_empty(),
        "STEP_10 iter3 — modern-only callers (in-band indexRoot without --index-root) \
         must NOT see the deprecation event; got: {lines:#?}"
    );
}

#[test]
fn no_warning_when_inband_indexroot_is_empty_string() {
    // STEP_10 iter3 — the extension trims the setting and only sends
    // it when non-empty, but the LSP also treats empty strings as
    // "absent" defensively. Confirm both directions: an empty-string
    // `indexRoot` in the payload is treated as not-present, so paired
    // with --index-root it MUST NOT trigger the warning.
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().to_path_buf();

    let log_output = capture_logs_with_filter("sqry::workspace=warn", |_logs| {
        let root = root.clone();
        async move {
            let session = SessionManager::new(options_with_index_root(Some(&root)));
            let service = build_test_service(&session);
            let mut buffered = Buffer::new(service, 4);

            let init_options = json!({
                "sqry": { "indexRoot": "   " }
            });

            drive_initialize_with_options(&mut buffered, Some(init_options))
                .await
                .expect("init ok");
        }
    });

    let lines = workspace_warning_lines(&log_output);
    assert!(
        lines.is_empty(),
        "STEP_10 iter3 — empty/whitespace indexRoot must be treated as absent and \
         must NOT trigger the deprecation event when paired with --index-root; got: {lines:#?}"
    );
}

#[test]
fn no_warning_when_workspace_payload_is_explicit_null() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().to_path_buf();

    let log_output = capture_logs_with_filter("sqry::workspace=warn", |_logs| {
        let root = root.clone();
        async move {
            let session = SessionManager::new(options_with_index_root(Some(&root)));
            let service = build_test_service(&session);
            let mut buffered = Buffer::new(service, 4);

            // Defensively encoded `null` payload — clients that set the
            // key to `null` to "explicitly disable" must not be nagged.
            let init_options = json!({ "sqry": { "workspace": Value::Null } });

            drive_initialize_with_options(&mut buffered, Some(init_options))
                .await
                .expect("init ok");
        }
    });

    let lines = workspace_warning_lines(&log_output);
    assert!(
        lines.is_empty(),
        "STEP_10 — explicit-null workspace payload must NOT trigger the deprecation event; \
         got: {lines:#?}"
    );
}
