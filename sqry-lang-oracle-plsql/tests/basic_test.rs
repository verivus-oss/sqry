//! Graph builder integration tests for Oracle PL/SQL plugin.

use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::{EdgeKind, TableWriteOp};
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::{NodeId, StringId};
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_oracle_plsql::OraclePlsqlPlugin;
use std::collections::HashMap;
use std::path::Path;

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn resolve_string(strings: &HashMap<u32, String>, id: StringId) -> Option<String> {
    strings.get(&id.index()).cloned()
}

fn build_node_lookup(staging: &StagingGraph) -> HashMap<NodeId, (String, NodeKind)> {
    let strings = build_string_lookup(staging);
    let mut nodes = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode {
            entry,
            expected_id: Some(node_id),
        } = op
        {
            let name = strings
                .get(&entry.name.index())
                .cloned()
                .unwrap_or_default();
            nodes.insert(*node_id, (name, entry.kind));
        }
    }
    nodes
}

fn build_staging(source: &[u8]) -> StagingGraph {
    let plugin = OraclePlsqlPlugin::default();
    let tree = plugin.parse_ast(source).expect("parse PL/SQL");
    let builder = plugin.graph_builder().expect("graph builder");
    let mut staging = StagingGraph::new();
    builder
        .build_graph(&tree, source, Path::new("test.pkb"), &mut staging)
        .expect("build graph");
    staging
}

fn find_node(staging: &StagingGraph, name: &str, kind: NodeKind) -> bool {
    let nodes = build_node_lookup(staging);
    nodes
        .values()
        .any(|(node_name, node_kind)| node_name == name && *node_kind == kind)
}

fn count_table_reads(staging: &StagingGraph, table: &str) -> usize {
    let strings = build_string_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                kind: EdgeKind::TableRead { table_name, .. },
                ..
            } = op
            {
                return resolve_string(&strings, *table_name);
            }
            None
        })
        .filter(|name| name == table)
        .count()
}

fn count_table_writes(staging: &StagingGraph, table: &str, op: TableWriteOp) -> usize {
    let strings = build_string_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op_entry| {
            if let StagingOp::AddEdge {
                kind:
                    EdgeKind::TableWrite {
                        table_name,
                        operation,
                        ..
                    },
                ..
            } = op_entry
                && *operation == op
            {
                return resolve_string(&strings, *table_name);
            }
            None
        })
        .filter(|name| name == table)
        .count()
}

#[test]
fn test_package_and_procedure_nodes() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE do_work IS
  BEGIN
    NULL;
  END do_work;
END test_pkg;
"#;

    let staging = build_staging(source);

    assert!(find_node(&staging, "test_pkg", NodeKind::Module));
    assert!(find_node(&staging, "test_pkg.do_work", NodeKind::Function));
}

#[test]
fn test_table_edges_from_procedure() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE do_work IS
  BEGIN
    SELECT * FROM employees;
    INSERT INTO audit_log VALUES (1);
  END do_work;
END test_pkg;
"#;

    let staging = build_staging(source);

    assert_eq!(count_table_reads(&staging, "employees"), 1);
    assert_eq!(
        count_table_writes(&staging, "audit_log", TableWriteOp::Insert),
        1
    );
}
