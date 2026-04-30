//! Regression tests for the `sqry::ambiguous_symbol` envelope under MCP
//! redaction.
//!
//! These tests pin the contract documented in the README and the DOCS unit of
//! the BadLiveware go-batch DAG: under the default `minimal` redaction preset,
//! `file_path` in each `AmbiguousSymbolCandidate` is rewritten to a
//! workspace-relative form so the absolute repository path (home directory,
//! machine layout, etc.) never leaks, while `qualified_name`, `kind`,
//! `start_line`, and `start_column` flow through unchanged. No additional
//! fields (in particular no `source_excerpt` / `source` / `code` / `context`)
//! are permitted to appear in the redacted envelope.
//!
//! The shape under test mirrors `sqry_cli::commands::impact::AmbiguousSymbolEnvelope`
//! and the wire response produced by `sqry-mcp` for the `dependency_impact`
//! tool when the bare symbol resolves to multiple nodes.

use serde_json::{Value, json};
use sqry_mcp_redaction::{RedactionConfig, Redactor};
use std::path::PathBuf;

fn minimal_with_workspace() -> RedactionConfig {
    RedactionConfig {
        workspace_root: Some(PathBuf::from("/home/user/project")),
        ..RedactionConfig::minimal()
    }
}

fn standard_with_workspace() -> RedactionConfig {
    RedactionConfig {
        workspace_root: Some(PathBuf::from("/home/user/project")),
        ..RedactionConfig::standard()
    }
}

fn make_envelope() -> Value {
    json!({
        "error": {
            "code": "sqry::ambiguous_symbol",
            "message": "Symbol 'NeedTags' is ambiguous; specify the qualified name",
            "candidates": [
                {
                    "qualified_name": "main.SelectorSource.NeedTags",
                    "kind": "property",
                    "file_path": "/home/user/project/main.go",
                    "start_line": 12,
                    "start_column": 4
                },
                {
                    "qualified_name": "main.useSelector.NeedTags",
                    "kind": "variable",
                    "file_path": "/home/user/project/main.go",
                    "start_line": 30,
                    "start_column": 6
                }
            ],
            "truncated": false
        }
    })
}

#[test]
fn minimal_preset_redacts_candidate_file_paths() {
    let mut envelope = make_envelope();
    let redactor = Redactor::new(minimal_with_workspace()).unwrap();
    let stats = redactor.redact(&mut envelope);

    assert!(
        stats.paths_redacted >= 2,
        "expected both candidate file_path values redacted, got stats={stats:?}"
    );

    let candidates = envelope["error"]["candidates"]
        .as_array()
        .expect("candidates array survives redaction");
    assert_eq!(candidates.len(), 2);

    for candidate in candidates {
        let path = candidate["file_path"]
            .as_str()
            .expect("file_path is still a string after redaction");
        assert!(
            !path.contains("/home/user"),
            "absolute home-directory path leaked: {path}"
        );
        assert!(
            !path.starts_with('/'),
            "path was not rewritten to a workspace-relative form: {path}"
        );
        assert!(
            candidate["qualified_name"]
                .as_str()
                .unwrap()
                .starts_with("main.")
        );
        assert!(matches!(
            candidate["kind"].as_str(),
            Some("property" | "variable")
        ));
        assert!(candidate["start_line"].is_number());
        assert!(candidate["start_column"].is_number());
    }

    let serialized = serde_json::to_string(&envelope).unwrap();
    assert!(
        !serialized.contains("/home/user"),
        "absolute repository path leaked into redacted envelope: {serialized}"
    );
}

#[test]
fn minimal_preset_preserves_envelope_shell_fields() {
    let mut envelope = make_envelope();
    let redactor = Redactor::new(minimal_with_workspace()).unwrap();
    redactor.redact(&mut envelope);

    let error = &envelope["error"];
    assert_eq!(error["code"], "sqry::ambiguous_symbol");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("Symbol 'NeedTags'")
    );
    assert_eq!(error["truncated"], false);
}

#[test]
fn minimal_preset_does_not_introduce_source_or_code_fields() {
    let mut envelope = make_envelope();
    let redactor = Redactor::new(minimal_with_workspace()).unwrap();
    redactor.redact(&mut envelope);

    let candidates = envelope["error"]["candidates"].as_array().unwrap();
    for candidate in candidates {
        let obj = candidate.as_object().expect("candidate is an object");
        for forbidden in [
            "source",
            "source_excerpt",
            "sourceExcerpt",
            "code",
            "code_snippet",
            "codeSnippet",
            "snippet",
            "context",
            "code_context",
            "codeContext",
            "source_code",
            "sourceCode",
        ] {
            assert!(
                !obj.contains_key(forbidden),
                "candidate gained forbidden field `{forbidden}` after redaction: {candidate}"
            );
        }
    }
}

#[test]
fn standard_preset_also_redacts_candidate_file_paths() {
    // The DAG specifies behavior under `minimal` (the default), but the
    // stricter `standard` preset must not regress: file_path is still
    // sensitive there too.
    let mut envelope = make_envelope();
    let redactor = Redactor::new(standard_with_workspace()).unwrap();
    let stats = redactor.redact(&mut envelope);

    assert!(stats.paths_redacted >= 2);
    let candidates = envelope["error"]["candidates"].as_array().unwrap();
    for candidate in candidates {
        let path = candidate["file_path"].as_str().unwrap();
        assert!(!path.contains("/home/user"));
        assert!(!path.starts_with('/'));
    }
}

#[test]
fn none_preset_leaves_envelope_intact() {
    // Sanity check: `none` (passthrough) should leave the envelope untouched
    // - the test itself verifies that the redaction layer is the only thing
    // doing the file_path stripping.
    let mut envelope = make_envelope();
    let redactor = Redactor::new(RedactionConfig::none()).unwrap();
    redactor.redact(&mut envelope);

    let candidates = envelope["error"]["candidates"].as_array().unwrap();
    for candidate in candidates {
        let path = candidate["file_path"].as_str().unwrap();
        assert!(path.starts_with("/home/user/project/"));
        assert!(path.ends_with("main.go"));
    }
}
