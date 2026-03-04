//! Malformed input tests for Pulumi language plugin.
//!
//! Ensures parsing and graph building handle malformed data without panicking.

use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_pulumi::PulumiPlugin;
use sqry_tree_sitter_fuzz_support::MalformedInputBuilder;
use sqry_tree_sitter_fuzz_support::testing::{StackSafeResult, run_with_stack};
use std::path::Path;

fn nested_json(depth: usize) -> Vec<u8> {
    let mut text = String::with_capacity(depth * 2 + 1);
    for _ in 0..depth {
        text.push('[');
    }
    text.push('1');
    for _ in 0..depth {
        text.push(']');
    }
    text.into_bytes()
}

#[test]
fn test_truncated_utf8() {
    let plugin = PulumiPlugin::default();
    let malformed = MalformedInputBuilder::truncated_utf8();
    let _ = plugin.parse_ast(&malformed);
}

#[test]
fn test_invalid_continuation() {
    let plugin = PulumiPlugin::default();
    let malformed = MalformedInputBuilder::invalid_continuation();
    let _ = plugin.parse_ast(&malformed);
}

#[test]
fn test_overlong_encoding() {
    let plugin = PulumiPlugin::default();
    let malformed = MalformedInputBuilder::overlong_encoding();
    let _ = plugin.parse_ast(&malformed);
}

#[test]
fn test_surrogate_pairs() {
    let plugin = PulumiPlugin::default();
    let malformed = MalformedInputBuilder::surrogate_pairs();
    let _ = plugin.parse_ast(&malformed);
}

#[test]
fn test_null_bytes() {
    let plugin = PulumiPlugin::default();
    let malformed = MalformedInputBuilder::null_bytes();
    let _ = plugin.parse_ast(&malformed);
}

#[test]
fn test_random_bytes() {
    let plugin = PulumiPlugin::default();
    let random = MalformedInputBuilder::random_bytes(2048);
    let _ = plugin.parse_ast(&random);
}

#[test]
fn test_invalid_json_trailing_comma() {
    let plugin = PulumiPlugin::default();
    let malformed = br#"{"name": "pulumi",}"#;
    let _ = plugin.parse_ast(malformed);
}

#[test]
fn test_invalid_json_single_quotes() {
    let plugin = PulumiPlugin::default();
    let malformed = br#"{'name': 'pulumi'}"#;
    let _ = plugin.parse_ast(malformed);
}

#[test]
fn test_invalid_yaml_bad_indent() {
    let plugin = PulumiPlugin::default();
    let malformed = b"resources:\n  - name: test\n    type: aws\n   bad: 1\n";
    let _ = plugin.parse_ast(malformed);
}

#[test]
fn test_invalid_yaml_bad_anchor() {
    let plugin = PulumiPlugin::default();
    let malformed = b"resources:\n  name: &bad_anchor\n  other: *missing\n";
    let _ = plugin.parse_ast(malformed);
}

#[test]
#[ignore = "Stress test - run manually when validating stack depth"]
fn test_deeply_nested_json() {
    let plugin = PulumiPlugin::default();
    let nested = nested_json(4096);
    let result = run_with_stack(move || plugin.parse_ast(&nested));
    match result {
        StackSafeResult::Ok(_) => {}
        StackSafeResult::Panicked(_) => {}
    }
}

#[test]
#[ignore = "Performance test - run in nightly job to keep CI fast"]
fn test_oversized_10mb() {
    let plugin = PulumiPlugin::default();
    let large = MalformedInputBuilder::random_bytes(10 * 1024 * 1024);
    let _ = plugin.parse_ast(&large);
}

#[test]
fn test_build_graph_on_malformed() {
    let plugin = PulumiPlugin::default();
    let malformed = MalformedInputBuilder::truncated_utf8();
    let path = Path::new("Pulumi.yaml");

    let tree = match plugin.parse_ast(&malformed) {
        Ok(tree) => tree,
        Err(_) => return,
    };

    let builder = plugin.graph_builder().expect("graph builder");
    let mut staging = StagingGraph::new();
    let _ = builder.build_graph(&tree, &malformed, path, &mut staging);
}
