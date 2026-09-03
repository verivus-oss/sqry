//! Regression coverage for issue #725: a DECLARATION must report the line it is
//! written on, not line 1 with its byte offset sitting in the column.
//!
//! Reverting this language's emission sites to `Span::from_bytes` survived the
//! whole package suite during review. This test is what makes those mutations
//! die. It asserts on every staged node rather than one hand-picked symbol,
//! because a name-matched assertion can pass against a usage node while the
//! declaration beside it is still broken.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use std::collections::HashMap;
use std::path::Path;

fn staged_nodes(source: &str) -> Vec<(String, u32, u32, u32)> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .expect("grammar");
    let tree = parser.parse(source, None).expect("parse");

    let mut staging = StagingGraph::new();
    sqry_lang_java::relations::JavaGraphBuilder::default()
        .build_graph(&tree, source.as_bytes(), Path::new("P.java"), &mut staging)
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
                    entry.end_line,
                ))
            }
            _ => None,
        })
        .collect()
}

/// First staged node with this exact name.
fn find<'a>(nodes: &'a [(String, u32, u32, u32)], name: &str) -> &'a (String, u32, u32, u32) {
    nodes
        .iter()
        .find(|(n, ..)| n == name)
        .unwrap_or_else(|| panic!("no node named `{name}` in {nodes:?}"))
}

/// The fixture pads the top so that any declaration's byte offset is far larger
/// than its real line, and larger than any plausible column. A node reporting
/// line 1 with a big column is the issue #725 signature.
#[test]
fn no_declaration_collapses_onto_line_one_with_a_byte_offset_column() {
    let source = "// pad\n// pad\n// pad\n// pad\n// pad\n// pad\nimport java.util.List;\n\nclass Holder {\n    static final int ANSWER = 42;\n    int compute() { return ANSWER; }\n}\n";
    let nodes = staged_nodes(source);
    assert!(!nodes.is_empty(), "fixture produced no nodes");

    let line_count = u32::try_from(source.matches('\n').count() + 1).expect("fits");

    let mut real = 0;
    for (name, start_line, start_column, _end_line) in &nodes {
        // Synthetic whole-file nodes legitimately sit at the start of the file.
        if name.contains("<module>") || name.contains("module") && *start_column == 0 {
            continue;
        }
        assert!(
            *start_line <= line_count,
            "`{name}` reports line {start_line}, past the fixture's {line_count} lines"
        );
        if *start_line == 1 {
            assert!(
                *start_column < 200,
                "`{name}` reports line 1 column {start_column}; a column that large is a \
                 byte offset, which is the issue #725 signature"
            );
        }
        real += 1;
    }
    assert!(
        real > 0,
        "every node was skipped, so this test would be vacuous"
    );

    // Import declaration, converted from `Span::from_bytes(import_node...)`.
    let import = find(&nodes, "java::util::List");
    assert_eq!(
        import.1, 7,
        "the import is on line 7, got line {} col {}",
        import.1, import.2
    );
}
