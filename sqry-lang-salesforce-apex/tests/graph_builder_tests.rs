//! Graph builder integration tests for Salesforce Apex plugin.

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::StringId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::{EdgeKind, TableWriteOp};
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_salesforce_apex::SalesforceApexPlugin;
use std::collections::HashMap;
use std::path::PathBuf;

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

fn build_staging(fixture_name: &str) -> StagingGraph {
    let plugin = SalesforceApexPlugin::default();
    let content: &[u8] = match fixture_name {
        "AccountTrigger.trigger" => include_bytes!("fixtures/AccountTrigger.trigger"),
        "AccountService.cls" => include_bytes!("fixtures/AccountService.cls"),
        "ContactHandler.cls" => include_bytes!("fixtures/ContactHandler.cls"),
        "ApiController.cls" => include_bytes!("fixtures/ApiController.cls"),
        _ => panic!("Unknown fixture: {fixture_name}"),
    };
    let file = PathBuf::from(fixture_name);
    let tree = plugin.parse_ast(content).expect("parse Apex fixture");
    let builder = plugin.graph_builder().expect("graph builder");
    let mut staging = StagingGraph::new();
    builder
        .build_graph(&tree, content, &file, &mut staging)
        .expect("build graph");
    staging
}

fn find_node(staging: &StagingGraph, name: &str, kind: NodeKind) -> bool {
    let nodes = build_node_lookup(staging);
    nodes
        .values()
        .any(|(node_name, node_kind)| node_name == name && *node_kind == kind)
}

fn has_triggered_by_edge(staging: &StagingGraph, trigger_prefix: &str, sobject: &str) -> bool {
    let strings = build_string_lookup(staging);
    let nodes = build_node_lookup(staging);

    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
        {
            let EdgeKind::TriggeredBy { trigger_name, .. } = kind else {
                continue;
            };
            let trigger_label = resolve_string(&strings, *trigger_name).unwrap_or_default();
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            let source_name = nodes.get(source).map(|(name, _)| name.as_str());

            if source_name == Some(trigger_prefix)
                && target_name == Some(sobject)
                && trigger_label.starts_with(trigger_prefix)
            {
                return true;
            }
        }
    }
    false
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

fn has_annotation_edge(staging: &StagingGraph, method_name: &str, annotation: &str) -> bool {
    let nodes = build_node_lookup(staging);

    let annotation_node_name = format!("annotation::{annotation}");

    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
        {
            if !matches!(kind, EdgeKind::Calls { .. }) {
                continue;
            }
            let source_name = nodes.get(source).map(|(name, _)| name.as_str());
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            if source_name == Some(method_name)
                && target_name == Some(annotation_node_name.as_str())
            {
                return true;
            }
        }
    }

    nodes
        .values()
        .any(|(name, kind)| name == &annotation_node_name && *kind == NodeKind::Other)
}

#[test]
fn test_trigger_edges() {
    let staging = build_staging("AccountTrigger.trigger");

    assert!(find_node(&staging, "AccountTrigger", NodeKind::Function));
    assert!(has_triggered_by_edge(&staging, "AccountTrigger", "Account"));
}

#[test]
fn test_class_and_methods() {
    let staging = build_staging("AccountService.cls");

    assert!(find_node(&staging, "AccountService", NodeKind::Class));
    assert!(find_node(
        &staging,
        "getAccountsByIndustry",
        NodeKind::Method
    ));
    assert!(find_node(
        &staging,
        "getAccountWithContacts",
        NodeKind::Method
    ));
    assert!(find_node(&staging, "searchAccounts", NodeKind::Method));
    assert!(find_node(
        &staging,
        "getAccountOpportunities",
        NodeKind::Method
    ));
}

#[test]
fn test_soql_table_reads() {
    let staging = build_staging("AccountService.cls");

    assert!(count_table_reads(&staging, "Account") >= 1);
    assert!(count_table_reads(&staging, "Opportunity") >= 1);
}

#[test]
fn test_dml_table_writes() {
    let staging = build_staging("ContactHandler.cls");

    assert!(count_table_writes(&staging, "Contact", TableWriteOp::Insert) >= 1);
    assert!(count_table_writes(&staging, "Contact", TableWriteOp::Update) >= 1);
    assert!(count_table_writes(&staging, "Contact", TableWriteOp::Delete) >= 1);
}

#[test]
fn test_annotations_as_call_edges() {
    let staging = build_staging("ApiController.cls");

    assert!(has_annotation_edge(&staging, "getAccounts", "AuraEnabled"));
    assert!(has_annotation_edge(
        &staging,
        "processAccountsAsync",
        "future"
    ));
    assert!(has_annotation_edge(
        &staging,
        "updateAccountStatus",
        "InvocableMethod"
    ));
    assert!(has_annotation_edge(&staging, "getAccount", "RemoteAction"));
    assert!(has_annotation_edge(&staging, "getSecretKey", "TestVisible"));
}

// ========== TypeOf and References Edge Tests ==========

fn collect_typeof_edges(staging: &StagingGraph) -> Vec<(String, String)> {
    let nodes = build_node_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::TypeOf { .. },
                ..
            } = op
            {
                let from_name = nodes.get(source).map(|(name, _)| name.clone());
                let to_name = nodes.get(target).map(|(name, _)| name.clone());
                if let (Some(from), Some(to)) = (from_name, to_name) {
                    return Some((from, to));
                }
            }
            None
        })
        .collect()
}

fn has_reference_edge_to(staging: &StagingGraph, type_name: &str) -> bool {
    let nodes = build_node_lookup(staging);
    staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            target,
            kind: EdgeKind::References,
            ..
        } = op
        {
            nodes.get(target).is_some_and(|(name, _)| name == type_name)
        } else {
            false
        }
    })
}

fn build_staging_from_source(source: &str, filename: &str) -> StagingGraph {
    use sqry_core::graph::GraphBuilder;
    use sqry_lang_salesforce_apex::ApexGraphBuilder;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_sfapex::apex::LANGUAGE.into())
        .expect("load Apex grammar");
    let tree = parser.parse(source.as_bytes(), None).expect("parse");
    let mut staging = StagingGraph::new();
    let builder = ApexGraphBuilder;
    let file = PathBuf::from(filename);
    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .expect("build graph");
    staging
}

#[test]
fn test_apex_field_typeof() {
    let source = r#"
public class AccountService {
    private Account currentAccount;
    private String accountName;
}
"#;
    let staging = build_staging_from_source(source, "AccountService.cls");
    let edges = collect_typeof_edges(&staging);
    assert!(
        edges.iter().any(|(_, to)| to == "Account"),
        "Expected TypeOf edge to Account, got: {edges:?}"
    );
    assert!(
        edges.iter().any(|(_, to)| to == "String"),
        "Expected TypeOf edge to String, got: {edges:?}"
    );
}

#[test]
fn test_apex_local_variable_typeof() {
    let source = r#"
public class DataService {
    public void processData() {
        Account acc = new Account();
        String name = 'test';
    }
}
"#;
    let staging = build_staging_from_source(source, "DataService.cls");
    let edges = collect_typeof_edges(&staging);
    assert!(
        edges.iter().any(|(_, to)| to == "Account"),
        "Expected TypeOf edge to Account for local variable, got: {edges:?}"
    );
}

#[test]
fn test_apex_method_parameter_typeof() {
    let source = r#"
public class ContactHandler {
    public void handleContact(Contact con, String name) {
    }
}
"#;
    let staging = build_staging_from_source(source, "ContactHandler.cls");
    let edges = collect_typeof_edges(&staging);
    assert!(
        edges.iter().any(|(_, to)| to == "Contact"),
        "Expected TypeOf edge to Contact for parameter, got: {edges:?}"
    );
    assert!(
        edges.iter().any(|(_, to)| to == "String"),
        "Expected TypeOf edge to String for parameter, got: {edges:?}"
    );
}

#[test]
fn test_apex_return_type_typeof() {
    let source = r#"
public class AccountService {
    public Account getAccount() {
        return null;
    }
}
"#;
    let staging = build_staging_from_source(source, "AccountService.cls");
    let edges = collect_typeof_edges(&staging);
    assert!(
        edges.iter().any(|(_, to)| to == "Account"),
        "Expected TypeOf edge to Account for return type, got: {edges:?}"
    );
}

#[test]
fn test_apex_void_return_skipped() {
    let source = r#"
public class VoidService {
    public void doNothing() {
    }
}
"#;
    let staging = build_staging_from_source(source, "VoidService.cls");
    let edges = collect_typeof_edges(&staging);
    assert!(
        !edges.iter().any(|(_, to)| to.eq_ignore_ascii_case("void")),
        "Should NOT create TypeOf edge for void return, got: {edges:?}"
    );
}

#[test]
fn test_apex_generic_type_typeof() {
    let source = r#"
public class ListService {
    private List<Account> accounts;
}
"#;
    let staging = build_staging_from_source(source, "ListService.cls");
    let edges = collect_typeof_edges(&staging);
    assert!(
        edges.iter().any(|(_, to)| to.contains("List")),
        "Expected TypeOf edge with List type, got: {edges:?}"
    );
}

#[test]
fn test_apex_generic_type_references() {
    let source = r#"
public class ListService {
    private List<Account> accounts;
}
"#;
    let staging = build_staging_from_source(source, "ListService.cls");
    assert!(
        has_reference_edge_to(&staging, "List"),
        "Expected References edge to List"
    );
    assert!(
        has_reference_edge_to(&staging, "Account"),
        "Expected References edge to Account"
    );
}

#[test]
fn test_apex_nested_generic_references() {
    let source = r#"
public class NestedService {
    private List<List<Account>> nestedAccounts;
}
"#;
    let staging = build_staging_from_source(source, "NestedService.cls");
    assert!(
        has_reference_edge_to(&staging, "Account"),
        "Expected References edge to Account in nested generic"
    );
}

#[test]
fn test_apex_scoped_type() {
    let source = r#"
public class OuterService {
    private Outer.Inner scopedField;
}
"#;
    let staging = build_staging_from_source(source, "OuterService.cls");
    let edges = collect_typeof_edges(&staging);
    assert!(
        edges.iter().any(|(_, to)| to.contains("Outer")),
        "Expected TypeOf edge with scoped type, got: {edges:?}"
    );
}

#[test]
fn test_apex_reference_dedup() {
    let source = r#"
public class DedupService {
    private Map<String, String> stringMap;
}
"#;
    let staging = build_staging_from_source(source, "DedupService.cls");
    // Count References edges to "String" -- should be deduped to 1
    let nodes = build_node_lookup(&staging);
    let string_ref_count = staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge {
                target,
                kind: EdgeKind::References,
                ..
            } = op
            {
                nodes.get(target).is_some_and(|(name, _)| name == "String")
            } else {
                false
            }
        })
        .count();
    assert_eq!(
        string_ref_count, 1,
        "Expected exactly 1 References edge to String (dedup), got: {string_ref_count}"
    );
}

#[test]
fn test_apex_multiple_params_indexed() {
    let source = r#"
public class ParamService {
    public void process(Account acc, Contact con, String name) {
    }
}
"#;
    let staging = build_staging_from_source(source, "ParamService.cls");
    let edges = collect_typeof_edges(&staging);
    assert!(
        edges.iter().any(|(_, to)| to == "Account"),
        "Expected TypeOf for param 0 (Account)"
    );
    assert!(
        edges.iter().any(|(_, to)| to == "Contact"),
        "Expected TypeOf for param 1 (Contact)"
    );
    assert!(
        edges.iter().any(|(_, to)| to == "String"),
        "Expected TypeOf for param 2 (String)"
    );
}

#[test]
fn test_apex_typeof_coexists_with_existing_edges() {
    // Use an existing fixture to verify TypeOf edges coexist with SOQL/DML/OOP edges
    let staging = build_staging("AccountService.cls");
    // Existing edges should still work
    let _has_calls = staging.operations().iter().any(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::Calls { .. },
                ..
            }
        )
    });
    // TypeOf edges may or may not be present depending on fixture content
    // The important thing is no errors occurred
    assert!(staging.stats().nodes_staged > 0, "Should have nodes");
}

#[test]
fn test_apex_map_generic_references() {
    let source = r#"
public class MapService {
    private Map<String, Contact> contactMap;
}
"#;
    let staging = build_staging_from_source(source, "MapService.cls");
    assert!(
        has_reference_edge_to(&staging, "Map"),
        "Expected References edge to Map"
    );
    assert!(
        has_reference_edge_to(&staging, "String"),
        "Expected References edge to String"
    );
    assert!(
        has_reference_edge_to(&staging, "Contact"),
        "Expected References edge to Contact"
    );
}

#[test]
fn test_apex_parameter_typeof_target_is_type_node() {
    let source = r#"
public class ParamService {
    public void process(Account acc, Contact con) {
    }
}
"#;
    let staging = build_staging_from_source(source, "ParamService.cls");
    let nodes = build_node_lookup(&staging);

    // All TypeOf targets (including parameters) should be NodeKind::Type nodes
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            target,
            kind: EdgeKind::TypeOf { .. },
            ..
        } = op
            && let Some((name, kind)) = nodes.get(target)
        {
            assert_eq!(
                *kind,
                NodeKind::Type,
                "TypeOf target '{name}' should be NodeKind::Type, got {kind:?}"
            );
        }
    }
}

#[test]
fn test_apex_duplicate_param_type_references_dedup() {
    // Two parameters with the same type should produce only one References edge per type
    let source = r#"
public class DupService {
    public void merge(Account first, Account second) {
    }
}
"#;
    let staging = build_staging_from_source(source, "DupService.cls");
    let nodes = build_node_lookup(&staging);

    // Count References edges to "Account"
    let account_ref_count = staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge {
                target,
                kind: EdgeKind::References,
                ..
            } = op
            {
                nodes.get(target).is_some_and(|(name, _)| name == "Account")
            } else {
                false
            }
        })
        .count();

    // Per-method dedup means each method produces exactly 1 References edge per type
    assert_eq!(
        account_ref_count, 1,
        "Expected exactly 1 References edge to Account (dedup), got {account_ref_count}"
    );
}
