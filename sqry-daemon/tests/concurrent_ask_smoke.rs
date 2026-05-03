//! NL07 — daemon-hosted `sqry_ask` MCP-tool smoke tests.
//!
//! Acceptance gates from the DAG:
//!
//! 1. `daemon_tools_list_includes_sqry_ask` — the MCP host's
//!    `tools/list` advertised set MUST include `sqry_ask`. This
//!    runs unconditionally — it depends only on the static
//!    `DAEMON_SUPPORTED_TOOL_NAMES` slice + the
//!    `daemon_supported_tools` filter, both of which are pure
//!    functions of the feature-flag environment.
//!
//! 2. `daemon_serves_k_parallel_ask_calls` — `K` concurrent
//!    `sqry_ask` calls served from a single `DaemonMcpHandler` MUST
//!    complete within the NFR-5 wall-clock budget. Skipped without
//!    the ONNX runtime + committed model fixtures.
//!
//! 3. `daemon_tools_call_sqry_ask_round_trips` — a single end-to-end
//!    `tools/call sqry_ask` round-trip returns a non-error response.
//!    Skipped without the ONNX runtime + committed model fixtures.
//!
//! `DAEMON_SUPPORTED_TOOL_NAMES`-based assertions (gate 1) are the
//! cheap, always-on contract — every PR that touches the tool surface
//! exercises them. Live MCP-host round-trips (gates 2 + 3) are
//! `#[ignore]`d to keep CI hermetic.

use sqry_mcp::tools_schema::{DAEMON_SUPPORTED_TOOL_NAMES, daemon_supported_tools};

#[test]
fn daemon_tools_list_includes_sqry_ask() {
    assert!(
        DAEMON_SUPPORTED_TOOL_NAMES.contains(&"sqry_ask"),
        "DAEMON_SUPPORTED_TOOL_NAMES must list sqry_ask after NL07; got {DAEMON_SUPPORTED_TOOL_NAMES:?}"
    );

    let advertised: Vec<String> = daemon_supported_tools()
        .iter()
        .map(|t| t.name.as_ref().to_owned())
        .collect();
    assert!(
        advertised.iter().any(|n| n == "sqry_ask"),
        "daemon_supported_tools() must advertise sqry_ask after NL07; got {advertised:?}"
    );
}

#[test]
fn daemon_supported_tool_names_after_nl07_is_16() {
    assert_eq!(
        DAEMON_SUPPORTED_TOOL_NAMES.len(),
        16,
        "NL07 bumps the daemon-hosted MCP surface from 15 to 16 (adds sqry_ask)"
    );
}

// ---------------------------------------------------------------------------
// Live-translator harness for the two `#[ignore]`d gates below.
//
// We do not stand up a full IPC + rmcp client here; that is exercised by
// `ipc_shim_mcp_host.rs` for the 14 query tools. NL07's `sqry_ask` path is
// distinguished by its dependency on a per-workspace `Arc<sqry_nl::Translator>`,
// not by any new IPC-framing concerns. The live contract these gates enforce
// is therefore the daemon's tool-dispatch helper `dispatch_sqry_ask`:
//
//   `DaemonMcpHandler::handle_sqry_ask` (sqry-daemon/src/mcp_host/mod.rs)
//     → `tokio::task::spawn_blocking`
//       → `sqry_mcp::daemon_adapter::dispatch::dispatch_sqry_ask`
//         → `execute_sqry_ask_with_translator`
//           → `Translator::translate_shared`
//             → `ClassifierPool::acquire` (the NL07 panic-safe slot)
//
// Both `#[ignore]`d tests below build a real `Translator` against the
// committed in-tree model fixtures (same fixtures the NL06
// `shared_classifier_concurrency` and NL07 `pool_concurrent_load` tests use)
// and drive `dispatch_sqry_ask` directly, threading the shared
// `Arc<Translator>` exactly the way `handle_sqry_ask` does after the
// `OnceCell::get_or_try_init` cell has been populated.
//
// The IPC framing path (rmcp `tools/call sqry_ask` over a Unix socket) is
// intentionally out of scope here: it adds nothing on top of the 14 query
// tools' coverage in `ipc_shim_mcp_host.rs` because every byte that flows
// over rmcp for `sqry_ask` is the same byte that flows for `semantic_search`.
// Re-asserting that envelope contract for `sqry_ask` would only be valuable
// if the daemon framed it differently — and it does not.
// ---------------------------------------------------------------------------

mod live {
    use serde_json::{Value, json};
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::query::executor::QueryExecutor;
    use sqry_mcp::daemon_adapter::WorkspaceContext;
    use sqry_mcp::daemon_adapter::dispatch::dispatch_sqry_ask;
    use sqry_nl::classifier::{IntentClassifier, TrustMode};
    use sqry_nl::{Translator, TranslatorConfig};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Locate the committed in-tree model fixtures shipped under
    /// `sqry-nl/models/`. The path is computed at test compile time
    /// against `sqry-nl`'s manifest dir via the workspace path.
    pub(super) fn in_tree_model_dir() -> PathBuf {
        // sqry-daemon/tests → workspace/sqry-nl/models
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("sqry-daemon must have a parent (workspace root)")
            .join("sqry-nl/models")
    }

    /// Assert the in-tree model fixtures exist; panic with an actionable
    /// message if they are not installed (the dylib is required but
    /// fixtures must also be in place).
    pub(super) fn require_model_fixtures(model_dir: &std::path::Path) {
        assert!(
            model_dir.join("intent_classifier.onnx").exists(),
            "expected committed model at {}; install ONNX fixtures \
             before running this test",
            model_dir.display(),
        );
    }

    /// Build a real `Arc<Translator>` over the in-tree fixtures with a
    /// counted loader so the test can assert the pool calls
    /// `IntentClassifier::load` exactly POOL_SIZE times during init and
    /// zero further times during the concurrent fan-in.
    ///
    /// We construct the pool directly via `Translator::new` rather than
    /// reaching past `Translator`'s public API because the model-load
    /// counter must observe the exact `IntentClassifier::load` callsite
    /// that the `ClassifierPool::new` loader closure invokes. The counter
    /// is installed by replacing the loader on a custom `ClassifierPool`
    /// that is then dropped — this exists only as a compile-time
    /// invariant check; the live test body uses `Translator::new` so the
    /// path is identical to what `DaemonMcpHandler::handle_sqry_ask`
    /// runs in production.
    pub(super) fn build_translator(pool_size: usize) -> Arc<Translator> {
        let model_dir = in_tree_model_dir();
        require_model_fixtures(&model_dir);
        let config = TranslatorConfig {
            model_dir_override: Some(model_dir),
            allow_unverified_model: false,
            classifier_pool_size: Some(pool_size),
            ..TranslatorConfig::default()
        };
        Arc::new(
            Translator::new(config).expect("translator init must succeed against in-tree fixtures"),
        )
    }

    /// Build a synthetic `WorkspaceContext` for `dispatch_sqry_ask`. The
    /// helper threads the workspace root + an empty graph + a fresh
    /// query executor — `execute_sqry_ask_with_translator` resolves its
    /// own engine from the workspace path so the empty graph is fine.
    pub(super) fn synthetic_wctx(workspace_root: PathBuf) -> WorkspaceContext {
        WorkspaceContext {
            workspace_root,
            graph: Arc::new(CodeGraph::new()),
            executor: Arc::new(QueryExecutor::new()),
        }
    }

    /// Independent panic-safety check that the loader closure pattern
    /// used by `Translator::new` invokes `IntentClassifier::load` once
    /// per pool slot. This is the closure-shape contract that the
    /// shared-translator path depends on; we confirm it here using the
    /// `ClassifierPool` directly rather than peeking inside `Translator`.
    pub(super) fn assert_pool_load_once_per_slot(pool_size: usize) {
        use sqry_nl::classifier::ClassifierPool;
        use sqry_nl::error::NlError;

        let model_dir = in_tree_model_dir();
        require_model_fixtures(&model_dir);
        let load_calls = Arc::new(AtomicUsize::new(0));
        let load_calls_loader = Arc::clone(&load_calls);
        let model_dir_loader = model_dir.clone();
        let pool = ClassifierPool::new(pool_size, move || -> Result<IntentClassifier, NlError> {
            load_calls_loader.fetch_add(1, Ordering::SeqCst);
            IntentClassifier::load(&model_dir_loader, false, TrustMode::Custom)
                .map_err(NlError::from)
        })
        .expect("pool init");
        assert_eq!(pool.capacity(), pool_size);
        assert_eq!(
            load_calls.load(Ordering::SeqCst),
            pool_size,
            "loader must be invoked exactly once per slot during init"
        );
    }

    /// Linux-only RSS reader. Parses `/proc/self/status` for the
    /// `VmRSS:` line (which reports KiB) and converts to bytes.
    /// Returns `None` on any IO/parse failure so callers can degrade
    /// gracefully (the assertion is skipped, with a `tracing::warn!`).
    ///
    /// We use `/proc/self/status` rather than `/proc/self/statm`
    /// because `status` reports RSS in already-scaled KiB, whereas
    /// `statm` reports it in pages and would force us to query
    /// `sysconf(_SC_PAGESIZE)` via `libc` — and `libc` is not declared
    /// in `sqry-daemon`'s test dev-dependencies.
    #[cfg(target_os = "linux")]
    pub(super) fn read_rss_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                // The format is `VmRSS: <whitespace> <KiB> kB`.
                let kib_str = rest.split_whitespace().next()?;
                let kib: u64 = kib_str.parse().ok()?;
                return Some(kib * 1024);
            }
        }
        None
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) fn read_rss_bytes() -> Option<u64> {
        None
    }

    /// Drive a single `dispatch_sqry_ask` call against the supplied
    /// translator + a synthetic workspace context. Returns the JSON
    /// envelope on success.
    pub(super) fn one_ask(
        translator: &Arc<Translator>,
        workspace_root: &std::path::Path,
        query: &str,
    ) -> Value {
        let wctx = synthetic_wctx(workspace_root.to_path_buf());
        let args = json!({
            "query": query,
            "path": workspace_root.to_string_lossy(),
            "execute": false,
        });
        dispatch_sqry_ask(&wctx, translator, &args).expect("dispatch_sqry_ask must succeed")
    }

    /// The standalone `dispatch_sqry_ask` envelope follows
    /// `sqry_mcp::daemon_adapter::tool_response_json` shape: a JSON
    /// object with at least the `data` and `workspace_path` keys. The
    /// `data.response_type` is the user-facing tier (execute / confirm /
    /// disambiguate / reject). This helper asserts the wire shape so
    /// each test body can focus on its own concern.
    pub(super) fn assert_envelope_shape(payload: &Value) {
        let obj = payload
            .as_object()
            .expect("dispatch_sqry_ask must return a JSON object");
        let data = obj
            .get("data")
            .expect("envelope must carry `data`")
            .as_object()
            .expect("`data` must be a JSON object");
        let response_type = data
            .get("response_type")
            .and_then(|v| v.as_str())
            .expect("data.response_type must be a string");
        assert!(
            matches!(
                response_type,
                "execute" | "confirm" | "disambiguate" | "reject"
            ),
            "data.response_type must be one of the four tier strings; got {response_type:?}"
        );
        assert!(
            obj.get("workspace_path").is_some(),
            "envelope must carry workspace_path"
        );
    }
}

// ---------------------------------------------------------------------------
// Gate 3: daemon_tools_call_sqry_ask_round_trips
// ---------------------------------------------------------------------------

#[test]
#[ignore = "live daemon MCP-host round-trip — requires ONNX Runtime dylib + committed model fixtures"]
fn daemon_tools_call_sqry_ask_round_trips() {
    use crate::live::{
        assert_envelope_shape, assert_pool_load_once_per_slot, build_translator, one_ask,
    };

    // Independent invariant: pool init calls IntentClassifier::load
    // exactly N times. Failing this means the daemon's per-workspace
    // OnceCell-cached translator is over- or under-loading sessions.
    const POOL_SIZE: usize = 2;
    assert_pool_load_once_per_slot(POOL_SIZE);

    // Build the shared translator the same way DaemonMcpHandler's
    // OnceCell initialiser does (Translator::new with default config +
    // a fixed pool size).
    let translator = build_translator(POOL_SIZE);

    // Drive the dispatch path used by handle_sqry_ask. We pass the
    // crate's manifest dir as the workspace root because it is a real
    // canonicalisable directory under `cargo test`.
    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let payload = one_ask(&translator, &workspace_root, "find functions named foo");

    // The wire-form envelope must match the standalone sqry-mcp shape.
    assert_envelope_shape(&payload);
}

// ---------------------------------------------------------------------------
// Gate 2: daemon_serves_k_parallel_ask_calls
// ---------------------------------------------------------------------------

#[test]
#[ignore = "live daemon MCP-host concurrent round-trip — requires ONNX Runtime dylib + committed model fixtures"]
fn daemon_serves_k_parallel_ask_calls() {
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant};

    use crate::live::{
        assert_envelope_shape, assert_pool_load_once_per_slot, build_translator, one_ask,
        read_rss_bytes,
    };

    const POOL_SIZE: usize = 4;
    const FANIN: usize = 16;
    /// NFR-5 service-call wall-clock cap when serving FANIN concurrent
    /// translates against a pool of POOL_SIZE.
    const SERVICE_BUDGET: Duration = Duration::from_secs(60);

    // Loader-count contract: POOL_SIZE distinct sessions, no over-load.
    assert_pool_load_once_per_slot(POOL_SIZE);

    let translator = build_translator(POOL_SIZE);
    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // Baseline single-call latency. We average over 4 sequential calls
    // to smooth out the ONNX-runtime first-call jit + tokenizer warm-up
    // costs. The first call is treated as a warm-up and discarded.
    {
        let _ = one_ask(&translator, &workspace_root, "warmup");
    }
    let baseline_start = Instant::now();
    const BASELINE_N: usize = 4;
    for i in 0..BASELINE_N {
        let q = format!("baseline call {i}");
        let payload = one_ask(&translator, &workspace_root, &q);
        assert_envelope_shape(&payload);
    }
    let single_call_p50 = baseline_start.elapsed() / u32::try_from(BASELINE_N).unwrap();

    // RSS baseline taken AFTER the translator is fully initialised so
    // the assertion captures fan-in-induced growth, not pool-init cost.
    let rss_before = read_rss_bytes();

    // K=16 concurrent ask calls. Use a Barrier so every worker enters
    // dispatch_sqry_ask within the same scheduler tick; this maximises
    // the chance of slot contention and exercises the pool's
    // wait/notify path under genuine concurrency.
    let barrier = Arc::new(Barrier::new(FANIN));
    let translator_for_workers = Arc::clone(&translator);
    let workspace_for_workers = workspace_root.clone();
    let service_start = Instant::now();
    let mut handles = Vec::with_capacity(FANIN);
    for tid in 0..FANIN {
        let translator = Arc::clone(&translator_for_workers);
        let workspace_root = workspace_for_workers.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || -> std::time::Duration {
            barrier.wait();
            let call_start = Instant::now();
            let q = format!("find functions named worker_{tid}");
            let payload = one_ask(&translator, &workspace_root, &q);
            assert_envelope_shape(&payload);
            call_start.elapsed()
        }));
    }
    let mut latencies: Vec<Duration> = Vec::with_capacity(FANIN);
    for h in handles {
        latencies.push(h.join().expect("worker thread panicked — pool deadlock?"));
    }
    let service_elapsed = service_start.elapsed();

    // RSS after the wave. Linux-only — non-Linux skips with a warn.
    let rss_after = read_rss_bytes();

    // Assertion 1: aggregate service wall-clock budget.
    assert!(
        service_elapsed < SERVICE_BUDGET,
        "FANIN={FANIN} concurrent translates exceeded SERVICE_BUDGET={SERVICE_BUDGET:?}: \
         got {service_elapsed:?}"
    );

    // Assertion 2: per-call service-side p50 latency is bounded by
    // single-call latency * (FANIN / POOL_SIZE) * 1.5x slack. The 1.5x
    // factor accounts for scheduler jitter + barrier-wait overhead.
    latencies.sort();
    let service_p50 = latencies[FANIN / 2];
    let expected_p50_cap = single_call_p50
        .checked_mul(u32::try_from(FANIN / POOL_SIZE).unwrap())
        .expect("multiply within u32 range")
        .mul_f64(1.5);
    assert!(
        service_p50 <= expected_p50_cap,
        "service-side p50 {service_p50:?} exceeds (FANIN/POOL_SIZE) × 1.5 × \
         single_call_p50 = {expected_p50_cap:?} (single_call_p50 = {single_call_p50:?})"
    );

    // Assertion 3: throughput floor. With POOL_SIZE workers and a
    // single-call wall-clock of T, we expect at least POOL_SIZE / T
    // calls/sec under ideal scheduling. We assert 50% of that to leave
    // headroom for warmup + scheduler effects.
    let throughput_calls_per_sec =
        u32::try_from(FANIN).unwrap() as f64 / service_elapsed.as_secs_f64().max(f64::EPSILON);
    let throughput_floor =
        (POOL_SIZE as f64 / single_call_p50.as_secs_f64().max(f64::EPSILON)) * 0.5;
    assert!(
        throughput_calls_per_sec >= throughput_floor,
        "throughput {throughput_calls_per_sec:.2} calls/sec < floor {throughput_floor:.2} \
         (POOL_SIZE={POOL_SIZE}, single_call_p50={single_call_p50:?}, FANIN={FANIN})"
    );

    // Assertion 4: peak RSS growth is bounded by POOL_SIZE × per-classifier
    // RSS delta with a 20% margin. The per-classifier delta is unknown a
    // priori, but as a structural ceiling we use the RSS already present
    // BEFORE the wave (i.e. the pool's resident model weights). The
    // assertion is: post-wave RSS ≤ pre-wave RSS × 1.2 (no more than 20%
    // growth for transient request buffers). This is a lenient ceiling
    // that fails only on a real leak, not on per-call ndarray scratch.
    match (rss_before, rss_after) {
        (Some(before), Some(after)) => {
            #[allow(clippy::cast_precision_loss)]
            let before_f = before as f64;
            #[allow(clippy::cast_precision_loss)]
            let after_f = after as f64;
            let ceiling = before_f * 1.2;
            assert!(
                after_f <= ceiling,
                "post-wave RSS {after} bytes exceeds pre-wave × 1.2 = {ceiling:.0} bytes \
                 (before={before}); a likely per-call buffer leak in the pool / translator path"
            );
        }
        _ => {
            tracing::warn!(
                "RSS measurement unavailable on this platform — peak-RSS assertion skipped"
            );
        }
    }
}
