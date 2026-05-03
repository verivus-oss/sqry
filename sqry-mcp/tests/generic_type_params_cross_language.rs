//! U14 / `C2_TESTS_EXT` generic type-parameter integration tests.
//!
//! Refs: REQ:R0025, REQ:R0026, REQ:R0027, REQ:R0028, REQ:R0029, REQ:R0030.

use anyhow::{Context, Result};
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
use sqry_core::graph::unified::edge::kind::{EdgeKind, TypeOfContext};
use sqry_core::graph::unified::materialize::find_nodes_by_name;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::resolution::{FileScope, SymbolResolveError};
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_plugin_registry::create_plugin_manager;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy)]
struct ConstraintCase {
    label: &'static str,
    file_fragment: &'static str,
    type_param_suffix: &'static str,
    constraint_targets: &'static [&'static str],
}

const CONSTRAINT_CASES: &[ConstraintCase] = &[
    ConstraintCase {
        label: "go Map T",
        file_fragment: "cross-language-typeparams/go/generics.go",
        type_param_suffix: "main.Map.T",
        constraint_targets: &["any"],
    },
    ConstraintCase {
        label: "go Map U",
        file_fragment: "cross-language-typeparams/go/generics.go",
        type_param_suffix: "main.Map.U",
        constraint_targets: &["comparable"],
    },
    ConstraintCase {
        label: "go Sum T union",
        file_fragment: "cross-language-typeparams/go/generics.go",
        type_param_suffix: "main.Sum.T",
        constraint_targets: &["int | float64"],
    },
    ConstraintCase {
        label: "go receiver-bound List E",
        file_fragment: "cross-language-typeparams/go/generics.go",
        type_param_suffix: "main.List.E",
        constraint_targets: &["any"],
    },
    ConstraintCase {
        label: "java generic class T",
        file_fragment: "cross-language-typeparams/java/Generics.java",
        type_param_suffix: "crosslanguage.generics.Generics.T",
        constraint_targets: &["Comparable", "Number"],
    },
    ConstraintCase {
        label: "java recursive method U",
        file_fragment: "cross-language-typeparams/java/Generics.java",
        type_param_suffix: "crosslanguage.generics.Generics.max.U",
        constraint_targets: &["Comparable"],
    },
    ConstraintCase {
        label: "java choose K multiple bounds",
        file_fragment: "cross-language-typeparams/java/Generics.java",
        type_param_suffix: "crosslanguage.generics.Generics.choose.K",
        constraint_targets: &["A", "B"],
    },
    ConstraintCase {
        label: "java generic constructor V",
        file_fragment: "cross-language-typeparams/java/Generics.java",
        type_param_suffix: "crosslanguage.generics.Generics.<init>.V",
        constraint_targets: &[],
    },
    ConstraintCase {
        label: "java generic interface Box W",
        file_fragment: "cross-language-typeparams/java/Generics.java",
        type_param_suffix: "crosslanguage.generics.Generics.Box.W",
        constraint_targets: &["Bar"],
    },
    ConstraintCase {
        label: "kotlin where-clause constrained T",
        file_fragment: "cross-language-typeparams/kotlin/Generics.kt",
        type_param_suffix: "ktConstrained.T",
        constraint_targets: &["A", "B"],
    },
    ConstraintCase {
        label: "kotlin reified identity T",
        file_fragment: "cross-language-typeparams/kotlin/Generics.kt",
        type_param_suffix: "ktIdentity.T",
        constraint_targets: &[],
    },
    ConstraintCase {
        label: "kotlin Store T",
        file_fragment: "cross-language-typeparams/kotlin/Generics.kt",
        type_param_suffix: "KStore.T",
        constraint_targets: &["A"],
    },
    ConstraintCase {
        label: "kotlin Store.put U",
        file_fragment: "cross-language-typeparams/kotlin/Generics.kt",
        type_param_suffix: "KStore.put.U",
        constraint_targets: &["B"],
    },
    ConstraintCase {
        label: "csharp class Box T",
        file_fragment: "cross-language-typeparams/csharp/Generics.cs",
        type_param_suffix: "CsBox.T",
        constraint_targets: &["IA"],
    },
    ConstraintCase {
        label: "csharp Identity T synthetic constraints",
        file_fragment: "cross-language-typeparams/csharp/Generics.cs",
        type_param_suffix: "Generics.Identity.T",
        constraint_targets: &["class", "new()"],
    },
    ConstraintCase {
        label: "csharp Combine T synthetic constraints",
        file_fragment: "cross-language-typeparams/csharp/Generics.cs",
        type_param_suffix: "Generics.Combine.T",
        constraint_targets: &["IA", "IB", "notnull"],
    },
    ConstraintCase {
        label: "rust trait Store T",
        file_fragment: "cross-language-typeparams/rust/generics.rs",
        type_param_suffix: "Store::T",
        constraint_targets: &["Clone"],
    },
    ConstraintCase {
        label: "rust identity T",
        file_fragment: "cross-language-typeparams/rust/generics.rs",
        type_param_suffix: "identity::T",
        constraint_targets: &["Clone"],
    },
    ConstraintCase {
        label: "rust where-clause constrained T",
        file_fragment: "cross-language-typeparams/rust/generics.rs",
        type_param_suffix: "constrained::T",
        constraint_targets: &["Display", "Send"],
    },
    ConstraintCase {
        label: "rust const generic N",
        file_fragment: "cross-language-typeparams/rust/generics.rs",
        type_param_suffix: "array::N",
        constraint_targets: &["usize"],
    },
    ConstraintCase {
        label: "typescript identity T",
        file_fragment: "cross-language-typeparams/typescript/generics.ts",
        type_param_suffix: "identity.T",
        constraint_targets: &["string"],
    },
    ConstraintCase {
        label: "typescript map U",
        file_fragment: "cross-language-typeparams/typescript/generics.ts",
        type_param_suffix: "map.U",
        constraint_targets: &["number"],
    },
    ConstraintCase {
        label: "typescript mapped type T",
        file_fragment: "cross-language-typeparams/typescript/generics.ts",
        type_param_suffix: "Mapped.T",
        constraint_targets: &[],
    },
    ConstraintCase {
        label: "typescript mapped type V default",
        file_fragment: "cross-language-typeparams/typescript/generics.ts",
        type_param_suffix: "Mapped.V",
        constraint_targets: &[],
    },
    ConstraintCase {
        label: "typescript mapped type binder K",
        file_fragment: "cross-language-typeparams/typescript/generics.ts",
        type_param_suffix: "Mapped.K",
        constraint_targets: &[],
    },
    ConstraintCase {
        label: "typescript variadic tuple T",
        file_fragment: "cross-language-typeparams/typescript/generics.ts",
        type_param_suffix: "Variadic.T",
        constraint_targets: &["unknown[]"],
    },
    ConstraintCase {
        label: "typescript conditional T",
        file_fragment: "cross-language-typeparams/typescript/generics.ts",
        type_param_suffix: "Conditional.T",
        constraint_targets: &[],
    },
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn fixture_root() -> PathBuf {
    repo_root().join("test-fixtures/cross-language-typeparams")
}

fn build_typeparam_graph() -> Result<CodeGraph> {
    let plugins = create_plugin_manager();
    let config = BuildConfig::default();
    build_unified_graph(&fixture_root(), &plugins, &config).context("build type-param graph")
}

fn node_label(snapshot: &GraphSnapshot, entry: &NodeEntry) -> Option<String> {
    let name = snapshot.strings().resolve(entry.name)?;
    Some(
        entry
            .qualified_name
            .and_then(|id| snapshot.strings().resolve(id))
            .unwrap_or(name)
            .to_string(),
    )
}

fn same_canonical_suffix(label: &str, suffix: &str) -> bool {
    let normalized_label = label.replace("::", ".");
    let normalized_suffix = suffix.replace("::", ".");
    label == suffix
        || label.ends_with(&format!("::{suffix}"))
        || normalized_label == normalized_suffix
        || normalized_label.ends_with(&format!(".{normalized_suffix}"))
}

fn entry_file_contains(snapshot: &GraphSnapshot, entry: &NodeEntry, fragment: &str) -> bool {
    snapshot
        .files()
        .resolve(entry.file)
        .is_some_and(|path| path.to_string_lossy().contains(fragment))
}

fn find_type_node_by_suffix(
    snapshot: &GraphSnapshot,
    suffix: &str,
    file_fragment: &str,
) -> Result<(NodeId, NodeEntry)> {
    let matches = snapshot
        .iter_nodes()
        .filter(|(_, entry)| entry.kind == NodeKind::Type)
        .filter(|(_, entry)| entry_file_contains(snapshot, entry, file_fragment))
        .filter(|(_, entry)| {
            node_label(snapshot, entry).is_some_and(|label| same_canonical_suffix(&label, suffix))
        })
        .map(|(id, entry)| (id, entry.clone()))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [(id, entry)] => Ok((*id, entry.clone())),
        [] => anyhow::bail!("missing Type node ending with `{suffix}`"),
        _ => anyhow::bail!("ambiguous Type node suffix `{suffix}`: {}", matches.len()),
    }
}

fn edge_target_label(snapshot: &GraphSnapshot, target: NodeId) -> String {
    let entry = snapshot
        .get_node(target)
        .unwrap_or_else(|| panic!("missing edge target {target:?}"));
    node_label(snapshot, entry).unwrap_or_else(|| format!("{target:?}"))
}

fn sorted_labels(mut labels: Vec<String>) -> Vec<String> {
    labels.sort();
    labels.dedup();
    labels
}

fn constraint_targets(snapshot: &GraphSnapshot, source: NodeId) -> Vec<String> {
    sorted_labels(
        snapshot
            .edges()
            .edges_from(source)
            .into_iter()
            .filter(|edge| {
                matches!(
                    edge.kind,
                    EdgeKind::TypeOf {
                        context: Some(TypeOfContext::Constraint),
                        ..
                    }
                )
            })
            .map(|edge| edge_target_label(snapshot, edge.target))
            .collect(),
    )
}

fn reference_targets(snapshot: &GraphSnapshot, source: NodeId) -> Vec<String> {
    sorted_labels(
        snapshot
            .edges()
            .edges_from(source)
            .into_iter()
            .filter(|edge| matches!(edge.kind, EdgeKind::References))
            .map(|edge| edge_target_label(snapshot, edge.target))
            .collect(),
    )
}

fn find_node_by_suffix(snapshot: &GraphSnapshot, suffix: &str) -> Result<NodeId> {
    let matches = snapshot
        .iter_nodes()
        .filter(|(_, entry)| {
            node_label(snapshot, entry).is_some_and(|label| same_canonical_suffix(&label, suffix))
        })
        .map(|(id, _)| id)
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [id] => Ok(*id),
        [] => anyhow::bail!("missing node ending with `{suffix}`"),
        _ => anyhow::bail!("ambiguous node suffix `{suffix}`: {}", matches.len()),
    }
}

#[test]
fn test_req_r0025_to_r0030_generic_type_parameter_constraints_cross_language() -> Result<()> {
    let graph = build_typeparam_graph()?;
    let snapshot = graph.snapshot();

    for case in CONSTRAINT_CASES {
        let (node_id, _) =
            find_type_node_by_suffix(&snapshot, case.type_param_suffix, case.file_fragment)
                .with_context(|| format!("{} Type node", case.label))?;

        let lookup_hits = find_nodes_by_name(&snapshot, case.type_param_suffix)
            .into_iter()
            .filter(|candidate_id| {
                snapshot
                    .get_node(*candidate_id)
                    .is_some_and(|entry| !entry.is_unified_loser())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            lookup_hits,
            vec![node_id],
            "{} find_nodes_by_name({:?}) must return exactly the fixture Type node",
            case.label,
            case.type_param_suffix,
        );

        let mut expected = case
            .constraint_targets
            .iter()
            .map(|target| (*target).to_string())
            .collect::<Vec<_>>();
        expected.sort();

        assert_eq!(
            constraint_targets(&snapshot, node_id),
            expected,
            "{} exact TypeOf{{Constraint}} targets",
            case.label
        );
    }

    Ok(())
}

#[test]
fn test_req_r0025_go_receiver_bound_typeparam_resolves_to_declared_node() -> Result<()> {
    let graph = build_typeparam_graph()?;
    let snapshot = graph.snapshot();
    let (receiver_type_param, _) = find_type_node_by_suffix(
        &snapshot,
        "main.List.E",
        "cross-language-typeparams/go/generics.go",
    )?;
    let push = find_node_by_suffix(&snapshot, "main.List.Push")?;
    let points_at_declared_param = snapshot.edges().edges_from(push).into_iter().any(|edge| {
        edge.target == receiver_type_param
            && matches!(
                edge.kind,
                EdgeKind::References
                    | EdgeKind::TypeOf {
                        context: Some(TypeOfContext::Parameter),
                        ..
                    }
            )
    });

    assert!(
        points_at_declared_param,
        "Go List.Push must point at the declared main.List.E Type node, not a bare E stub"
    );
    Ok(())
}

#[test]
fn test_req_r0030_typescript_default_type_parameter_emits_reference_edge() -> Result<()> {
    let graph = build_typeparam_graph()?;
    let snapshot = graph.snapshot();
    let (mapped_v, _) = find_type_node_by_suffix(
        &snapshot,
        "Mapped.V",
        "cross-language-typeparams/typescript/generics.ts",
    )?;

    assert!(
        reference_targets(&snapshot, mapped_v).contains(&"string".to_string()),
        "TypeScript Mapped.V must reference its default type `string`"
    );
    Ok(())
}

#[test]
fn test_typeparam_bare_name_collision_yields_ambiguous() -> Result<()> {
    let graph = build_typeparam_graph()?;
    let snapshot = graph.snapshot();

    let err = snapshot
        .resolve_global_symbol_ambiguity_aware("T", FileScope::Any)
        .expect_err("bare T must be ambiguous across generic declarations");
    assert!(
        matches!(err, SymbolResolveError::Ambiguous(_)),
        "bare T must return AmbiguousSymbol, got {err}"
    );
    Ok(())
}
