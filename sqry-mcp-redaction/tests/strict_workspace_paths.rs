//! Strict-mode workspace-aware redaction tests — `STEP_7` acceptance criterion 7.
//!
//! Criterion 7 (verbatim from the DAG):
//!
//! > preset=strict + any workspace-relative or source-root-relative form
//! > → end-to-end hashed; cleartext `source_root_id` / `workspace_id_short`
//! > NEVER appears in any strict-mode response (test enforces).
//!
//! The strict pipeline is the load-bearing security surface. The hashed
//! token must cover the whole `<workspace_prefix>/<rel>` form so neither
//! the workspace identity nor the per-source-root id leaks through to
//! the wire. Each test below enforces a slice of that invariant by
//! exercising the redactor end-to-end and asserting the rendered output
//! against an explicit set of substrings that MUST NOT appear in any
//! strict-mode response.

use std::path::PathBuf;

use serde_json::json;
use sqry_mcp_redaction::{LogicalWorkspaceView, RedactionConfig, Redactor, compute_source_root_id};

const WORKSPACE_ID_SHORT: &str = "0123456789abcdef";

fn source_root_id_for(path: &std::path::Path) -> String {
    compute_source_root_id(WORKSPACE_ID_SHORT, path)
}

fn build_view(source_root: PathBuf, member_folders: Vec<PathBuf>) -> LogicalWorkspaceView {
    LogicalWorkspaceView {
        workspace_id_short: WORKSPACE_ID_SHORT.to_string(),
        source_roots: vec![(source_root_id_for(&source_root), source_root)],
        member_folders,
        exclusions: Vec::new(),
    }
}

fn strict_redactor(view: LogicalWorkspaceView) -> Redactor {
    Redactor::with_logical_workspace(RedactionConfig::strict(), view)
        .expect("strict-config redactor must construct")
}

#[test]
fn strict_preset_hashes_source_root_prefix_end_to_end() {
    // The strict redactor MUST replace a source-root-relative path with
    // an opaque hash token. The pre-hash workspace prefix
    // (<source_root_id>/<rel>) must NOT survive in the rendered output —
    // it is the SHA-256 input only.
    let source_root = PathBuf::from("/home/user/proj");
    let view = build_view(source_root, Vec::new());
    let source_root_id = view.source_roots[0].0.clone();
    let redactor = strict_redactor(view);

    let mut response = json!({
        "fileUri": "file:///home/user/proj/src/main.rs",
        "filePath": "/home/user/proj/lib/api.rs"
    });
    redactor.redact(&mut response);

    let serialized = serde_json::to_string(&response).expect("serialize redacted");

    // Strict tokens render through the `<redacted>/[<8-hex>]` path —
    // the hashed prefix MUST cover both source_root_id and rel, so the
    // cleartext source_root_id never appears.
    assert!(
        !serialized.contains(&source_root_id),
        "strict mode must NOT leak source_root_id `{source_root_id}` in cleartext, got `{serialized}`"
    );
    assert!(
        !serialized.contains("/home/user/proj"),
        "strict mode must NOT leak the workspace path"
    );
    // Sanity: the response was actually transformed.
    assert!(serialized.contains("<redacted>"));
}

#[test]
fn strict_preset_hashes_workspace_id_short_prefix_end_to_end() {
    // Same as above but for the member-folder path (criterion 5
    // composed with criterion 7): the workspace_id_short is folded into
    // the SHA-256 input but never escapes in cleartext.
    let source_root = PathBuf::from("/home/user/proj");
    let scripts = PathBuf::from("/home/user/scripts");
    let view = build_view(source_root, vec![scripts]);
    let redactor = strict_redactor(view);

    let mut response = json!({
        "fileUri": "file:///home/user/scripts/build.sh"
    });
    redactor.redact(&mut response);

    let serialized = serde_json::to_string(&response).expect("serialize redacted");

    assert!(
        !serialized.contains(WORKSPACE_ID_SHORT),
        "strict mode must NOT leak workspace_id_short `{WORKSPACE_ID_SHORT}` in cleartext, got `{serialized}`"
    );
    assert!(
        !serialized.contains("/home/user/scripts"),
        "strict mode must NOT leak the member folder path"
    );
    assert!(!serialized.contains("build.sh"));
    assert!(serialized.contains("<redacted>"));
}

#[test]
fn strict_preset_no_cleartext_source_root_id_in_any_response() {
    // Robustness sweep: drive the redactor through a representative MCP
    // response covering every path-bearing field shape (fileUri,
    // filePath, location, workspace_path, free-form pattern strings)
    // and assert the source_root_id never escapes in cleartext.
    let source_root = PathBuf::from("/home/user/proj");
    let view = build_view(source_root, Vec::new());
    let source_root_id = view.source_roots[0].0.clone();
    let redactor = strict_redactor(view);

    let mut response = json!({
        "results": [
            {
                "name": "main",
                "fileUri": "file:///home/user/proj/src/main.rs",
                "filePath": "/home/user/proj/src/main.rs",
                "location": {
                    "path": "/home/user/proj/src/main.rs",
                    "uri": "file:///home/user/proj/src/main.rs"
                }
            }
        ],
        "workspace_path": "/home/user/proj",
        "diagnostics": "Resolved at /home/user/proj/src/main.rs"
    });
    redactor.redact(&mut response);

    let serialized = serde_json::to_string(&response).expect("serialize redacted");

    assert!(
        !serialized.contains(&source_root_id),
        "source_root_id `{source_root_id}` MUST NOT appear in any strict-mode response.\nResponse: {serialized}"
    );
}

#[test]
fn strict_preset_no_cleartext_workspace_id_short_in_any_response() {
    // Member-folder-aware variant of the sweep: every path-bearing
    // field that resolves into the member folder must hash through the
    // `<workspace_id_short>/<rel>` form, so the workspace_id_short
    // never leaks in cleartext.
    let source_root = PathBuf::from("/home/user/proj");
    let scripts = PathBuf::from("/home/user/scripts");
    let view = build_view(source_root, vec![scripts]);
    let redactor = strict_redactor(view);

    let mut response = json!({
        "results": [
            {
                "fileUri": "file:///home/user/scripts/build.sh",
                "filePath": "/home/user/scripts/lint.sh",
                "location": {
                    "uri": "file:///home/user/scripts/release.sh",
                    "path": "/home/user/scripts/test.sh"
                }
            }
        ],
        "workspace_path": "/home/user/scripts",
        "log_message": "Running /home/user/scripts/build.sh"
    });
    redactor.redact(&mut response);

    let serialized = serde_json::to_string(&response).expect("serialize redacted");

    assert!(
        !serialized.contains(WORKSPACE_ID_SHORT),
        "workspace_id_short `{WORKSPACE_ID_SHORT}` MUST NOT appear in any strict-mode response.\nResponse: {serialized}"
    );
    assert!(
        !serialized.contains("/home/user/scripts"),
        "member folder path MUST NOT leak through.\nResponse: {serialized}"
    );
}
