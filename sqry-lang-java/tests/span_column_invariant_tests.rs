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

/// Imports, and a PUBLIC field constant. The public modifier matters: the
/// exported-member mint is gated on `is_public`, so a package-private field
/// never reaches it. An earlier fixture missed that and the site looked like it
/// had no span at all.
#[test]
fn declarations_and_imports_keep_columns_inside_their_lines() {
    assert_columns_fit_their_lines(
        "// p\n// p\n// p\n// p\n// p\n// p\nimport java.util.List;\n\npublic class Holder {\n    public static final int ANSWER = 42;\n    int compute() { return ANSWER; }\n}\n",
    );
}

/// The JNA and Panama FFI mints. Each shape is dispatched on a distinct
/// receiver and method name, so the fixture has to name them exactly:
/// `Native.load`, a `Library` interface field call, `Linker.nativeLinker`,
/// `SymbolLookup.libraryLookup`, and a `MethodHandle` invoke.
#[test]
fn ffi_caller_and_target_mints_keep_columns_inside_their_lines() {
    assert_columns_fit_their_lines(
        "// p\n// p\n// p\n// p\n// p\n// p\nimport com.sun.jna.Library;\nimport com.sun.jna.Native;\nimport java.lang.foreign.Linker;\nimport java.lang.foreign.SymbolLookup;\n\ninterface CLib extends Library {\n    int strlen(String s);\n}\n\npublic class Bridge {\n    CLib lib;\n    static native void jniCall();\n    void load() { Native.load(\"c\", Bridge.class); }\n    void iface() { lib.strlen(\"hi\"); }\n    void pan() {\n        Linker linker = Linker.nativeLinker();\n        SymbolLookup.libraryLookup(\"c\", null);\n        java.lang.invoke.MethodHandle mh = null;\n        mh.invokeExact();\n    }\n}\n",
    );
}
