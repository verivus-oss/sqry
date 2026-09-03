//! Regression coverage for issue #725: a declaration must report the line it is
//! written on, not line 1 with its byte offset sitting in the column.
//!
//! The oracle is an invariant rather than a hardcoded position: EVERY staged
//! node's start column must fit inside the line it claims to be on. A span
//! built from `Span::from_bytes` puts a whole-file byte offset in the column
//! and pins the line to 1, so as soon as the declaration sits past the first
//! line the column cannot fit and the node is caught. That holds for every
//! mint in this plugin without naming any of them, which is what makes it
//! survive refactoring of the fixture.

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
        .build_graph(&tree, source.as_bytes(), Path::new("q.py"), &mut staging)
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

/// Every node's column must fit the line it reports. A byte-offset column
/// cannot, once the declaration is past line 1.
fn assert_columns_fit_their_lines(source: &str) {
    let lines: Vec<&str> = source.split('\n').collect();
    let nodes = staged_nodes(source);
    assert!(
        !nodes.is_empty(),
        "fixture staged no nodes, so this test would be vacuous"
    );

    let mut checked = 0;
    for (name, start_line, start_column) in &nodes {
        // Nodes with no span at all report line 0; they are a separate concern.
        if *start_line == 0 {
            continue;
        }
        let idx = usize::try_from(*start_line).expect("line fits") - 1;
        let line = lines.get(idx).unwrap_or_else(|| {
            panic!(
                "`{name}` reports line {start_line}, past the fixture's {} lines",
                lines.len()
            )
        });
        let width = u32::try_from(line.len()).expect("line length fits");
        assert!(
            *start_column <= width,
            "`{name}` reports line {start_line} column {start_column}, but that line is only \
             {width} bytes wide. A column that cannot fit its own line is a whole-file byte \
             offset, which is the issue #725 signature."
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "every node was skipped, so this test would be vacuous"
    );
}

/// The `CallContext` caller mint used by the FFI/call path.
#[test]
fn caller_context_mints_keep_columns_inside_their_lines() {
    assert_columns_fit_their_lines(
        "# pad\n# pad\n# pad\n# pad\n# pad\n# pad\nimport ctypes\n\ndef load():\n    lib = ctypes.CDLL(\"libc.so.6\")\n    return lib\n",
    );
}

/// A MODULE-LEVEL ffi call, with no enclosing function. This takes the
/// synthetic `<module>` context branch of `get_ffi_caller_node_id`, which a
/// reviewer showed was the one place the caller span is observable.
///
/// Two properties are pinned together. The node must be POSITIONED at line 1
/// column 0, the honest position for module-level code, and its span must stay
/// DEGENERATE so `has_valid_body_span` keeps a non-body out of the body-hash and
/// shape planes. An earlier revision of this branch widened it to the whole
/// file; three reviewers then measured `<module>` carrying a body hash and a
/// shape descriptor and competing with real functions in `shape-match`.
#[test]
fn module_level_ffi_caller_is_positioned_and_unhashed() {
    // The SHIPPED corpus fixture, not a local string. `test-fixtures/ffi/python/`
    // exists so the BASE-vs-HEAD corpus diff can observe this path at all, and a
    // reviewer showed that nothing tied it to an oracle: replacing its
    // `ctypes.CDLL(...)` call with `None` survived the entire workspace. Reading
    // it here means the fixture cannot silently stop exercising the path.
    let source = include_str!("../../test-fixtures/ffi/python/example.py");
    assert!(
        source.contains("ctypes.CDLL("),
        "the corpus fixture no longer makes a ctypes call, so it no longer \
         exercises the synthetic `<module>` caller branch it exists for"
    );

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("grammar");
    let tree = parser.parse(source, None).expect("parse");
    let mut staging = StagingGraph::new();
    sqry_lang_python::relations::PythonGraphBuilder::default()
        .build_graph(
            &tree,
            source.as_bytes(),
            std::path::Path::new("m.py"),
            &mut staging,
        )
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

/// Which mint reaches `<module>` FIRST decides its span. Nothing widens it
/// later.
///
/// A reviewer measured this after the comments on these sites claimed
/// `apply_span_to_entry` would widen a degenerate context span to a call-site
/// one. It does not: `ensure_callee` returns a cache hit without touching
/// metadata, and `apply_span_to_entry` has a single caller reached only through
/// `update_node_entry`. So the outcome is decided by ordering, and the two
/// orderings below are the evidence.
///
/// This is master's behaviour in both directions. It is pinned here because it
/// was asserted without being tested, which is how the wrong mechanism survived
/// two rounds of review.
#[test]
fn the_first_mint_decides_the_module_context_span() {
    /// `(start_line, start_column, end_line, end_column)` of the `<module>` node.
    fn module_span(source: &str) -> (u32, u32, u32, u32) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .expect("grammar");
        let tree = parser.parse(source, None).expect("parse");
        let mut staging = StagingGraph::new();
        sqry_lang_python::relations::PythonGraphBuilder::default()
            .build_graph(
                &tree,
                source.as_bytes(),
                std::path::Path::new("o.py"),
                &mut staging,
            )
            .expect("build graph");

        let strings: HashMap<u32, String> = staging
            .operations()
            .iter()
            .filter_map(|op| match op {
                StagingOp::InternString { local_id, value } => {
                    Some((local_id.index(), value.clone()))
                }
                _ => None,
            })
            .collect();

        staging
            .operations()
            .iter()
            .find_map(|op| match op {
                StagingOp::AddNode { entry, .. } => {
                    let id = entry.qualified_name.unwrap_or(entry.name).index();
                    (strings.get(&id).map(String::as_str) == Some("<module>")).then_some((
                        entry.start_line,
                        entry.start_column,
                        entry.end_line,
                        entry.end_column,
                    ))
                }
                _ => None,
            })
            .expect("a module-level call must stage a `<module>` node")
    }

    // FFI mint first: the synthetic context creates the node, so the degenerate
    // span stands and the node stays out of the body planes.
    let ffi_first = "import ctypes\nctypes.CDLL(\"libc.so.6\")\nprint(\"after\")\n";
    assert_eq!(
        module_span(ffi_first),
        (1, 0, 1, 0),
        "with the ffi mint first, `<module>` must keep the degenerate context span"
    );

    // Plain call first: that mint creates the node with its call-site span, and
    // the later synthetic context does NOT overwrite or widen it.
    let call_first = "import ctypes\nprint(\"before\")\nctypes.CDLL(\"libc.so.6\")\n";
    let span = module_span(call_first);
    assert_ne!(
        span,
        (1, 0, 1, 0),
        "with a plain call first, `<module>` must carry that call site's span, \
         not the synthetic context's"
    );
    assert_eq!(
        span.0, 2,
        "and that span is the first call site's line, not a widened union: got {span:?}"
    );
}
