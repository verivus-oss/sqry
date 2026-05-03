//! U14 / `C2_TESTS_EXT` cross-language field emission integration tests.
//!
//! Refs: REQ:R0001, REQ:R0002, REQ:R0003, REQ:R0004, REQ:R0005,
//! REQ:R0006, REQ:R0007, REQ:R0008, REQ:R0009, REQ:R0010, REQ:R0011,
//! REQ:R0012, REQ:R0013, REQ:R0014, REQ:R0015, REQ:R0016, REQ:R0017,
//! REQ:R0018, REQ:R0019, REQ:R0020, REQ:R0021, REQ:R0022, REQ:R0023,
//! REQ:R0024.

use anyhow::{Context, Result};
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
use sqry_core::graph::unified::edge::kind::{EdgeKind, TypeOfContext};
use sqry_core::graph::unified::materialize::{display_entry_qualified_name, find_nodes_by_name};
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::resolution::{FileScope, SymbolResolveError};
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_db::planner::{execute_plan, parse_query};
use sqry_db::{QueryDb, QueryDbConfig};
use sqry_mcp::engine::engine_for_workspace;
use sqry_mcp::test_setup::{
    init_discovery_cache, init_engine_cache, init_subgraph_cache, init_trace_path_cache,
};
use sqry_mcp::tool_args::{
    FindUnusedArgs, PaginationArgs, RelationQueryArgs, RelationType, UnusedScope,
};
use sqry_mcp::tool_handlers::{execute_find_unused, execute_relation_query};
use sqry_plugin_registry::create_plugin_manager;
use std::collections::BTreeSet;
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::Duration;
use tempfile::TempDir;

#[derive(Debug, Clone, Copy)]
struct FieldCase {
    label: &'static str,
    file_fragment: &'static str,
    fixture_dir: &'static str,
    lookup_name: &'static str,
    mutable_suffix: &'static str,
    mutable_bare: &'static str,
    constant_suffix: Option<&'static str>,
    static_suffix: Option<&'static str>,
    public_suffix: Option<&'static str>,
    private_suffix: Option<&'static str>,
    collision_bare: &'static str,
}

const FIELD_CASES: &[FieldCase] = &[
    FieldCase {
        label: "cpp",
        file_fragment: "cross-language/cpp/fields.cpp",
        fixture_dir: "cross-language/cpp",
        lookup_name: "Ledger.mutableField",
        mutable_suffix: "Ledger.mutableField",
        mutable_bare: "mutableField",
        constant_suffix: Some("Ledger.immutableField"),
        static_suffix: Some("Ledger.staticField"),
        public_suffix: Some("Ledger.mutableField"),
        private_suffix: Some("Ledger.privateField"),
        collision_bare: "sharedName",
    },
    FieldCase {
        label: "csharp",
        file_fragment: "cross-language/csharp/Fields.cs",
        fixture_dir: "cross-language/csharp",
        lookup_name: "Ledger.MutableField",
        mutable_suffix: "Ledger.MutableField",
        mutable_bare: "MutableField",
        constant_suffix: Some("Ledger.ImmutableField"),
        static_suffix: Some("Ledger.StaticField"),
        public_suffix: Some("Ledger.MutableField"),
        private_suffix: Some("Ledger.PrivateField"),
        collision_bare: "SharedName",
    },
    FieldCase {
        label: "javascript",
        file_fragment: "cross-language/javascript/fields.js",
        fixture_dir: "cross-language/javascript",
        lookup_name: "Ledger.mutableField",
        mutable_suffix: "Ledger.mutableField",
        mutable_bare: "mutableField",
        constant_suffix: None,
        static_suffix: Some("Ledger.staticField"),
        public_suffix: Some("Ledger.mutableField"),
        private_suffix: Some("Ledger.#privateField"),
        collision_bare: "sharedName",
    },
    FieldCase {
        label: "php",
        file_fragment: "cross-language/php/Fields.php",
        fixture_dir: "cross-language/php",
        lookup_name: "Ledger.mutableField",
        mutable_suffix: "Ledger.mutableField",
        mutable_bare: "mutableField",
        constant_suffix: Some("Ledger.immutableField"),
        static_suffix: Some("Ledger.staticField"),
        public_suffix: Some("Ledger.mutableField"),
        private_suffix: Some("Ledger.privateField"),
        collision_bare: "sharedName",
    },
    FieldCase {
        label: "ruby",
        file_fragment: "cross-language/ruby/fields.rb",
        fixture_dir: "cross-language/ruby",
        lookup_name: "Ledger#mutable_field",
        mutable_suffix: "Ledger#mutable_field",
        mutable_bare: "mutable_field",
        constant_suffix: Some("Ledger#immutable_field"),
        static_suffix: None,
        public_suffix: Some("Ledger#mutable_field"),
        private_suffix: Some("Ledger#private_field"),
        collision_bare: "shared_name",
    },
    FieldCase {
        label: "rust",
        file_fragment: "cross-language/rust/fields.rs",
        fixture_dir: "cross-language/rust",
        lookup_name: "Ledger::mutable_field",
        mutable_suffix: "Ledger::mutable_field",
        mutable_bare: "mutable_field",
        constant_suffix: None,
        static_suffix: None,
        public_suffix: Some("Ledger::mutable_field"),
        private_suffix: Some("Ledger::immutable_field"),
        collision_bare: "shared_name",
    },
    FieldCase {
        label: "apex",
        file_fragment: "cross-language/apex/Fields.cls",
        fixture_dir: "cross-language/apex",
        lookup_name: "Ledger.mutableField",
        mutable_suffix: "Ledger.mutableField",
        mutable_bare: "mutableField",
        constant_suffix: Some("Ledger.immutableField"),
        static_suffix: Some("Ledger.staticField"),
        public_suffix: Some("Ledger.mutableField"),
        private_suffix: Some("Ledger.privateField"),
        collision_bare: "sharedName",
    },
    FieldCase {
        label: "abap",
        file_fragment: "cross-language/abap/fields.abap",
        fixture_dir: "cross-language/abap",
        lookup_name: "zcl_ledger.mutable_field",
        mutable_suffix: "zcl_ledger.mutable_field",
        mutable_bare: "mutable_field",
        constant_suffix: Some("zcl_ledger.immutable_field"),
        static_suffix: Some("zcl_ledger.static_field"),
        public_suffix: Some("zcl_ledger.mutable_field"),
        private_suffix: Some("zcl_ledger.private_field"),
        collision_bare: "shared_name",
    },
    FieldCase {
        label: "scala",
        file_fragment: "cross-language/scala/Fields.scala",
        fixture_dir: "cross-language/scala",
        lookup_name: "Ledger.mutableField",
        mutable_suffix: "Ledger.mutableField",
        mutable_bare: "mutableField",
        constant_suffix: Some("Ledger.immutableField"),
        static_suffix: Some("Ledger.staticField"),
        public_suffix: Some("Ledger.mutableField"),
        private_suffix: Some("Ledger.privateField"),
        collision_bare: "sharedName",
    },
    FieldCase {
        label: "swift",
        file_fragment: "cross-language/swift/Fields.swift",
        fixture_dir: "cross-language/swift",
        lookup_name: "Ledger.mutableField",
        mutable_suffix: "Ledger.mutableField",
        mutable_bare: "mutableField",
        constant_suffix: Some("Ledger.immutableField"),
        static_suffix: Some("Ledger.staticField"),
        public_suffix: Some("Ledger.mutableField"),
        private_suffix: Some("Ledger.privateField"),
        collision_bare: "sharedName",
    },
    FieldCase {
        label: "typescript",
        file_fragment: "cross-language/typescript/fields.ts",
        fixture_dir: "cross-language/typescript",
        lookup_name: "Ledger.mutableField",
        mutable_suffix: "Ledger.mutableField",
        mutable_bare: "mutableField",
        constant_suffix: Some("Ledger.immutableField"),
        static_suffix: Some("Ledger.staticField"),
        public_suffix: Some("Ledger.mutableField"),
        private_suffix: Some("Ledger.privateField"),
        collision_bare: "sharedName",
    },
    FieldCase {
        label: "zig",
        file_fragment: "cross-language/zig/fields.zig",
        fixture_dir: "cross-language/zig",
        lookup_name: "Ledger.mutableField",
        mutable_suffix: "Ledger.mutableField",
        mutable_bare: "mutableField",
        constant_suffix: Some("Ledger.staticField"),
        static_suffix: Some("Ledger.staticField"),
        public_suffix: None,
        private_suffix: None,
        collision_bare: "sharedName",
    },
    FieldCase {
        label: "java",
        file_fragment: "cross-language/java/fields.java",
        fixture_dir: "cross-language/java",
        lookup_name: "Ledger.mutableField",
        mutable_suffix: "Ledger.mutableField",
        mutable_bare: "mutableField",
        constant_suffix: Some("Ledger.immutableField"),
        static_suffix: Some("Ledger.staticField"),
        public_suffix: Some("Ledger.mutableField"),
        private_suffix: Some("Ledger.privateField"),
        collision_bare: "sharedName",
    },
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn fixture_root_for(case: &FieldCase) -> PathBuf {
    repo_root().join("test-fixtures").join(case.fixture_dir)
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = dst.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_all(&source_path, &target_path)?;
        } else if source_path.is_file() {
            fs::copy(&source_path, &target_path).with_context(|| {
                format!(
                    "copy {} -> {}",
                    source_path.display(),
                    target_path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn temp_fixture_root_for(case: &FieldCase) -> Result<TempDir> {
    let tmp = tempfile::tempdir()?;
    copy_dir_all(&fixture_root_for(case), tmp.path())?;
    Ok(tmp)
}

fn init_mcp_caches() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        init_discovery_cache(NonZeroUsize::new(64).expect("non-zero discovery cache"));
        init_engine_cache(NonZeroUsize::new(16).expect("non-zero engine cache"));
        init_trace_path_cache(
            NonZeroUsize::new(64).expect("non-zero trace cache"),
            Duration::from_secs(60),
        );
        init_subgraph_cache(
            NonZeroUsize::new(64).expect("non-zero subgraph cache"),
            Duration::from_secs(60),
        );
    });
}

fn index_mcp_fixture(root: &Path) -> Result<()> {
    init_mcp_caches();
    let engine = engine_for_workspace(Some(&root.to_path_buf()))?;
    let _ = engine.ensure_graph()?;
    Ok(())
}

fn workspace_arg(root: &Path) -> String {
    root.canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn paging() -> PaginationArgs {
    PaginationArgs {
        offset: 0,
        size: 1_000,
    }
}

fn build_graph(root: &Path) -> Result<CodeGraph> {
    let plugins = create_plugin_manager();
    let config = BuildConfig::default();
    build_unified_graph(root, &plugins, &config)
        .with_context(|| format!("build graph at {}", root.display()))
}

fn resolved(snapshot: &GraphSnapshot, entry: &NodeEntry) -> (String, Option<String>) {
    let name = snapshot
        .strings()
        .resolve(entry.name)
        .expect("node name must resolve");
    let qualified_name = entry
        .qualified_name
        .and_then(|id| snapshot.strings().resolve(id))
        .map(|name| name.to_string());
    (name.to_string(), qualified_name)
}

fn node_label(snapshot: &GraphSnapshot, entry: &NodeEntry) -> String {
    let (name, _) = resolved(snapshot, entry);
    display_entry_qualified_name(entry, snapshot.strings(), snapshot.files(), &name)
}

fn same_canonical_suffix(label: &str, suffix: &str) -> bool {
    label == suffix
        || label.ends_with(&format!("::{suffix}"))
        || label.ends_with(&format!(".{suffix}"))
        || label.ends_with(&format!("#{suffix}"))
}

fn graph_lookup_for_display(case: &FieldCase, display_name: &str) -> String {
    if matches!(case.label, "cpp" | "php") {
        display_name.to_string()
    } else {
        display_name.replace(['.', '#'], "::")
    }
}

fn entry_file_contains(snapshot: &GraphSnapshot, entry: &NodeEntry, fragment: &str) -> bool {
    snapshot
        .files()
        .resolve(entry.file)
        .is_some_and(|path| path.to_string_lossy().contains(fragment))
}

fn find_node_by_suffix(
    snapshot: &GraphSnapshot,
    suffix: &str,
    kind: NodeKind,
    file_fragment: &str,
) -> Result<(NodeId, NodeEntry)> {
    let matches = snapshot
        .iter_nodes()
        .filter(|(_, entry)| !entry.is_unified_loser())
        .filter(|(_, entry)| entry.kind == kind)
        .filter(|(_, entry)| entry_file_contains(snapshot, entry, file_fragment))
        .filter(|(_, entry)| {
            let label = node_label(snapshot, entry);
            same_canonical_suffix(&label, suffix)
        })
        .map(|(id, entry)| (id, entry.clone()))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [(id, entry)] => Ok((*id, entry.clone())),
        [] => anyhow::bail!("missing {kind:?} node ending with `{suffix}`"),
        _ => {
            let normalized_suffix = suffix.replace("::", ".");
            let exact_matches = matches
                .iter()
                .filter(|(_, entry)| {
                    node_label(snapshot, entry).replace("::", ".") == normalized_suffix
                })
                .collect::<Vec<_>>();
            match exact_matches.as_slice() {
                [(id, entry)] => Ok((*id, entry.clone())),
                _ => anyhow::bail!(
                    "ambiguous {kind:?} node suffix `{suffix}`: {}",
                    matches.len()
                ),
            }
        }
    }
}

fn visibility(snapshot: &GraphSnapshot, entry: &NodeEntry) -> Option<String> {
    entry
        .visibility
        .and_then(|id| snapshot.strings().resolve(id))
        .map(|value| value.to_string())
}

fn has_typeof_field_edge(snapshot: &GraphSnapshot, source: NodeId, bare_name: &str) -> bool {
    snapshot.edges().edges_from(source).into_iter().any(|edge| {
        if let EdgeKind::TypeOf { context, name, .. } = edge.kind {
            let edge_name = name
                .and_then(|id| snapshot.strings().resolve(id))
                .map(|value| value.to_string());
            context == Some(TypeOfContext::Field) && edge_name.as_deref() == Some(bare_name)
        } else {
            false
        }
    })
}

// Returns `Result<()>` to match the surrounding contract-assertion
// helpers (see `assert_planner_kind_name_contains`, etc.) so callers
// can chain `?` uniformly. The current body never returns `Err`.
#[allow(clippy::unnecessary_wraps)]
fn assert_find_nodes_by_name_unique(
    snapshot: &GraphSnapshot,
    query: &str,
    expected: NodeId,
    case: &FieldCase,
) -> Result<()> {
    // Per 05_TEST_PLAN §7.5, qualified user-facing display names (e.g.
    // "Ledger.mutableField" / "Ledger#mutable_field" / "Ledger::mutable_field")
    // MUST resolve to exactly one live Property/Constant NodeId via the
    // documented display form — no graph-internal "::" rewrite. We drop
    // unified losers because phase4c-prime tombstones them legitimately,
    // but the live result set must equal exactly the expected node.
    let live_matches = find_nodes_by_name(snapshot, query)
        .into_iter()
        .filter(|node_id| {
            snapshot
                .get_node(*node_id)
                .is_some_and(|entry| !entry.is_unified_loser())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        live_matches,
        vec![expected],
        "{} find_nodes_by_name({query:?}) must return exactly the live field node \
         (no extra candidates, no display→`::` rewrite); got {live_matches:?}",
        case.label
    );
    Ok(())
}

fn planner_node_ids(snapshot: &GraphSnapshot, query: &str) -> Result<Vec<NodeId>> {
    let db = QueryDb::new(Arc::new(snapshot.clone()), QueryDbConfig::default());
    let plan = parse_query(query).with_context(|| format!("parse planner query `{query}`"))?;
    Ok(execute_plan(&plan, &db))
}

fn quoted_name_query(kind: &str, name: &str) -> String {
    format!("kind:{kind} name:\"{}\"", name.replace('"', "\\\""))
}

fn assert_planner_kind_name_contains(
    snapshot: &GraphSnapshot,
    kind: &str,
    name: &str,
    expected: NodeId,
    case: &FieldCase,
) -> Result<()> {
    let query = quoted_name_query(kind, name);
    let matches = planner_node_ids(snapshot, &query)?;
    assert!(
        matches.contains(&expected),
        "{} planner `{query}` must include `{}`; got {matches:?}",
        case.label,
        case.mutable_suffix
    );
    Ok(())
}

fn assert_strict_bare_collision_is_ambiguous(snapshot: &GraphSnapshot, case: &FieldCase) {
    let err = snapshot
        .resolve_global_symbol_ambiguity_aware(case.collision_bare, FileScope::Any)
        .expect_err("duplicate bare field name must be ambiguous");
    assert!(
        matches!(err, SymbolResolveError::Ambiguous(_)),
        "{} strict resolver should return AmbiguousSymbol for bare `{}`, got {err}",
        case.label,
        case.collision_bare
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn test_req_r0001_r0003_r0004_r0005_field_contract_cross_language() -> Result<()> {
    for case in FIELD_CASES {
        let graph = build_graph(&fixture_root_for(case))?;
        let snapshot = graph.snapshot();

        let (node_id, mutable) = find_node_by_suffix(
            &snapshot,
            case.mutable_suffix,
            NodeKind::Property,
            case.file_fragment,
        )
        .with_context(|| format!("{} mutable property", case.label))?;
        assert!(
            has_typeof_field_edge(&snapshot, node_id, case.mutable_bare),
            "{} mutable field must have TypeOf{{Field}} edge named `{}`",
            case.label,
            case.mutable_bare
        );
        assert_find_nodes_by_name_unique(&snapshot, case.lookup_name, node_id, case)?;
        let strict_id = snapshot
            .resolve_global_symbol_ambiguity_aware(case.lookup_name, FileScope::Any)
            .with_context(|| format!("{} strict resolver for {}", case.label, case.lookup_name))?;
        assert_eq!(
            strict_id, node_id,
            "{} strict resolver must return the field NodeId",
            case.label
        );
        assert_planner_kind_name_contains(&snapshot, "property", case.lookup_name, node_id, case)?;
        let variable_query = quoted_name_query("variable", case.mutable_bare);
        let variable_ids = planner_node_ids(&snapshot, &variable_query)?;
        assert!(
            !variable_ids.contains(&node_id),
            "{} planner `{variable_query}` must not return the class field node",
            case.label
        );

        if let Some(constant_suffix) = case.constant_suffix {
            let (constant_id, _) = find_node_by_suffix(
                &snapshot,
                constant_suffix,
                NodeKind::Constant,
                case.file_fragment,
            )
            .with_context(|| format!("{} constant field", case.label))?;
            let constant_lookup = graph_lookup_for_display(case, constant_suffix);
            assert_planner_kind_name_contains(
                &snapshot,
                "constant",
                &constant_lookup,
                constant_id,
                case,
            )?;
        }

        if let Some(static_suffix) = case.static_suffix {
            let (_, static_node) = find_node_by_suffix(
                &snapshot,
                static_suffix,
                if case.label == "zig" {
                    NodeKind::Constant
                } else {
                    NodeKind::Property
                },
                case.file_fragment,
            )
            .with_context(|| format!("{} static field", case.label))?;
            assert!(
                static_node.is_static,
                "{} static field must set is_static",
                case.label
            );
        } else {
            assert!(
                !mutable.is_static,
                "{} instance field must not be static",
                case.label
            );
        }

        if let Some(public_suffix) = case.public_suffix {
            let (_, public_node) = find_node_by_suffix(
                &snapshot,
                public_suffix,
                NodeKind::Property,
                case.file_fragment,
            )
            .with_context(|| format!("{} public field", case.label))?;
            assert_eq!(
                visibility(&snapshot, &public_node).as_deref(),
                Some("public"),
                "{} public field visibility",
                case.label
            );
        }

        if let Some(private_suffix) = case.private_suffix {
            let (_, private_node) = find_node_by_suffix(
                &snapshot,
                private_suffix,
                NodeKind::Property,
                case.file_fragment,
            )
            .with_context(|| format!("{} private field", case.label))?;
            assert_eq!(
                visibility(&snapshot, &private_node).as_deref(),
                Some("private"),
                "{} private field visibility",
                case.label
            );
        }

        if case.label == "ruby" {
            let (_, protected_node) = find_node_by_suffix(
                &snapshot,
                "Ledger#protected_field",
                NodeKind::Constant,
                case.file_fragment,
            )
            .with_context(|| format!("{} protected attr_reader field", case.label))?;
            assert_eq!(
                visibility(&snapshot, &protected_node).as_deref(),
                Some("protected"),
                "Ruby protected attr_reader field visibility",
            );
        }
    }

    Ok(())
}

#[test]
fn test_req_r0011_duplicate_bare_field_names_are_qualified() -> Result<()> {
    for case in FIELD_CASES {
        let graph = build_graph(&fixture_root_for(case))?;
        let snapshot = graph.snapshot();

        let labels = snapshot
            .iter_nodes()
            .filter(|(_, entry)| !entry.is_unified_loser())
            .filter(|(_, entry)| matches!(entry.kind, NodeKind::Property | NodeKind::Constant))
            .filter(|(_, entry)| entry_file_contains(&snapshot, entry, case.file_fragment))
            .map(|(_, entry)| node_label(&snapshot, entry))
            .filter(|label| same_canonical_suffix(label, case.collision_bare))
            .collect::<BTreeSet<_>>();

        assert!(
            labels.len() >= 2,
            "{} must expose duplicate bare field `{}` as qualified nodes, got {labels:?}",
            case.label,
            case.collision_bare
        );
        assert_strict_bare_collision_is_ambiguous(&snapshot, case);
    }

    Ok(())
}

#[test]
fn test_req_r0012_mcp_find_unused_excludes_public_typeof_field_roots() -> Result<()> {
    for case in FIELD_CASES
        .iter()
        .filter(|case| case.public_suffix.is_some())
    {
        let workspace = temp_fixture_root_for(case)?;
        index_mcp_fixture(workspace.path())?;
        let args = FindUnusedArgs {
            path: workspace_arg(workspace.path()),
            scope: UnusedScope::Public,
            languages: Vec::new(),
            kinds: vec!["property".to_string(), "constant".to_string()],
            max_results: 10_000,
            pagination: paging(),
        };
        let result = execute_find_unused(&args)
            .with_context(|| format!("{} execute_find_unused", case.label))?;
        let public_suffix = case.public_suffix.expect("filtered above");
        assert!(
            result
                .data
                .symbols
                .iter()
                .all(|symbol| !same_canonical_suffix(&symbol.qualified_name, public_suffix)),
            "{} public field `{public_suffix}` must not be reported unused: {:?}",
            case.label,
            result.data.symbols
        );
    }
    Ok(())
}

#[test]
fn test_req_r0011_relation_query_resolves_qualified_field_symbols() -> Result<()> {
    for case in FIELD_CASES {
        let workspace = temp_fixture_root_for(case)?;
        index_mcp_fixture(workspace.path())?;
        let args = RelationQueryArgs {
            symbol: case.lookup_name.to_string(),
            relation: RelationType::Returns,
            path: workspace_arg(workspace.path()),
            max_depth: 1,
            max_results: 100,
            pagination: paging(),
        };
        let result = execute_relation_query(&args)
            .with_context(|| format!("{} relation_query qualified field resolution", case.label))?;
        assert_eq!(
            result.data.total, 0,
            "{} field `{}` should resolve through relation_query and have no return edges",
            case.label, case.lookup_name
        );
    }
    Ok(())
}

#[test]
fn test_req_r0020_cpp_legacy_double_colon_lookup_returns_zero() -> Result<()> {
    let cpp_case = FIELD_CASES
        .iter()
        .find(|case| case.label == "cpp")
        .expect("cpp case present");
    let graph = build_graph(&fixture_root_for(cpp_case))?;
    let snapshot = graph.snapshot();

    let legacy = snapshot
        .find_by_exact_name("Ledger::mutableField")
        .into_iter()
        .filter_map(|node_id| snapshot.get_node(node_id))
        .filter(|entry| !entry.is_unified_loser())
        .filter(|entry| entry_file_contains(&snapshot, entry, "cross-language/cpp/fields.cpp"))
        .filter(|entry| matches!(entry.kind, NodeKind::Property | NodeKind::Constant))
        .map(|entry| node_label(&snapshot, entry))
        .collect::<Vec<_>>();

    assert!(
        legacy.is_empty(),
        "C++ legacy Class::field lookup must return zero field hits, got {legacy:?}"
    );

    find_node_by_suffix(
        &snapshot,
        "Ledger.mutableField",
        NodeKind::Property,
        "cross-language/cpp/fields.cpp",
    )?;
    Ok(())
}
