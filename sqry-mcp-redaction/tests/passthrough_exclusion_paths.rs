//! Passthrough-mode exclusion tests — `STEP_7` acceptance criterion 6.
//!
//! `preset=any + path in exclusions → opaque hash + excluded: true` is the
//! literal DAG criterion. The `minimal` and `strict` halves are covered in
//! `tests/workspace_aware_paths.rs::excluded_path_emits_opaque_hash_with_excluded_flag`
//! (which exercises [`sqry_mcp_redaction::rules::path::redact_path_with_workspace`]
//! across the two non-passthrough presets).
//!
//! This file closes the `preset=none` half by driving the actual JSON
//! walker (the surface every MCP response flows through) under
//! `RedactionConfig::none()` with a bound `LogicalWorkspaceView`. The
//! codex iter1 BLOCK observed that `SecurityMode::Passthrough` short-
//! circuited `should_redact_field`, leaking excluded paths in cleartext.
//! The fix in `walker.rs::handle_object_field` adds a passthrough-with-
//! exclusion branch (`redact_excluded_in_passthrough`) and the tests
//! below assert:
//!
//! 1. Excluded path under `none` preset is rewritten to the opaque
//!    `<excluded>/[hash]` form.
//! 2. Non-excluded path under `none` preset (criterion 3) is preserved
//!    verbatim — the fix does not regress the criterion the BLOCK
//!    explicitly enumerated as still passing.
//! 3. Nested path-bearing object structure under `none` preset has its
//!    excluded child rewritten while leaving the rest intact.

use std::path::PathBuf;

use sqry_mcp_redaction::{LogicalWorkspaceView, RedactionConfig, Redactor, compute_source_root_id};

fn ws_id_short() -> String {
    "0123456789abcdef".to_string()
}

fn make_view_with_exclusion(
    source_root: PathBuf,
    excluded: PathBuf,
    workspace_id_short: String,
) -> LogicalWorkspaceView {
    let id = compute_source_root_id(&workspace_id_short, &source_root);
    LogicalWorkspaceView {
        workspace_id_short,
        source_roots: vec![(id, source_root)],
        member_folders: Vec::new(),
        exclusions: vec![excluded],
    }
}

#[test]
fn none_preset_redacts_excluded_path_to_opaque_hash() {
    // Acceptance criterion 6 (passthrough half): with a bound
    // LogicalWorkspaceView, the `none` preset MUST still rewrite an
    // excluded path to the opaque-hash form. This is the regression
    // codex iter1 flagged: `SecurityMode::Passthrough` short-circuited
    // `should_redact_field`, leaking cleartext.
    let source_root = PathBuf::from("/home/user/proj");
    let excluded = PathBuf::from("/home/user/proj/secrets");
    let view = make_view_with_exclusion(source_root, excluded, ws_id_short());

    let mut config = RedactionConfig::none();
    config.logical_workspace = Some(view);
    let redactor = Redactor::new(config).expect("none-config redactor with workspace");

    let mut response = serde_json::json!({
        "fileUri": "file:///home/user/proj/secrets/api_keys.toml"
    });
    let stats = redactor.redact(&mut response);

    let rendered = response["fileUri"]
        .as_str()
        .expect("fileUri must remain a string");
    assert!(
        rendered.starts_with("<excluded>/["),
        "none preset must rewrite excluded paths to `<excluded>/[hash]`, got `{rendered}`"
    );
    assert!(
        !rendered.contains("api_keys"),
        "filename leaf must not survive in cleartext, got `{rendered}`"
    );
    assert!(
        !rendered.contains("secrets"),
        "excluded directory name must not survive in cleartext, got `{rendered}`"
    );
    assert!(
        stats.any_redacted(),
        "passthrough+exclusion must record at least one redaction event"
    );
}

#[test]
fn none_preset_preserves_non_excluded_absolute_path() {
    // Regression guard for acceptance criterion 3 — the BLOCK fix MUST
    // not regress this. preset=none + path inside source_root → absolute
    // emitted. This asserts the passthrough-exclusion branch leaves
    // every other path verbatim.
    let source_root = PathBuf::from("/home/user/proj");
    let excluded = PathBuf::from("/home/user/proj/secrets");
    let view = make_view_with_exclusion(source_root, excluded, ws_id_short());

    let mut config = RedactionConfig::none();
    config.logical_workspace = Some(view);
    let redactor = Redactor::new(config).expect("none-config redactor with workspace");

    let mut response = serde_json::json!({
        "fileUri": "file:///home/user/proj/src/main.rs"
    });
    let stats = redactor.redact(&mut response);

    assert!(
        !stats.any_redacted(),
        "non-excluded passthrough path must record no redactions"
    );
    assert_eq!(
        response["fileUri"].as_str().unwrap(),
        "file:///home/user/proj/src/main.rs",
        "non-excluded path must survive verbatim under none+exclusion"
    );
}

#[test]
fn none_preset_redacts_nested_excluded_path_in_array() {
    // Multi-result MCP responses bury fileUri inside arrays. Ensure the
    // passthrough-exclusion branch reaches nested path-bearing string
    // values even when the outer container is an array of objects.
    let source_root = PathBuf::from("/home/user/proj");
    let excluded = PathBuf::from("/home/user/proj/secrets");
    let view = make_view_with_exclusion(source_root, excluded, ws_id_short());

    let mut config = RedactionConfig::none();
    config.logical_workspace = Some(view);
    let redactor = Redactor::new(config).expect("none-config redactor with workspace");

    let mut response = serde_json::json!({
        "results": [
            {"fileUri": "file:///home/user/proj/src/main.rs"},
            {"fileUri": "file:///home/user/proj/secrets/db.env"}
        ]
    });
    redactor.redact(&mut response);

    let r0 = response["results"][0]["fileUri"].as_str().unwrap();
    let r1 = response["results"][1]["fileUri"].as_str().unwrap();
    assert_eq!(
        r0, "file:///home/user/proj/src/main.rs",
        "non-excluded array element must survive verbatim, got `{r0}`"
    );
    assert!(
        r1.starts_with("<excluded>/["),
        "excluded array element must be rewritten, got `{r1}`"
    );
}

#[test]
fn none_preset_without_logical_workspace_remains_passthrough() {
    // Symmetry guard: when no LogicalWorkspaceView is bound the
    // passthrough-exclusion branch MUST remain inert — the original
    // `none` preset semantics still apply.
    let config = RedactionConfig::none();
    let redactor = Redactor::new(config).expect("plain none redactor");

    let mut response = serde_json::json!({
        "fileUri": "file:///home/user/proj/secrets/api_keys.toml"
    });
    let stats = redactor.redact(&mut response);

    assert!(
        !stats.any_redacted(),
        "plain none preset must record zero redactions"
    );
    assert_eq!(
        response["fileUri"].as_str().unwrap(),
        "file:///home/user/proj/secrets/api_keys.toml",
        "plain none preset must leave path verbatim"
    );
}
