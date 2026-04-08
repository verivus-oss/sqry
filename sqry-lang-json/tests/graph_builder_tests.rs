mod common;

use common::{build_staging_from_source, count_nodes, has_edge_between, has_node, node_kind_for};
use serial_test::serial;
use sqry_core::graph::unified::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;

// ─── Generic Profile: Node Extraction ───────────────────────────────────────

#[test]
fn test_empty_object() {
    let staging = build_staging_from_source(b"{}", "config.json");
    assert_eq!(count_nodes(&staging), 1);
    assert!(has_node(&staging, "<module>"));
}

#[test]
fn test_empty_array() {
    let staging = build_staging_from_source(b"[]", "config.json");
    assert_eq!(count_nodes(&staging), 1);
    assert!(has_node(&staging, "<module>"));
}

#[test]
fn test_flat_object() {
    let source = br#"{"name": "sqry", "version": "1.0"}"#;
    let staging = build_staging_from_source(source, "config.json");

    assert_eq!(count_nodes(&staging), 3);
    assert!(has_node(&staging, "name"));
    assert!(has_node(&staging, "version"));
    assert_eq!(node_kind_for(&staging, "name"), Some(NodeKind::Variable));
}

#[test]
fn test_nested_objects() {
    let source = br#"{"a": {"b": {"c": 1}}}"#;
    let staging = build_staging_from_source(source, "config.json");

    assert!(has_node(&staging, "a"));
    assert!(has_node(&staging, "a::b"));
    assert!(has_node(&staging, "a::b::c"));

    assert!(has_edge_between(
        &staging,
        "<module>",
        "a",
        |kind| matches!(kind, EdgeKind::Defines)
    ));
    assert!(has_edge_between(&staging, "a", "a::b", |kind| matches!(
        kind,
        EdgeKind::Contains
    )));
    assert!(has_edge_between(
        &staging,
        "a::b",
        "a::b::c",
        |kind| matches!(kind, EdgeKind::Contains)
    ));
}

#[test]
fn test_array_of_scalars() {
    let source = br#"{"items": ["a", "b", "c"]}"#;
    let staging = build_staging_from_source(source, "config.json");

    assert!(has_node(&staging, "items"));
    assert!(has_node(&staging, "items::[0]"));
    assert!(has_node(&staging, "items::[1]"));
    assert!(has_node(&staging, "items::[2]"));

    assert!(has_edge_between(
        &staging,
        "items",
        "items::[0]",
        |kind| matches!(kind, EdgeKind::Contains)
    ));
}

#[test]
fn test_array_of_objects() {
    let source = br#"{"list": [{"id": 1}, {"id": 2}]}"#;
    let staging = build_staging_from_source(source, "config.json");

    assert!(has_node(&staging, "list::[0]"));
    assert!(has_node(&staging, "list::[0]::id"));
    assert!(has_node(&staging, "list::[1]"));
    assert!(has_node(&staging, "list::[1]::id"));
}

#[test]
fn test_keys_with_dots() {
    // JSON key "my.key" should be escaped to "my\.key" in the qualified name.
    // The canonical resolver converts "." to "::" but escaped "\." should be
    // preserved. However, the current canonicalization replaces ALL dots with "::".
    // So "my\.key" becomes "my\::key" in canonical form.
    let source = br#"{"my.key": "value"}"#;
    let staging = build_staging_from_source(source, "config.json");

    // Verify the node exists (canonical name may vary due to separator conversion)
    // At minimum, the graph should have 2 nodes: <module> + the key
    assert_eq!(count_nodes(&staging), 2);
}

#[test]
fn test_keys_with_backslash() {
    let source = br#"{"back\\slash": "value"}"#;
    let staging = build_staging_from_source(source, "config.json");

    // The JSON key is "back\slash" (single backslash after JSON decode),
    // which gets escaped to "back\\slash" in qualified name
    assert_eq!(count_nodes(&staging), 2);
}

#[test]
fn test_duplicate_keys() {
    let source = br#"{"a": 1, "a": 2}"#;
    let staging = build_staging_from_source(source, "config.json");

    assert!(has_node(&staging, "a"));
}

#[test]
fn test_unicode_keys() {
    // Keys with actual unicode characters (not \u escapes) are preserved
    let source = "{\"\u{00e9}l\u{00e8}ve\": \"student\"}".as_bytes();
    let staging = build_staging_from_source(source, "config.json");

    assert!(has_node(&staging, "\u{00e9}l\u{00e8}ve"));
}

#[test]
fn test_unicode_escape_keys() {
    // Keys with \uXXXX escapes should be decoded to actual unicode.
    // JSON source: {"\u00e9l\u00e8ve": "student"}
    let source = b"{\"\\u00e9l\\u00e8ve\": \"student\"}";
    let staging = build_staging_from_source(source, "config.json");
    assert!(has_node(&staging, "\u{00e9}l\u{00e8}ve"));
}

#[test]
fn test_unicode_escape_malformed() {
    // Malformed \uXXXX (non-hex digit 'G') should produce U+FFFD replacement char.
    // decode_unicode_escape reads '1','2','G' → None (G consumed), '4' remains.
    // Result: U+FFFD then '4' literal.
    let source = b"{\"\\u12G4\": \"bad\"}";
    let staging = build_staging_from_source(source, "config.json");
    // The key should contain the replacement character followed by remaining chars
    assert!(has_node(&staging, "\u{FFFD}4"));
}

#[test]
fn test_unicode_escape_surrogate_pair() {
    // U+1F600 (😀) encoded as UTF-16 surrogate pair: \uD83D\uDE00
    let source = b"{\"\\uD83D\\uDE00\": \"grinning\"}";
    let staging = build_staging_from_source(source, "config.json");
    assert!(has_node(&staging, "\u{1F600}"));
}

#[test]
fn test_non_object_root_scalar() {
    // Bare scalar JSON roots (42, "hello", true, null) are rejected by the
    // fast pre-check because they don't start with `{` or `[`.  These have
    // no keys or structure, so they carry zero semantic value for search.
    let staging = build_staging_from_source(b"42", "config.json");
    assert_eq!(count_nodes(&staging), 0);

    let staging = build_staging_from_source(b"\"hello\"", "config.json");
    assert_eq!(count_nodes(&staging), 0);

    let staging = build_staging_from_source(b"true", "config.json");
    assert_eq!(count_nodes(&staging), 0);

    let staging = build_staging_from_source(b"null", "config.json");
    assert_eq!(count_nodes(&staging), 0);
}

#[test]
fn test_malformed_json() {
    let source = br#"{"a": 1 "b": 2}"#;
    let staging = build_staging_from_source(source, "config.json");
    assert!(count_nodes(&staging) >= 1);
}

// ─── Edge Verification ──────────────────────────────────────────────────────

#[test]
fn test_top_level_gets_defines_edge() {
    let source = br#"{"key": "value"}"#;
    let staging = build_staging_from_source(source, "config.json");

    assert!(has_edge_between(
        &staging,
        "<module>",
        "key",
        |kind| matches!(kind, EdgeKind::Defines)
    ));
}

#[test]
fn test_nested_gets_contains_edge() {
    let source = br#"{"parent": {"child": 1}}"#;
    let staging = build_staging_from_source(source, "config.json");

    assert!(has_edge_between(
        &staging,
        "parent",
        "parent::child",
        |kind| matches!(kind, EdgeKind::Contains)
    ));
}

// ─── Exclusion Tests ────────────────────────────────────────────────────────

#[test]
fn test_excluded_lockfile() {
    let source = br#"{"name": "test"}"#;
    let staging = build_staging_from_source(source, "package-lock.json");
    assert_eq!(count_nodes(&staging), 0);
}

#[test]
fn test_excluded_minified() {
    let source = br#"{"name": "test"}"#;
    let staging = build_staging_from_source(source, "bundle.min.json");
    assert_eq!(count_nodes(&staging), 0);
}

#[test]
fn test_excluded_shrinkwrap() {
    let source = br#"{"name": "test"}"#;
    let staging = build_staging_from_source(source, "shrinkwrap.json");
    assert_eq!(count_nodes(&staging), 0);
}

#[test]
fn test_not_excluded_normal_file() {
    let source = br#"{"name": "test"}"#;
    let staging = build_staging_from_source(source, "data.json");
    assert!(count_nodes(&staging) > 0);
}

// ─── now-ui.json Profile ────────────────────────────────────────────────────

#[test]
fn test_now_ui_component_nodes() {
    let source = br#"{
        "components": {
            "snc-my-component": {
                "properties": {
                    "label": {"fieldType": "string"}
                },
                "actions": {
                    "ITEM_CLICKED": {"description": "clicked"}
                }
            }
        }
    }"#;
    let staging = build_staging_from_source(source, "now-ui.json");

    assert_eq!(
        node_kind_for(&staging, "components::snc-my-component"),
        Some(NodeKind::Component)
    );

    assert_eq!(
        node_kind_for(&staging, "components::snc-my-component::properties"),
        Some(NodeKind::Variable)
    );
    assert_eq!(
        node_kind_for(&staging, "components::snc-my-component::properties::label"),
        Some(NodeKind::Variable)
    );

    assert_eq!(
        node_kind_for(&staging, "components"),
        Some(NodeKind::Variable)
    );
}

// ─── package.json Profile ───────────────────────────────────────────────────

#[test]
fn test_package_json_import_nodes() {
    let source = br#"{
        "name": "my-app",
        "dependencies": {
            "express": "^4.18.0"
        },
        "devDependencies": {
            "jest": "^29.0.0"
        },
        "scripts": {
            "build": "tsc"
        }
    }"#;
    let staging = build_staging_from_source(source, "package.json");

    assert_eq!(
        node_kind_for(&staging, "dependencies::express"),
        Some(NodeKind::Import)
    );
    assert_eq!(
        node_kind_for(&staging, "devDependencies::jest"),
        Some(NodeKind::Import)
    );
    assert_eq!(
        node_kind_for(&staging, "scripts::build"),
        Some(NodeKind::Variable)
    );
    assert_eq!(node_kind_for(&staging, "name"), Some(NodeKind::Variable));

    assert!(has_edge_between(
        &staging,
        "<module>",
        "dependencies::express",
        |kind| matches!(kind, EdgeKind::Imports { .. })
    ));
    assert!(has_edge_between(
        &staging,
        "<module>",
        "devDependencies::jest",
        |kind| matches!(kind, EdgeKind::Imports { .. })
    ));
}

// ─── Mixed Nesting ──────────────────────────────────────────────────────────

#[test]
fn test_mixed_nesting_qualified_paths() {
    let source = br#"{"a": {"b": [{"c": 1}]}}"#;
    let staging = build_staging_from_source(source, "config.json");

    assert!(has_node(&staging, "a"));
    assert!(has_node(&staging, "a::b"));
    assert!(has_node(&staging, "a::b::[0]"));
    assert!(has_node(&staging, "a::b::[0]::c"));
}

// ─── All Value Types ────────────────────────────────────────────────────────

#[test]
fn test_all_value_types() {
    let source = br#"{
        "str": "hello",
        "num": 42,
        "float": 3.14,
        "bool_true": true,
        "bool_false": false,
        "null_val": null,
        "obj": {},
        "arr": []
    }"#;
    let staging = build_staging_from_source(source, "config.json");

    assert!(has_node(&staging, "str"));
    assert!(has_node(&staging, "num"));
    assert!(has_node(&staging, "float"));
    assert!(has_node(&staging, "bool_true"));
    assert!(has_node(&staging, "bool_false"));
    assert!(has_node(&staging, "null_val"));
    assert!(has_node(&staging, "obj"));
    assert!(has_node(&staging, "arr"));
}

// ─── Safety Limit Tests ──────────────────────────────────────────────────────

#[test]
#[serial]
fn test_max_depth_truncation() {
    // Set a low depth limit to make the test fast and deterministic.
    unsafe {
        std::env::set_var("SQRY_JSON_MAX_DEPTH", "8");
    }

    // Build JSON nested 12 levels deep (exceeds limit of 8).
    let depth = 12_usize;
    let mut json = String::new();
    for i in 0..depth {
        #[allow(clippy::format_push_string)] // Test output formatting
        json.push_str(&format!("{{\"d{i}\": "));
    }
    json.push('1');
    for _ in 0..depth {
        json.push('}');
    }

    let staging = build_staging_from_source(json.as_bytes(), "deep.json");

    unsafe {
        std::env::remove_var("SQRY_JSON_MAX_DEPTH");
    }

    // Boundary check: d0 (depth 0) through d7 (depth 7) should exist.
    // Build the expected qualified name for the last included level.
    let mut last_included = String::from("d0");
    assert!(has_node(&staging, &last_included), "d0 should exist");
    for i in 1..8_usize {
        last_included = format!("{last_included}::d{i}");
    }
    assert!(
        has_node(&staging, &last_included),
        "depth 7 node should exist: {last_included}"
    );

    // First excluded level (depth 8) should NOT exist as a nested node.
    let first_excluded = format!("{last_included}::d8");
    assert!(
        !has_node(&staging, &first_excluded),
        "depth 8 node should NOT exist: {first_excluded}"
    );
}

#[test]
#[serial]
#[allow(clippy::format_push_string)] // String building in test/doc
fn test_max_nodes_limit() {
    // Use SQRY_JSON_MAX_NODES env var to set a low limit for testing.
    // Build 200 top-level keys × 200 sub-keys = 40,200 potential nodes.
    // With limit set to 1,000, verify the graph caps.
    unsafe {
        std::env::set_var("SQRY_JSON_MAX_NODES", "1000");
    }

    let mut json = String::from("{");
    for i in 0..200 {
        if i > 0 {
            json.push(',');
        }
        #[allow(clippy::format_push_string)] // String formatting in test/doc builder
        json.push_str(&format!("\"g{i}\": {{"));
        for j in 0..200 {
            if j > 0 {
                json.push(',');
            }
            json.push_str(&format!("\"s{j}\": {j}"));
        }
        json.push('}');
    }
    json.push('}');

    let staging = build_staging_from_source(json.as_bytes(), "huge.json");
    let total = count_nodes(&staging);

    unsafe {
        std::env::remove_var("SQRY_JSON_MAX_NODES");
    }

    // module + up to 1,000 nodes from the walk
    assert!(
        total <= 1_001,
        "expected at most 1,001 nodes (module + limit), got {total}"
    );
    // Should have hit the cap — far fewer than the 40,201 potential
    assert!(total < 40_201, "node limit did not cap output, got {total}");
}
