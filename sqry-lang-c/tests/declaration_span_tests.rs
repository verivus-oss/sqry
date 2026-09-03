//! Regression coverage for issue #725: a DECLARATION must report the line it is
//! written on, not line 1 with its byte offset sitting in the column.
//!
//! Reverting this language's emission site to `Span::from_bytes` survived the
//! whole package suite during review. This test is what makes that mutation die.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use std::collections::HashMap;
use std::path::Path;

/// `(name, start_line, start_column)` for every staged node.
fn staged_nodes(source: &str) -> Vec<(String, u32, u32)> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .expect("grammar");
    let tree = parser.parse(source, None).expect("parse");

    let mut staging = StagingGraph::new();
    sqry_lang_c::CGraphBuilder::default()
        .build_graph(&tree, source.as_bytes(), Path::new("p.c"), &mut staging)
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

/// The declaration is placed well down the file so its byte offset could not be
/// mistaken for a plausible line or column.
#[test]
fn declaration_reports_its_real_line_not_line_one() {
    let source = "// pad\n// pad\n// pad\n// pad\nint main(void) {\n    int answer = 42;\n    return answer;\n}\n";

    let nodes = staged_nodes(source);
    let hit = nodes
        .iter()
        .find(|(name, ..)| name.contains("answer"))
        .unwrap_or_else(|| panic!("no node matching `answer` in {nodes:?}"));

    assert_eq!(
        hit.1, 6,
        "`answer` is declared on line 6, got line {} col {} (col {} would be its byte offset)",
        hit.1, hit.2, hit.2
    );
    assert_ne!(
        hit.1, 1,
        "line 1 with the byte offset in the column is the issue #725 signature"
    );
}
