//! Regression coverage for issue #725: local binding *declarations* reported
//! line 1 with the byte offset sitting in the column, while their usages
//! reported correct positions.
//!
//! PR #742 fixed function and class declarations. It did not fix local
//! bindings, because those are recorded as byte offsets and the tree-sitter
//! node is gone by the time the graph node is minted. These tests pin the
//! declaration side specifically, since that is the half that regressed twice.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::node::NodeKind;
use sqry_lang_javascript::JavaScriptGraphBuilder;
use std::collections::HashMap;
use std::path::Path;

/// Every `Variable` node, as `(name, start_line, start_column)`.
fn variable_nodes(source: &str) -> Vec<(String, u32, u32)> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .expect("javascript grammar");
    let tree = parser.parse(source, None).expect("parse");

    let mut staging = StagingGraph::new();
    JavaScriptGraphBuilder::default()
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("lib/util.js"),
            &mut staging,
        )
        .expect("build graph");

    let strings: HashMap<u32, String> = staging
        .operations()
        .iter()
        .filter_map(|op| match op {
            StagingOp::InternString { local_id, value } => Some((local_id.index(), value.clone())),
            _ => None,
        })
        .collect();

    staging
        .operations()
        .iter()
        .filter_map(|op| match op {
            StagingOp::AddNode { entry, .. } if entry.kind == NodeKind::Variable => {
                let id = entry.qualified_name.unwrap_or(entry.name).index();
                Some((
                    strings.get(&id).cloned().unwrap_or_default(),
                    entry.start_line,
                    entry.start_column,
                ))
            }
            _ => None,
        })
        .collect()
}

/// 1-indexed line of a byte offset, derived from the fixture text so the
/// expectation cannot drift away from the source it describes.
fn line_of(source: &str, offset: usize) -> u32 {
    u32::try_from(source[..offset].matches('\n').count() + 1).expect("line fits in u32")
}

/// Issue #725's own repro file. Every `name@offset` node must report the line
/// its offset actually falls on, declarations included.
#[test]
fn issue_725_repro_reports_real_lines_for_declarations_and_usages() {
    let source = "const os = require('os');\n\nfunction alpha(a) {\n  return os.tmpdir() + a;\n}\n\nfunction beta(b) {\n  return alpha(b);\n}\n\nmodule.exports = { alpha, beta };\n";

    let nodes = variable_nodes(source);
    assert!(!nodes.is_empty(), "expected Variable nodes");

    for (name, start_line, _) in &nodes {
        let Some(offset) = name
            .rsplit('@')
            .next()
            .and_then(|s| s.parse::<usize>().ok())
        else {
            continue;
        };
        assert_eq!(
            *start_line,
            line_of(source, offset),
            "{name} must report the line its byte offset falls on"
        );
    }

    // The two parameter declarations named in the issue, pinned by name so a
    // regression cannot pass by simply dropping the nodes.
    let line_for = |n: &str| {
        nodes
            .iter()
            .find(|(name, ..)| name == n)
            .unwrap_or_else(|| panic!("{n} must be staged"))
            .1
    };
    assert_eq!(line_for("a@42"), 3, "parameter `a`");
    assert_eq!(line_for("b@90"), 7, "parameter `b`");
}

/// A declaration's column must be its column within its own line, not a byte
/// offset from the start of the file. Distinguishing the two needs a
/// declaration far enough down that the offset could not be a valid column.
#[test]
fn declaration_column_is_a_column_not_a_byte_offset() {
    let source = "// padding\n// padding\n// padding\nfunction f() {\n  const answer = 42;\n  return answer;\n}\n";

    let nodes = variable_nodes(source);
    let (name, line, column) = nodes
        .iter()
        .find(|(name, ..)| name.starts_with("answer@"))
        .cloned()
        .expect("answer declaration staged");

    let offset: u32 = name
        .rsplit('@')
        .next()
        .and_then(|s| s.parse().ok())
        .expect("offset suffix");

    assert_eq!(line, 5, "`answer` is declared on line 5");
    assert_eq!(
        column, 8,
        "column within the line, past the two-space indent"
    );
    assert_ne!(column, offset, "column must not be the file byte offset");
}
