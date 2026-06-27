//! Issue #394 item 4: the `relative` redaction preset renders legible
//! workspace-relative paths (no anonymizing `<source_root_id>` /
//! `<workspace_id_short>` prefix) while preserving the absolute-host-path strip
//! and the other redactions. These tests pin the AC1-AC5 contract.

use std::path::PathBuf;

use sqry_mcp_redaction::{LogicalWorkspaceView, RedactionConfig, Redactor, compute_source_root_id};

fn ws_id_short() -> String {
    "0123456789abcdef".to_string()
}

fn view_with(
    source_root: PathBuf,
    member_folders: Vec<PathBuf>,
    exclusions: Vec<PathBuf>,
) -> LogicalWorkspaceView {
    let workspace_id_short = ws_id_short();
    let id = compute_source_root_id(&workspace_id_short, &source_root);
    LogicalWorkspaceView {
        workspace_id_short,
        source_roots: vec![(id, source_root)],
        member_folders,
        exclusions,
    }
}

fn rendered_uri(config: RedactionConfig, uri: &str) -> String {
    let redactor = Redactor::new(config).expect("relative-config redactor");
    let mut response = serde_json::json!({ "fileUri": uri });
    redactor.redact(&mut response);
    response["fileUri"].as_str().unwrap().to_string()
}

#[test]
fn relative_preset_emits_clean_relative_for_source_root() {
    // AC1: in-workspace source-root path renders as the clean relative remainder,
    // with NO source_root_id hex prefix.
    let source_root = PathBuf::from("/home/user/proj");
    let id = compute_source_root_id(&ws_id_short(), &source_root);
    let mut config = RedactionConfig::relative();
    config.logical_workspace = Some(view_with(source_root, Vec::new(), Vec::new()));

    let rendered = rendered_uri(config, "file:///home/user/proj/kernel/time.rs");

    assert_eq!(
        rendered, "kernel/time.rs",
        "relative preset must drop the prefix"
    );
    assert!(!rendered.contains(&id), "no source_root_id hex prefix");
    assert!(
        !rendered.contains("/home/user"),
        "AC2: no absolute host path"
    );
}

#[test]
fn relative_preset_emits_clean_relative_for_member_folder() {
    // AC1: member-folder path renders as the clean relative remainder, no
    // workspace_id_short prefix.
    let source_root = PathBuf::from("/home/user/proj");
    let member_folder = PathBuf::from("/home/user/scripts");
    let mut config = RedactionConfig::relative();
    config.logical_workspace = Some(view_with(source_root, vec![member_folder], Vec::new()));

    let rendered = rendered_uri(config, "file:///home/user/scripts/build.sh");

    assert_eq!(rendered, "build.sh");
    assert!(
        !rendered.contains(&ws_id_short()),
        "no workspace_id_short prefix"
    );
    assert!(
        !rendered.contains("/home/user"),
        "AC2: no absolute host path"
    );
}

#[test]
fn relative_preset_external_path_stays_safe_basename() {
    // AC2: a genuinely-external path still renders as `<external>/<basename>`;
    // revealing its directory tail would leak the host layout, so the relative
    // preset deliberately does NOT change external rendering.
    let source_root = PathBuf::from("/home/user/proj");
    let mut config = RedactionConfig::relative();
    config.logical_workspace = Some(view_with(source_root, Vec::new(), Vec::new()));

    let rendered = rendered_uri(config, "file:///opt/other/crate/src/lib.rs");

    assert_eq!(
        rendered, "<external>/lib.rs",
        "external stays basename-only"
    );
    assert!(
        !rendered.contains("/opt/other"),
        "AC2: no external host dir leaked"
    );
}

#[test]
fn relative_preset_excluded_path_still_anonymized() {
    // AC2/AC5: excluded paths remain opaque-hashed even under `relative`.
    let source_root = PathBuf::from("/home/user/proj");
    let secret = PathBuf::from("/home/user/proj/secrets");
    let mut config = RedactionConfig::relative();
    config.logical_workspace = Some(view_with(source_root, Vec::new(), vec![secret]));

    let rendered = rendered_uri(config, "file:///home/user/proj/secrets/api_keys.toml");

    assert!(
        rendered.starts_with("<excluded>/["),
        "excluded path must stay opaque-hashed, got `{rendered}`"
    );
    assert!(
        !rendered.contains("api_keys"),
        "excluded basename must not leak"
    );
    assert!(!rendered.contains("/home/user"), "no absolute host path");
}

#[test]
fn relative_preset_config_shape_preserves_other_redactions() {
    // AC3/AC4: `relative` keeps minimal's posture except the reveal flag.
    let cfg = RedactionConfig::relative();
    assert!(cfg.reveal_workspace_relative_layout, "reveal flag set");
    assert!(cfg.redact_absolute_paths, "absolute-path strip preserved");
    assert!(!cfg.hash_filenames, "no filename hashing");
    assert!(!cfg.redact_code_context, "code preserved (like minimal)");
    assert!(!cfg.redact_documentation, "docs preserved (like minimal)");

    // The defaults and other presets keep the anonymizing behaviour.
    assert!(!RedactionConfig::minimal().reveal_workspace_relative_layout);
    assert!(!RedactionConfig::standard().reveal_workspace_relative_layout);
    assert!(!RedactionConfig::strict().reveal_workspace_relative_layout);
    assert!(!RedactionConfig::none().reveal_workspace_relative_layout);
}

#[test]
fn minimal_preset_unchanged_keeps_hex_prefix() {
    // AC3: with the default (non-relative) minimal preset, the source_root_id
    // prefix is still emitted (no behaviour change for existing presets).
    let source_root = PathBuf::from("/home/user/proj");
    let id = compute_source_root_id(&ws_id_short(), &source_root);
    let mut config = RedactionConfig::minimal();
    config.logical_workspace = Some(view_with(source_root, Vec::new(), Vec::new()));

    let rendered = rendered_uri(config, "file:///home/user/proj/kernel/time.rs");

    assert!(
        rendered.starts_with(&format!("{id}/")),
        "minimal keeps hex prefix"
    );
    assert!(rendered.ends_with("kernel/time.rs"));
}

#[test]
fn strict_with_reveal_flag_still_hashes_prefix_token() {
    // Security: even if `reveal_workspace_relative_layout` is forced on under a
    // hashing preset, the hash branch wins, so the cleartext relative path never
    // escapes. (`strict` hashes the whole prefixed token.)
    let source_root = PathBuf::from("/home/user/proj");
    let mut config = RedactionConfig::strict();
    config.reveal_workspace_relative_layout = true;
    config.logical_workspace = Some(view_with(source_root, Vec::new(), Vec::new()));

    let rendered = rendered_uri(config, "file:///home/user/proj/kernel/time.rs");

    assert!(
        rendered.starts_with("<redacted>/["),
        "strict must still hash even with reveal on, got `{rendered}`"
    );
    assert!(
        !rendered.contains("kernel/time.rs"),
        "cleartext path must not escape"
    );
    assert!(!rendered.contains("/home/user"), "no absolute host path");
}

#[test]
fn relative_preset_root_itself_renders_dot() {
    // The source-root path itself (empty relative remainder) renders as "." under
    // `relative`, never an empty string or an absolute path.
    let source_root = PathBuf::from("/home/user/proj");
    let mut config = RedactionConfig::relative();
    config.logical_workspace = Some(view_with(source_root, Vec::new(), Vec::new()));

    let rendered = rendered_uri(config, "file:///home/user/proj");

    assert_eq!(rendered, ".", "root itself must render as `.`");
    assert!(!rendered.contains("/home/user"), "no absolute host path");
}

#[test]
fn relative_matches_minimal_except_for_the_path_prefix() {
    // End-to-end equivalence: `relative` must behave exactly like `minimal` for
    // every field EXCEPT that in-workspace paths lose the anonymizing prefix.
    // This proves all non-path redaction (security mode, code/doc/URI handling,
    // excluded hashing, etc.) is preserved without modelling each rule here.
    let source_root = PathBuf::from("/home/user/proj");

    let response = || {
        serde_json::json!({
            "fileUri": "file:///home/user/proj/kernel/time.rs",
            "external": "file:///opt/other/crate/src/lib.rs",
            "code_context": "fn secret() { let key = 42; }",
            "documentation": "Some docs here.",
            "nested": { "uri": "file:///home/user/proj/drivers/x.rs" }
        })
    };

    let mut min_resp = response();
    let mut min_cfg = RedactionConfig::minimal();
    min_cfg.logical_workspace = Some(view_with(source_root.clone(), Vec::new(), Vec::new()));
    Redactor::new(min_cfg)
        .expect("minimal redactor")
        .redact(&mut min_resp);

    let mut rel_resp = response();
    let mut rel_cfg = RedactionConfig::relative();
    rel_cfg.logical_workspace = Some(view_with(source_root, Vec::new(), Vec::new()));
    Redactor::new(rel_cfg)
        .expect("relative redactor")
        .redact(&mut rel_resp);

    // The in-workspace path fields differ only by the dropped prefix.
    assert_eq!(rel_resp["fileUri"].as_str().unwrap(), "kernel/time.rs");
    assert_eq!(rel_resp["nested"]["uri"].as_str().unwrap(), "drivers/x.rs");
    assert_ne!(min_resp["fileUri"], rel_resp["fileUri"]);

    // Everything else is byte-identical between the two presets.
    assert_eq!(
        min_resp["external"], rel_resp["external"],
        "external identical"
    );
    assert_eq!(
        min_resp["code_context"], rel_resp["code_context"],
        "code identical"
    );
    assert_eq!(
        min_resp["documentation"], rel_resp["documentation"],
        "docs identical"
    );
    // No absolute host path under relative anywhere.
    assert!(!rel_resp.to_string().contains("/home/user"));
}
