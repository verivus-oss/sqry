//! Regression coverage for the synthetic whole-file context, issue #725's class.
//!
//! Three separate paths in this plugin build a synthetic `<module>` context when
//! a call has no enclosing callable: a regular call, a constructor call, and the
//! FFI caller lookup. All three reach a mint through `ensure_caller_node`.
//!
//! Whichever of them runs FIRST creates the node and its span stands: a later
//! `ensure_callee` for the same name returns a cache hit without touching
//! metadata, so nothing widens it afterwards. An earlier revision of this header
//! said `apply_span_to_entry` had nothing to lose to, which described a
//! mechanism that does not exist. A reviewer measured the real one, and
//! `sqry-lang-python`'s `the_first_mint_decides_the_module_context_span` pins
//! both orderings.
//!
//! Two properties are pinned together, because fixing either one alone breaks
//! the other:
//!
//! 1. The node is POSITIONED: line 1, column 0. That is the honest position for
//!    module-level code, and it is what makes the node navigable.
//! 2. The node's span stays DEGENERATE (it ends where it starts), which is what
//!    `has_valid_body_span` uses to keep a non-body out of the body-hash and
//!    shape planes.
//!
//! An earlier revision of this branch widened these spans to cover the whole
//! file. That reads better and is wrong: three reviewers measured `<module>`
//! gaining a `body_hash` and a shape descriptor, and it then surfaced in
//! `sqry shape-match` as a 0.812 structural neighbour of a real function. A
//! whole-file pseudo-body is not a body.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use std::collections::HashMap;
use std::path::Path;

/// Build the fixture, attach body hashes exactly as the index pipeline does,
/// and assert both halves of the invariant.
fn assert_module_context_positioned_and_unhashed(source: &str, what: &str) {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .expect("grammar");
    let tree = parser.parse(source, None).expect("parse");

    let mut staging = StagingGraph::new();
    sqry_lang_javascript::JavaScriptGraphBuilder::default()
        .build_graph(&tree, source.as_bytes(), Path::new("m.js"), &mut staging)
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
                "{what}: synthetic node `{name}` carries a body hash. It is not a function \
                 body, and hashing it puts it into duplicate groups and shape-match \
                 alongside real functions."
            );
            // Nodes staged with no span at all report line 0. That is the
            // separate `None`-span class, not this one.
            if name == "<module>" && entry.start_line != 0 {
                saw_positioned_module = true;
                assert_eq!(
                    (entry.start_line, entry.start_column),
                    (1, 0),
                    "{what}: the `<module>` context must report line 1 column 0"
                );
                assert_eq!(
                    (entry.end_line, entry.end_column),
                    (1, 0),
                    "{what}: the `<module>` context span must stay degenerate. Widening it \
                     to the whole file is what let it into the body-hash plane."
                );
            }
        } else if entry.body_hash.is_some() {
            saw_real_declaration_hash = true;
        }
    }

    assert!(
        saw_positioned_module,
        "{what}: no positioned `<module>` node was staged, so this test is vacuous"
    );
    assert!(
        saw_real_declaration_hash,
        "{what}: no real declaration hashed, so the exclusion could be arbitrarily wide \
         and this test would not notice"
    );
}

/// A plain top-level call. No FFI involved, which is what makes this the widest
/// of the three paths.
#[test]
fn module_level_regular_call_is_positioned_and_unhashed() {
    assert_module_context_positioned_and_unhashed(
        "// pad\n// pad\nfunction helper() { return 1; }\nhelper();\n",
        "regular top-level call",
    );
}

/// A top-level `new` expression, which takes the constructor path.
#[test]
fn module_level_constructor_call_is_positioned_and_unhashed() {
    assert_module_context_positioned_and_unhashed(
        "// pad\n// pad\nfunction helper() { return 1; }\nclass Widget {}\nnew Widget();\n",
        "top-level constructor call",
    );
}

/// A top-level `require`, which takes the FFI caller path.
#[test]
fn module_level_ffi_caller_is_positioned_and_unhashed() {
    assert_module_context_positioned_and_unhashed(
        "// pad\n// pad\nfunction helper() { return 1; }\nconst addon = require(\"./addon.node\");\n",
        "top-level require",
    );
}
