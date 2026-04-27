//! STEP_6 (workspace-aware-cross-repo, 2026-04-26) — `WorkspaceKey`
//! wire-format compatibility tests.
//!
//! Asserts the three round-trip properties documented in the STEP_6
//! brief:
//!
//! 1. The frozen v1 JSON fixture deserialises with `workspace_id =
//!    None` and `source_root = <old index_root>`.
//! 2. Re-serialising the upconverted shape into the new wire form and
//!    parsing back into a `WorkspaceKey` yields the same value (Eq).
//! 3. The on-disk persisted-state file v1 → v2 upconverter runs once
//!    on load and produces a v2 [`PersistedState`] whose every key
//!    carries `workspace_id = None`.

use std::path::PathBuf;

use sqry_core::project::ProjectRootMode;
use sqry_daemon::workspace::{PersistedState, WorkspaceKey, parse_persisted_state};

const V1_FIXTURE: &str = include_str!("fixtures/workspace_key_v1.json");

#[test]
fn workspace_key_v1_fixture_deserialises_with_workspace_id_none() {
    // Pull the first key out of the fixture's `keys` array, drive it
    // through `WorkspaceKey`'s Deserialize impl directly. The legacy
    // `index_root` field name MUST be accepted via
    // `#[serde(alias = "index_root")]` and `workspace_id` MUST default
    // to `None` via `#[serde(default)]`.
    let raw: serde_json::Value = serde_json::from_str(V1_FIXTURE).expect("v1 fixture parses");
    let v1_first_key = &raw["keys"][0];

    let key: WorkspaceKey =
        serde_json::from_value(v1_first_key.clone()).expect("v1 key deserialises into v2 shape");

    assert!(
        key.workspace_id.is_none(),
        "v1 fixtures must round-trip with workspace_id = None"
    );
    assert_eq!(
        key.source_root,
        PathBuf::from("/repos/example"),
        "v1 `index_root` field name must alias into `source_root`"
    );
    assert_eq!(key.root_mode, ProjectRootMode::GitRoot);
    assert_eq!(key.config_fingerprint, 0x12345678);
}

#[test]
fn workspace_key_v1_fixture_round_trips_to_v2_format() {
    // Acceptance criterion: re-serialise the upconverted key in the
    // new format, then parse it again — must yield Eq.
    let raw: serde_json::Value = serde_json::from_str(V1_FIXTURE).expect("v1 fixture parses");
    let v1_first_key = &raw["keys"][0];
    let key: WorkspaceKey =
        serde_json::from_value(v1_first_key.clone()).expect("v1 key deserialises");

    // New wire form: `workspace_id` is omitted via
    // `skip_serializing_if = "Option::is_none"`, and the field name on
    // the wire is `source_root` (the rename target).
    let new_wire = serde_json::to_string(&key).expect("serialise v2");
    assert!(
        new_wire.contains(r#""source_root":"/repos/example""#),
        "v2 wire form must use `source_root`: {new_wire}"
    );
    assert!(
        !new_wire.contains("workspace_id"),
        "anonymous v2 wire form must omit `workspace_id`: {new_wire}"
    );
    assert!(
        !new_wire.contains("index_root"),
        "v2 wire form must NOT emit the legacy field name: {new_wire}"
    );

    // Parse the v2 wire form back; the new `Deserialize` impl accepts it.
    let back: WorkspaceKey = serde_json::from_str(&new_wire).expect("v2 wire form parses back");
    assert_eq!(back, key, "v1 → v2 → v1 round-trip preserves identity");
}

#[test]
fn workspace_key_eq_after_round_trip() {
    // A freshly-constructed key with the same fields compares Eq with
    // the round-tripped one — i.e. v1 deserialisation truly produces
    // the canonical v2 shape, not a near-equivalent.
    let constructed = WorkspaceKey::new(
        PathBuf::from("/repos/example"),
        ProjectRootMode::GitRoot,
        0x12345678,
    );

    let raw: serde_json::Value = serde_json::from_str(V1_FIXTURE).expect("v1 fixture parses");
    let v1_first_key = &raw["keys"][0];
    let upconverted: WorkspaceKey =
        serde_json::from_value(v1_first_key.clone()).expect("v1 key deserialises");

    assert_eq!(
        upconverted, constructed,
        "upconverted v1 fixture must Eq a freshly-constructed v2 key"
    );
}

#[test]
fn persisted_state_v1_to_v2_upconverter_runs_once() {
    // STEP_6 brief acceptance criterion #9 — the on-load upconverter
    // exercises the v1 → v2 path on a fixture and produces a
    // PersistedState whose `format_version` is exactly v2.
    let upconverted =
        parse_persisted_state(V1_FIXTURE.as_bytes()).expect("v1 persisted-state upconverts");

    assert_eq!(
        upconverted.format_version,
        PersistedState::FORMAT_VERSION,
        "upconverter must produce v2",
    );
    assert_eq!(upconverted.keys.len(), 2, "fixture has two v1 keys");

    for key in &upconverted.keys {
        assert!(
            key.workspace_id.is_none(),
            "every v1-upconverted key must carry workspace_id = None"
        );
    }

    // First key matches the fixture's first entry.
    assert_eq!(
        upconverted.keys[0].source_root,
        PathBuf::from("/repos/example")
    );
    assert_eq!(upconverted.keys[0].root_mode, ProjectRootMode::GitRoot);
    assert_eq!(upconverted.keys[0].config_fingerprint, 0x12345678);

    // Second key matches the fixture's second entry.
    assert_eq!(
        upconverted.keys[1].source_root,
        PathBuf::from("/repos/another")
    );
    assert_eq!(
        upconverted.keys[1].root_mode,
        ProjectRootMode::WorkspaceFolder
    );
    assert_eq!(upconverted.keys[1].config_fingerprint, 0);
}
