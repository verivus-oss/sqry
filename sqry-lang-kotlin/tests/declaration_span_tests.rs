//! Regression coverage for issue #725: a DECLARATION must report the line it is
//! written on, not line 1 with its byte offset sitting in the column.
//!
//! Reverting this language's emission site to `Span::from_bytes` survived the
//! whole package suite during review. This test is what makes that mutation die.
//!
//! The assertion is deliberately offset-derived rather than a hardcoded line:
//! every node this builder names `symbol@<byte-offset>` must report the line
//! that offset actually falls on, so the test cannot pass by matching a usage
//! node when the declaration is the broken one.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use std::collections::HashMap;
use std::path::Path;

fn staged_nodes(source: &str) -> Vec<(String, u32, u32)> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_kotlin_sqry::language())
        .expect("grammar");
    let tree = parser.parse(source, None).expect("parse");

    let mut staging = StagingGraph::new();
    sqry_lang_kotlin::relations::KotlinGraphBuilder
        .build_graph(&tree, source.as_bytes(), Path::new("P.kt"), &mut staging)
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
            StagingOp::AddNode { entry, .. } => {
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

/// 1-indexed line of a byte offset, derived from the fixture text.
fn line_of(source: &str, offset: usize) -> u32 {
    u32::try_from(source[..offset.min(source.len())].matches('\n').count() + 1)
        .expect("line fits in u32")
}

#[test]
fn every_offset_named_node_reports_its_real_line() {
    let source = "// pad\n// pad\n// pad\n// pad\nfun compute(): Int {\n    val answer = 42\n    return answer\n}\n";
    let nodes = staged_nodes(source);

    let mut checked = 0;
    for (name, start_line, start_column) in &nodes {
        let Some(offset) = name
            .rsplit('@')
            .next()
            .and_then(|s| s.parse::<usize>().ok())
        else {
            continue;
        };
        checked += 1;
        assert_eq!(
            *start_line,
            line_of(source, offset),
            "`{name}` must report the line its byte offset falls on, got line {start_line} \
             column {start_column} (a column equal to the offset is the issue #725 signature)"
        );
    }
    assert!(
        checked > 0,
        "fixture produced no `symbol@offset` node, so this test would be vacuous: {nodes:?}"
    );
}
