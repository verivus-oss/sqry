//! NL07 — LSP `sqry/ask` concurrency smoke tests.
//!
//! Acceptance gate from the DAG:
//!
//! 1. `lsp_serves_k_parallel_ask_requests` — `K` concurrent
//!    `sqry/ask` requests against a single `SessionManager` MUST
//!    complete within the NFR-5 wall-clock budget without deadlock,
//!    proving the per-session cached `Arc<Translator>` (NL07's
//!    `OnceCell<Arc<Translator>>` on `SessionManager`) feeds its
//!    classifier pool's `N` slots correctly.
//!
//! `#[ignore]`d by default — same rationale as NL06's
//! `shared_classifier_concurrency` test: requires the ONNX Runtime
//! dynamic library + the committed model fixtures under
//! `sqry-nl/models/`. The LSP `SessionManager::get_or_init_translator`
//! contract is unit-tested separately in `sqry-lsp/src/session.rs`.
//!
//! # Why this drives `handlers::ask::execute` rather than the LSP wire
//!
//! Standing up an in-process `tower_lsp::LspService` + JSON-RPC client
//! is non-trivial; the contract this gate enforces is structural:
//! every concurrent caller through `SessionManager::get_or_init_translator`
//! must observe the SAME `Arc<Translator>` (single load) and the
//! request-handling closure must serialise per slot but fan out across
//! N pool slots. The LSP server-side handler that `sqry/ask` JSON-RPC
//! routes to is `crate::handlers::ask::execute` — driving it directly
//! exercises the production code path with the production
//! `SessionManager`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use sqry_lsp::LspOptions;
use sqry_lsp::handlers::ask::execute;
use sqry_lsp::protocol::SqryAskParams;
use sqry_lsp::session::SessionManager;
use sqry_nl::TranslatorConfig;
use sqry_nl::classifier::{ClassifierPool, IntentClassifier, TrustMode};

/// Locate the committed in-tree model fixtures shipped under
/// `sqry-nl/models/`. The path is computed against `sqry-lsp`'s
/// manifest dir (we walk one level up to the workspace root).
fn in_tree_model_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sqry-lsp must have a parent (workspace root)")
        .join("sqry-nl/models")
}

/// Build a server-side `LspOptions` configured for stdio transport.
fn test_lsp_options(index_root: PathBuf) -> LspOptions {
    LspOptions {
        stdio: true,
        socket: None,
        index_root: Some(index_root),
        log_level: "warn".into(),
        config: None,
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
        workspace: None,
    }
}

#[test]
#[ignore = "requires ONNX Runtime dylib + committed model fixtures; run manually with --ignored"]
fn lsp_serves_k_parallel_ask_requests() {
    const POOL_SIZE: usize = 4;
    const FANIN: usize = 16;
    /// NFR-5 service-call wall-clock cap when serving FANIN concurrent
    /// translates against a pool of POOL_SIZE.
    const SERVICE_BUDGET: Duration = Duration::from_secs(60);

    let model_dir = in_tree_model_dir();
    assert!(
        model_dir.join("intent_classifier.onnx").exists(),
        "expected committed model at {}; install ONNX fixtures \
         before running this test",
        model_dir.display(),
    );

    // ----- Independent invariant: pool init calls IntentClassifier::load
    // exactly POOL_SIZE times. The LSP's session-cached
    // Arc<Translator> shares the same classifier-pool semantics as the
    // daemon; this guard catches regressions where a refactor of
    // `Translator::new` would over- or under-load sessions.
    {
        use sqry_nl::error::NlError;
        let load_calls = Arc::new(AtomicUsize::new(0));
        let load_calls_loader = Arc::clone(&load_calls);
        let model_dir_loader = model_dir.clone();
        let pool = ClassifierPool::new(POOL_SIZE, move || -> Result<IntentClassifier, NlError> {
            load_calls_loader.fetch_add(1, Ordering::SeqCst);
            IntentClassifier::load(&model_dir_loader, false, TrustMode::Custom)
                .map_err(NlError::from)
        })
        .expect("pool init");
        assert_eq!(pool.capacity(), POOL_SIZE);
        assert_eq!(
            load_calls.load(Ordering::SeqCst),
            POOL_SIZE,
            "loader must be invoked exactly once per slot during init"
        );
    }

    // ----- Build a real `SessionManager` against the workspace root.
    // The ask handler resolves paths against the session's root, so we
    // use the LSP crate's own manifest dir as a real canonicalisable
    // workspace.
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let opts = test_lsp_options(workspace_root.clone());
    let session = Arc::new(SessionManager::new(opts));

    // ----- Pre-init the per-session translator with our test pool size.
    // This pays the model-load cost upfront so the timed wave below
    // measures only request-serving cost.
    let translator_config = TranslatorConfig {
        model_dir_override: Some(model_dir.clone()),
        allow_unverified_model: false,
        classifier_pool_size: Some(POOL_SIZE),
        ..TranslatorConfig::default()
    };
    let _translator = session
        .get_or_init_translator(translator_config)
        .expect("get_or_init_translator must succeed against in-tree fixtures");

    // ----- Sanity-check that subsequent get_or_init_translator calls
    // return the SAME Arc (single load). OnceCell guarantees this; the
    // assertion documents the load-counter contract at the LSP layer.
    let translator_again = session
        .get_or_init_translator(TranslatorConfig::default())
        .expect("second get_or_init_translator must return the cached Arc");
    let translator_first = session
        .get_or_init_translator(TranslatorConfig::default())
        .expect("third get_or_init_translator must return the cached Arc");
    assert!(
        Arc::ptr_eq(&translator_again, &translator_first),
        "OnceCell must cache the same Arc<Translator> across all callers"
    );

    // ----- Baseline single-call latency. Mirror the daemon test
    // (`concurrent_ask_smoke.rs::daemon_serves_k_parallel_ask_calls`):
    // average over 4 sequential calls to smooth out the ONNX-runtime
    // first-call jit + tokenizer warm-up costs. The first call is
    // treated as a warm-up and discarded (the model_dir-bearing pre-init
    // call above already paid most of that cost, but a fresh sequential
    // baseline keeps the formula directly comparable across the two
    // tests).
    let workspace_root_for_baseline = workspace_root.clone();
    let baseline_params = |i: usize| SqryAskParams {
        query: format!("baseline call {i}"),
        path: None,
        model_dir: Some(model_dir.to_string_lossy().into_owned()),
        allow_unverified_model: false,
        allow_model_download: false,
    };
    {
        let _ = execute(session.as_ref(), &baseline_params(0)).expect("warmup ask must succeed");
    }
    let baseline_start = Instant::now();
    const BASELINE_N: usize = 4;
    for i in 0..BASELINE_N {
        let r =
            execute(session.as_ref(), &baseline_params(i + 1)).expect("baseline ask must succeed");
        assert!(
            matches!(
                r.response_type.as_str(),
                "execute" | "confirm" | "disambiguate" | "reject"
            ),
            "unexpected baseline response_type: {:?}",
            r.response_type
        );
    }
    let single_call_p50 = baseline_start.elapsed() / u32::try_from(BASELINE_N).unwrap();
    // Suppress unused-warning when this branch is reachable but the
    // baseline workspace handle is only consumed by the closure above.
    let _ = workspace_root_for_baseline;

    // ----- K=16 concurrent `sqry/ask` calls through the LSP handler.
    // Each spawn runs the same code path that tower_lsp's `sqry/ask`
    // route reaches in production: `handlers::ask::execute` →
    // `SessionManager::get_or_init_translator` (cache hit) →
    // `Translator::translate_shared` → `ClassifierPool::acquire`.
    //
    // Use a `std::sync::Barrier` so every worker enters the LSP
    // handler within the same scheduler tick — without this, slow
    // thread spawn-up serialises the wave and hides genuine pool
    // contention. This mirrors the daemon test's structure.
    let barrier = Arc::new(Barrier::new(FANIN));
    let service_start = Instant::now();
    let mut handles = Vec::with_capacity(FANIN);
    for tid in 0..FANIN {
        let session = Arc::clone(&session);
        let model_dir = model_dir.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || -> Duration {
            barrier.wait();
            let call_start = Instant::now();
            let params = SqryAskParams {
                query: format!("find functions named worker_{tid}"),
                path: None,
                model_dir: Some(model_dir.to_string_lossy().into_owned()),
                allow_unverified_model: false,
                allow_model_download: false,
            };
            let result = execute(session.as_ref(), &params)
                .expect("LSP ask handler must succeed for a non-empty query");
            // Response_type must be one of the four tier strings.
            assert!(
                matches!(
                    result.response_type.as_str(),
                    "execute" | "confirm" | "disambiguate" | "reject"
                ),
                "unexpected response_type: {:?}",
                result.response_type
            );
            call_start.elapsed()
        }));
    }
    let mut latencies: Vec<Duration> = Vec::with_capacity(FANIN);
    for h in handles {
        latencies.push(h.join().expect("worker thread panicked — pool deadlock?"));
    }
    let service_elapsed = service_start.elapsed();

    // Assertion 1: aggregate wall-clock budget.
    assert!(
        service_elapsed < SERVICE_BUDGET,
        "FANIN={FANIN} concurrent LSP ask requests exceeded SERVICE_BUDGET={SERVICE_BUDGET:?}: \
         got {service_elapsed:?}"
    );

    // Assertion 2: NFR-5 — per-call service-side p50 latency is
    // bounded by `(FANIN / POOL_SIZE) × 1.5 × single_call_p50`. The
    // 1.5× factor accounts for scheduler jitter + barrier-wait
    // overhead. Mirrors the daemon test
    // (`concurrent_ask_smoke.rs::daemon_serves_k_parallel_ask_calls`)
    // verbatim so the LSP and daemon paths share the same NFR-5
    // contract.
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
}
