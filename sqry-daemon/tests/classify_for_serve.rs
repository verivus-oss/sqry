//! Task 7 Phase 7c — `WorkspaceManager::classify_for_serve` surface
//! integration tests.
//!
//! Exercises every arm of the classifier against real workspace
//! states plus the `stale_serve_max_age_hours` cap against synthetic
//! timestamps. The `-32002 WorkspaceStaleExpired` JSON-RPC code is
//! asserted end-to-end via `DaemonError::jsonrpc_code()`.

mod support;

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use sqry_core::project::ProjectRootMode;
use sqry_daemon::{
    DaemonConfig, DaemonError, JSONRPC_WORKSPACE_STALE_EXPIRED, ServeVerdict, WorkspaceKey,
    WorkspaceManager, WorkspaceState,
    workspace::{WorkingSetInputs, working_set_estimate},
};

fn make_manager(stale_cap_hours: u32) -> Arc<WorkspaceManager> {
    let config = Arc::new(DaemonConfig {
        stale_serve_max_age_hours: stale_cap_hours,
        ..DaemonConfig::default()
    });
    WorkspaceManager::new_without_reaper(config)
}

fn register_key() -> WorkspaceKey {
    WorkspaceKey::new(
        PathBuf::from("/repos/classify_test"),
        ProjectRootMode::GitRoot,
        0,
    )
}

#[test]
fn classify_for_serve_returns_fresh_for_loaded_workspace() {
    let harness = support::DispatchHarness::new();
    let verdict = harness
        .manager
        .classify_for_serve(&harness.key, SystemTime::now())
        .expect("Loaded workspace must classify as Fresh");
    assert!(
        matches!(verdict, ServeVerdict::Fresh { .. }),
        "expected Fresh, got {verdict:?}"
    );
}

#[test]
fn classify_for_serve_returns_stale_within_cap() {
    let harness = support::DispatchHarness::with_debounce(50);
    let ws = harness.manager.lookup(&harness.key).expect("loaded");
    // Drive into Failed state synthetically.
    ws.store_state(WorkspaceState::Failed);
    let now = SystemTime::now();
    // 12h ago — well within a 24h cap.
    ws.set_last_good_at_for_test(Some(now - Duration::from_secs(12 * 3600)));

    let verdict = harness
        .manager
        .classify_for_serve(&harness.key, now)
        .expect("Failed within cap must classify as Stale");
    match verdict {
        ServeVerdict::Stale { age_hours, .. } => {
            assert_eq!(age_hours, 12, "age_hours must reflect 12h synthetic offset");
        }
        other => panic!("expected Stale, got {other:?}"),
    }
}

#[test]
fn classify_for_serve_returns_expired_past_cap_with_correct_age_and_cap() {
    let harness = support::DispatchHarness::with_debounce(50);
    let ws = harness.manager.lookup(&harness.key).expect("loaded");
    ws.store_state(WorkspaceState::Failed);
    let now = SystemTime::now();
    // 48h ago with cap = 24 → Expired.
    ws.set_last_good_at_for_test(Some(now - Duration::from_secs(48 * 3600)));

    let err = harness
        .manager
        .classify_for_serve(&harness.key, now)
        .expect_err("Failed past cap must error");

    match err {
        DaemonError::WorkspaceStaleExpired {
            age_hours,
            cap_hours,
            ..
        } => {
            assert_eq!(age_hours, 48, "age_hours");
            assert_eq!(cap_hours, 24, "cap_hours must match the config");
            // Full round-trip of the JSON-RPC mapping.
            assert_eq!(
                DaemonError::WorkspaceStaleExpired {
                    root: PathBuf::from("/dummy"),
                    age_hours: 48,
                    cap_hours: 24,
                    last_good_at: None,
                    last_error: None,
                }
                .jsonrpc_code(),
                Some(JSONRPC_WORKSPACE_STALE_EXPIRED)
            );
        }
        other => panic!("expected WorkspaceStaleExpired, got {other:?}"),
    }
}

#[test]
fn classify_for_serve_returns_build_failed_when_no_prior_good() {
    // Use a manager with no loaded workspace, but register the
    // workspace in Failed state with no last_good_at.
    let manager = make_manager(24);
    let key = register_key();
    // We need to drive a workspace into the map in Failed state with
    // no prior good. The simplest way is to add a WorkspaceBuilder
    // that errors, get_or_load once — which transitions it to Failed
    // via the LoadingGuard.
    let builder = support::AlwaysFailBuilder;
    let estimate = working_set_estimate(WorkingSetInputs {
        new_graph_final_estimate: 64 * 1024,
        staging_overhead: 32 * 1024,
        interner_snapshot_bytes: 16 * 1024,
    });
    let _err = manager
        .get_or_load(&key, &builder, estimate)
        .expect_err("must fail");

    let ws = manager.lookup(&key).expect("entry present");
    assert_eq!(ws.load_state(), WorkspaceState::Failed);
    assert!(ws.last_good_at.read().is_none());

    let err = manager
        .classify_for_serve(&key, SystemTime::now())
        .expect_err("no prior good must error");
    assert!(
        matches!(err, DaemonError::WorkspaceBuildFailed { .. }),
        "expected WorkspaceBuildFailed, got {err:?}"
    );
}

#[test]
fn classify_for_serve_returns_evicted_for_missing_workspace() {
    let manager = make_manager(24);
    let key = register_key();

    let err = manager
        .classify_for_serve(&key, SystemTime::now())
        .expect_err("missing workspace must error");
    assert!(
        matches!(err, DaemonError::WorkspaceEvicted { .. }),
        "expected WorkspaceEvicted, got {err:?}"
    );
    assert_eq!(err.jsonrpc_code(), Some(-32004));
}

#[test]
fn classify_for_serve_cap_zero_disables_expiry() {
    let harness = support::DispatchHarness::with_debounce_and_cap(50, 0);
    let ws = harness.manager.lookup(&harness.key).expect("loaded");
    ws.store_state(WorkspaceState::Failed);
    let now = SystemTime::now();
    // Ancient: 10,000 hours ago. cap = 0 → Stale regardless.
    ws.set_last_good_at_for_test(Some(now - Duration::from_secs(10_000 * 3600)));

    let verdict = harness
        .manager
        .classify_for_serve(&harness.key, now)
        .expect("cap=0 must never expire");
    match verdict {
        ServeVerdict::Stale { age_hours, .. } => {
            assert_eq!(age_hours, 10_000);
        }
        other => panic!("expected Stale with cap=0, got {other:?}"),
    }
}

#[test]
fn classify_for_serve_returns_not_ready_for_unloaded_state() {
    let manager = make_manager(24);
    // Pre-create a LoadedWorkspace in Unloaded state without going
    // through get_or_load (which would transition to Loading/Loaded).
    let key = register_key();
    support::insert_workspace_in_state(&manager, &key, WorkspaceState::Unloaded);

    let verdict = manager
        .classify_for_serve(&key, SystemTime::now())
        .expect("Unloaded must classify as NotReady");
    match verdict {
        ServeVerdict::NotReady { state } => assert_eq!(state, WorkspaceState::Unloaded),
        other => panic!("expected NotReady {{ Unloaded }}, got {other:?}"),
    }
}

#[test]
fn classify_for_serve_returns_not_ready_for_loading_state() {
    let manager = make_manager(24);
    let key = register_key();
    support::insert_workspace_in_state(&manager, &key, WorkspaceState::Loading);

    let verdict = manager
        .classify_for_serve(&key, SystemTime::now())
        .expect("Loading must classify as NotReady");
    match verdict {
        ServeVerdict::NotReady { state } => assert_eq!(state, WorkspaceState::Loading),
        other => panic!("expected NotReady {{ Loading }}, got {other:?}"),
    }
}

/// Task 5 (spec item E). Verifies `error_data()` renders
/// `last_good_at` as RFC3339 / UTC-Zulu and round-trips `last_error`
/// verbatim so Task 7's tool dispatcher can surface actionable
/// diagnostics to IPC clients without parsing the `message` string.
#[test]
fn workspace_stale_expired_error_data_contains_last_good_at_rfc3339() {
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_760_000_000);
    let err = DaemonError::WorkspaceStaleExpired {
        root: PathBuf::from("/repos/x"),
        age_hours: 48,
        cap_hours: 24,
        last_good_at: Some(t),
        last_error: Some("index corrupt".into()),
    };
    let data = err.error_data().expect("stale_expired has error_data");
    assert_eq!(data["age_hours"], 48);
    assert_eq!(data["cap_hours"], 24);
    assert_eq!(data["last_error"], "index corrupt");
    // RFC3339 / UTC-Zulu — starts with year, ends with 'Z'.
    let rendered = data["last_good_at"].as_str().expect("rfc3339 string");
    assert!(rendered.ends_with('Z'), "must be UTC-Zulu: {rendered}");
    // 2025-10-09T07:33:20Z or similar — exact precision depends on the formatter.
    assert!(
        rendered.starts_with("202"),
        "must start with 20xx year: {rendered}"
    );
}
