//! Regression coverage for the synthetic whole-file context, issue #725's class.
//!
//! `build_call_edge_ids` builds a synthetic `<module>` context for a top-level
//! expression, and at the `else` arm below it that span becomes the node's only
//! span, so nothing later widens over it:
//!
//! ```ignore
//! let source_id = if helper.has_node(&call_context.qualified_name()) { ... } else {
//!     let span = call_context.decl_span;
//!     helper.add_function(&call_context.qualified_name(), Some(span), false, false)
//! };
//! ```
//!
//! Two reviewers independently found this site had no oracle, each by probing a
//! top-level call. The invariant pinned here is the same one the javascript and
//! typescript oracles pin: positioned at line 1 column 0, span degenerate, and
//! therefore out of the body-hash and shape planes.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use std::collections::HashMap;
use std::path::Path;

#[test]
fn module_level_call_is_positioned_and_unhashed() {
    let source = "// pad\n// pad\n// pad\nfn helper() u8 { return 1; }\nconst v = helper();\n";

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_zig::LANGUAGE.into())
        .expect("grammar");
    let tree = parser.parse(source, None).expect("parse");

    let mut staging = StagingGraph::new();
    sqry_lang_zig::ZigGraphBuilder::default()
        .build_graph(&tree, source.as_bytes(), Path::new("m.zig"), &mut staging)
        .expect("build graph");
    staging.attach_body_hashes(source.as_bytes(), None);

    let strings: HashMap<u32, String> = staging
        .operations()
        .iter()
        .filter_map(|op| match op {
            StagingOp::InternString { local_id, value } => Some((local_id.index(), value.clone())),
            _ => None,
        })
        .collect();

    let mut saw_positioned_module = false;
    let mut saw_real_declaration_hash = false;

    for op in staging.operations() {
        let StagingOp::AddNode { entry, .. } = op else {
            continue;
        };
        let name = strings
            .get(&entry.qualified_name.unwrap_or(entry.name).index())
            .cloned()
            .unwrap_or_default();

        if name.starts_with('<') {
            assert!(
                entry.body_hash.is_none(),
                "synthetic node `{name}` carries a body hash; it is not a function body"
            );
            if name == "<module>" && entry.start_line != 0 {
                saw_positioned_module = true;
                assert_eq!(
                    (entry.start_line, entry.start_column),
                    (1, 0),
                    "the `<module>` context must report line 1 column 0"
                );
                assert_eq!(
                    (entry.end_line, entry.end_column),
                    (1, 0),
                    "the `<module>` context span must stay degenerate, or it enters the \
                     body-hash and shape planes"
                );
            }
        } else if entry.body_hash.is_some() {
            saw_real_declaration_hash = true;
        }
    }

    assert!(
        saw_positioned_module,
        "a top-level call must stage a positioned `<module>` node"
    );
    assert!(
        saw_real_declaration_hash,
        "no real declaration hashed, so this test would not notice an over-wide exclusion"
    );
}
