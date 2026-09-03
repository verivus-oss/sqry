//! Regression coverage for the synthetic whole-file context, issue #725's class.
//!
//! A reviewer found this site by taking the hunt that broke the python case and
//! applying it here: look for a context with NO enclosing declaration, because
//! that is where the synthetic span is the only span the node ever gets and
//! nothing later widens over it.
//!
//! The invariant has two halves, and they hold each other in place. The node
//! must be POSITIONED at line 1 column 0, which is the honest position for
//! module-level code, and its span must stay DEGENERATE so `has_valid_body_span`
//! keeps a non-body out of the body-hash and shape planes. See the javascript
//! oracle for the measurement that produced this shape.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use std::collections::HashMap;
use std::path::Path;

#[test]
fn module_level_ffi_caller_is_positioned_and_unhashed() {
    // `process.dlopen` at module level: no enclosing callable, so the FFI path
    // takes the synthetic `<module>` branch of `get_caller_node_id`.
    let source = "// pad\n// pad\n// pad\n// pad\nfunction helper(): number { return 1; }\nconst m: any = {};\nprocess.dlopen(m, \"./addon.node\");\n";

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        .expect("grammar");
    let tree = parser.parse(source, None).expect("parse");

    let mut staging = StagingGraph::new();
    sqry_lang_typescript::relations::TypeScriptGraphBuilder::default()
        .build_graph(&tree, source.as_bytes(), Path::new("m.ts"), &mut staging)
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
        "a module-level ffi call must stage a positioned `<module>` caller node"
    );
    assert!(
        saw_real_declaration_hash,
        "no real declaration hashed, so this test would not notice an over-wide exclusion"
    );
}

/// A real function whose name happens to start with `<` must keep its body
/// hash.
///
/// An earlier revision excluded nodes from the body-hash plane by asking
/// `NodeEntry::is_synthetic_placeholder_name`, which is true for any
/// `<`-prefixed name. Measured over the fixture corpus, that stripped hashes
/// from this plugin's anonymous arrow functions and anonymous classes, and from
/// a Scala method literally named `<`. The name is not the question; whether
/// the node is a function body is.
///
/// Without this test, re-introducing that guard passes every other oracle here:
/// they only assert that `<module>` carries no hash, which a too-wide exclusion
/// also satisfies.
#[test]
fn an_angle_bracket_named_real_function_keeps_its_body_hash() {
    let source = "const handler = function (x: number): number {\n  if (x > 0) {\n    return x * 2;\n  }\n  return 0;\n};\nexport default handler;\n";

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        .expect("grammar");
    let tree = parser.parse(source, None).expect("parse");

    let mut staging = StagingGraph::new();
    sqry_lang_typescript::relations::TypeScriptGraphBuilder::default()
        .build_graph(&tree, source.as_bytes(), Path::new("anon.ts"), &mut staging)
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

    let angle_named: Vec<(String, bool)> = staging
        .operations()
        .iter()
        .filter_map(|op| match op {
            StagingOp::AddNode { entry, .. } => {
                let name = strings
                    .get(&entry.qualified_name.unwrap_or(entry.name).index())
                    .cloned()
                    .unwrap_or_default();
                (name.starts_with("<anon:")).then_some((name, entry.body_hash.is_some()))
            }
            _ => None,
        })
        .collect();

    assert!(
        !angle_named.is_empty(),
        "the fixture must stage an `<anon:...>` node, or this test is vacuous"
    );
    assert!(
        angle_named.iter().any(|(_, hashed)| *hashed),
        "every `<anon:...>` node lost its body hash: {angle_named:?}. A name-shaped \
         exclusion is too wide; these are real function bodies."
    );
}
