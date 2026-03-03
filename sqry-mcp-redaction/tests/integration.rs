//! Integration tests for sqry-mcp-redaction.

use serde_json::{Value, json};
use sqry_mcp_redaction::{RedactionConfig, Redactor};
use std::path::PathBuf;

#[test]
fn test_real_mcp_response() {
    let response = include_str!("fixtures/sample_responses/semantic_search.json");
    let mut json: Value = serde_json::from_str(response).unwrap();

    let config = RedactionConfig {
        workspace_root: Some(PathBuf::from("/home/user/project")),
        ..RedactionConfig::standard()
    };
    let redactor = Redactor::new(config).unwrap();
    let stats = redactor.redact(&mut json);

    // Verify statistics
    assert!(stats.workspace_path_redacted);
    assert!(stats.code_contexts_redacted > 0);

    // Verify no absolute paths remain
    let json_str = serde_json::to_string(&json).unwrap();
    assert!(!json_str.contains("file:///"));
    assert!(!json_str.contains("/home/user"));

    // Verify symbol names preserved
    assert!(json_str.contains("searchFunction"));
    assert!(json_str.contains("indexFile"));

    // Verify positions preserved
    let result = &json["result"]["results"][0];
    assert_eq!(result["location"]["start"]["line"], 42);
    assert_eq!(result["score"], 0.95);
}

#[test]
fn test_minimal_preset_preserves_code() {
    let config = RedactionConfig::minimal();
    let redactor = Redactor::new(config).unwrap();

    let mut response = json!({
        "results": [{
            "fileUri": "file:///home/user/project/src/main.rs",
            "context": {
                "code": "fn main() {\n    println!(\"Hello, world!\");\n}"
            }
        }],
        "workspace_path": "/home/user/project"
    });

    let stats = redactor.redact(&mut response);

    // Paths should be redacted
    assert!(stats.paths_redacted > 0 || stats.uris_redacted > 0);
    assert!(stats.workspace_path_redacted);

    // Code should be preserved
    assert_eq!(stats.code_contexts_redacted, 0);
    let code = response["results"][0]["context"]["code"].as_str().unwrap();
    assert!(code.contains("fn main()"));
}

#[test]
fn test_strict_preset_hashes_filenames() {
    let config = RedactionConfig {
        workspace_root: Some(PathBuf::from("/home/user/project")),
        ..RedactionConfig::strict()
    };
    let redactor = Redactor::new(config).unwrap();

    let mut response = json!({
        "fileUri": "file:///home/user/project/src/main.rs",
        "code": "fn main() {}"
    });

    let stats = redactor.redact(&mut response);

    // Everything should be redacted
    assert!(stats.paths_redacted > 0 || stats.uris_redacted > 0);
    assert!(stats.code_contexts_redacted > 0);

    // File URI should be hashed
    let uri = response["fileUri"].as_str().unwrap();
    assert!(uri.contains('['));
    assert!(!uri.contains("main.rs"));
}

#[test]
fn test_none_preset_passthrough() {
    let redactor = Redactor::new(RedactionConfig::none()).unwrap();

    let original = json!({
        "fileUri": "file:///home/user/project/src/main.rs",
        "workspace_path": "/home/user/project",
        "context": {
            "code": "fn main() { println!(\"Hello\"); }"
        },
        "documentation": "/// This is documentation"
    });

    let mut redacted = original.clone();
    let stats = redactor.redact(&mut redacted);

    // Nothing should be changed
    assert_eq!(original, redacted);
    assert!(!stats.any_redacted());
}

#[test]
fn test_windows_path_redaction() {
    let config = RedactionConfig {
        workspace_root: Some(PathBuf::from("C:\\Users\\john\\project")),
        ..RedactionConfig::standard()
    };
    let redactor = Redactor::new(config).unwrap();

    let mut json = json!({
        "fileUri": "file:///C:/Users/john/project/src/main.rs"
    });

    let stats = redactor.redact(&mut json);
    assert!(stats.paths_redacted > 0 || stats.uris_redacted > 0);

    let uri = json["fileUri"].as_str().unwrap();
    assert!(!uri.contains("C:"));
    assert!(!uri.contains("Users"));
}

#[test]
fn test_unc_path_redaction() {
    let redactor = Redactor::new(RedactionConfig::standard()).unwrap();

    let mut json = json!({
        "path": "\\\\server\\share\\project\\src\\main.rs"
    });

    let stats = redactor.redact(&mut json);
    assert!(stats.paths_redacted > 0);

    let path = json["path"].as_str().unwrap();
    assert!(!path.contains("server"));
    assert!(!path.contains("share"));
}

#[test]
fn test_jsonpath_nested_array() {
    let config = RedactionConfig {
        redact_paths: vec!["$..from.fileUri".to_string()],
        ..RedactionConfig::minimal()
    };
    let redactor = Redactor::new(config).unwrap();

    let mut json = json!({
        "results": [{
            "edges": [{
                "from": { "fileUri": "file:///home/user/src/a.rs" },
                "to": { "fileUri": "file:///home/user/src/b.rs" }
            }]
        }]
    });

    redactor.redact(&mut json);

    // "from.fileUri" should be redacted
    let from_uri = json["results"][0]["edges"][0]["from"]["fileUri"]
        .as_str()
        .unwrap();
    assert!(!from_uri.contains("file:///"));

    // "to.fileUri" should still have the original (in minimal mode, paths are redacted anyway)
    // But the JSONPath specifically targets "from"
}

#[test]
fn test_streaming_redaction() {
    let redactor = Redactor::new(RedactionConfig::standard()).unwrap();

    let input = r#"{"fileUri": "file:///home/user/project/src/main.rs", "name": "main"}"#;
    let mut output = Vec::new();

    let stats = redactor
        .redact_stream(input.as_bytes(), &mut output)
        .unwrap();

    assert!(stats.paths_redacted > 0 || stats.uris_redacted > 0);

    let result: Value = serde_json::from_slice(&output).unwrap();
    assert!(!result["fileUri"].as_str().unwrap().contains("file:///"));
    assert_eq!(result["name"], "main");
}

#[test]
fn test_dry_run_preview() {
    let redactor = Redactor::new(RedactionConfig::standard()).unwrap();

    let json = json!({
        "fileUri": "file:///home/user/project/src/main.rs",
        "name": "main",
        "workspace_path": "/home/user/project"
    });

    let preview = redactor.preview(&json);

    assert!(preview.would_redact_anything());
    assert!(preview.stats.workspace_path_redacted);

    // Check that we have preview targets
    assert!(preview.redaction_count() >= 1);
}

#[test]
fn test_pattern_detection_in_messages() {
    let config = RedactionConfig {
        workspace_root: Some(PathBuf::from("/home/user/project")),
        detect_paths_in_strings: true,
        ..RedactionConfig::minimal()
    };
    let redactor = Redactor::new(config).unwrap();

    let mut json = json!({
        "message": "Error at /home/user/project/src/main.rs:42 - syntax error",
        "level": "error"
    });

    let stats = redactor.redact(&mut json);
    assert!(stats.pattern_paths_redacted > 0);

    let message = json["message"].as_str().unwrap();
    assert!(!message.contains("/home/user/project"));
    // Should still contain the relative path part
    assert!(message.contains("src/main.rs"));
}

#[test]
fn test_custom_field_redaction() {
    let config = RedactionConfig {
        custom_redact_fields: vec!["secret_token".to_string()],
        ..RedactionConfig::minimal()
    };
    let redactor = Redactor::new(config).unwrap();

    let mut json = json!({
        "name": "test",
        "secret_token": "abc123xyz",
        "other_field": "visible"
    });

    redactor.redact(&mut json);

    assert_eq!(json["name"], "test");
    assert_eq!(json["secret_token"], "[REDACTED]");
    assert_eq!(json["other_field"], "visible");
}

#[test]
fn test_environment_variable_config() {
    // This test verifies from_env() works without actually setting env vars
    // (would need to use serial_test for that)
    let config = RedactionConfig::from_env();

    // Should default to standard preset
    assert!(config.redact_absolute_paths);
    assert!(config.redact_code_context);
}

#[test]
fn test_redact_clone_preserves_original() {
    let redactor = Redactor::with_defaults();

    let original = json!({
        "workspace_path": "/home/user/project",
        "name": "test"
    });

    let (redacted, stats) = redactor.redact_clone(&original);

    // Original unchanged
    assert_eq!(original["workspace_path"], "/home/user/project");

    // Clone redacted
    assert_eq!(redacted["workspace_path"], "<workspace>");
    assert!(stats.workspace_path_redacted);
}

#[test]
fn test_nested_code_context_redaction() {
    let redactor = Redactor::new(RedactionConfig::standard()).unwrap();

    let mut json = json!({
        "results": [{
            "name": "function",
            "context": {
                "code": "fn test() {}\nfn another() {}",
                "surrounding": {
                    "before": "// previous line",
                    "after": "// next line"
                }
            }
        }]
    });

    let stats = redactor.redact(&mut json);
    assert!(stats.code_contexts_redacted > 0);

    // When code_context is an object, the entire object is replaced with a placeholder string
    let context = json["results"][0]["context"].as_str().unwrap();
    assert!(context.contains("[REDACTED"));
}

#[test]
fn test_preserves_json_structure() {
    let redactor = Redactor::with_defaults();

    let mut json = json!({
        "array": [1, 2, 3],
        "nested": {
            "deeply": {
                "workspace_path": "/home/user/project"
            }
        },
        "null_field": null,
        "bool_field": true,
        "number_field": 42.5
    });

    redactor.redact(&mut json);

    // Structure preserved
    assert!(json["array"].is_array());
    assert!(json["nested"]["deeply"].is_object());
    assert!(json["null_field"].is_null());
    assert!(json["bool_field"].is_boolean());
    assert!(json["number_field"].is_number());

    // Workspace path redacted
    assert_eq!(json["nested"]["deeply"]["workspace_path"], "<workspace>");
}
