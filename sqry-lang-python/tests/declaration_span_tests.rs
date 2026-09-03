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
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("grammar");
    let tree = parser.parse(source, None).expect("parse");

    let mut staging = StagingGraph::new();
    sqry_lang_python::relations::PythonGraphBuilder::default()
        .build_graph(&tree, source.as_bytes(), Path::new("p.py"), &mut staging)
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
    let source = "# pad\n# pad\n# pad\n# pad\ndef compute():\n    answer = 42\n    return answer\n";
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

/// The declaration and each usage must be SEPARATE nodes, and the declaration
/// must keep the declaration's line.
///
/// Python was the outlier here. It named the usage node after the
/// DECLARATION's offset, so usage and declaration shared a qualified name,
/// collapsed into one node, and the survivor reported the usage's position
/// while holding a reference edge to itself. JavaScript and Rust already
/// emitted one node per occurrence. The test above cannot see this: every node
/// it checks reports the line of the offset in its own name, and a collapsed
/// node named after the declaration still does.
///
/// This is also what the `sqry index --help` disclosure rests on. Splitting
/// these grew Python indexes (+88% nodes on the python3.13 asyncio tree) and
/// more than doubled Python's `sqry unused` row count, and the claim that those rows are
/// not a new KIND of result is only true while Python matches the shape the
/// other languages already had.
#[test]
fn a_local_declaration_and_its_usages_are_distinct_nodes() {
    let source = "def compute(seed):\n    # pad\n    total = seed\n    # pad\n    total = total + 1\n    return total\n";
    let declaration_offset = source.find("total = seed").expect("fixture declares total");
    let declaration_line = line_of(source, declaration_offset);
    let nodes = staged_nodes(source);

    // Identified by NAME and POSITION, not by an offset parsed out of the name.
    // The declaration publishes the bare identifier; only occurrence nodes
    // carry the binding-site suffix, because publishing that suffix as the
    // node address leaked it into planner, MCP and LSP output.
    let declarations: Vec<_> = nodes.iter().filter(|(n, _, _)| n == "total").collect();
    let occurrences: Vec<_> = nodes
        .iter()
        .filter(|(n, _, _)| n.starts_with("total@"))
        .collect();

    assert_eq!(
        declarations.len(),
        1,
        "exactly one declaration node named `total` is expected: {nodes:?}"
    );
    assert!(
        !occurrences.is_empty(),
        "declaration and usages collapsed into one node: {nodes:?}"
    );
    assert_eq!(
        declarations[0].1, declaration_line,
        "the declaration node must report the declaration's line, not a usage's: {nodes:?}"
    );
    for (name, start_line, _) in &occurrences {
        assert_ne!(
            *start_line, declaration_line,
            "occurrence `{name}` reports the declaration's line, so they collapsed: {nodes:?}"
        );
    }
}
