//! TypeOf and Reference edge tests for Oracle PL/SQL plugin.
//!
//! Tests TypeOf edges (full type signatures) and Reference edges (nested type names)
//! for procedure parameters, function return types, variable declarations,
//! `%TYPE`/`%ROWTYPE` references, and user-defined types.

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::kind::{EdgeKind, TypeOfContext};
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_oracle_plsql::OraclePlsqlPlugin;
use std::collections::HashMap;
use std::path::Path;

// ============================================================================
// Test Helper Functions
// ============================================================================

/// Build a node lookup mapping `NodeId` to `(name, kind)`.
fn build_node_lookup(staging: &StagingGraph) -> HashMap<NodeId, (String, NodeKind)> {
    let mut nodes = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode {
            entry,
            expected_id: Some(node_id),
        } = op
        {
            let name = staging
                .resolve_node_canonical_name(entry)
                .map(str::to_owned)
                .unwrap_or_default();
            nodes.insert(*node_id, (name, entry.kind));
        }
    }
    nodes
}

/// Parse PL/SQL source into a `StagingGraph`.
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

/// Collect `TypeOf` edges as `(source_name, type_name, context)`.
fn collect_typeof_edges(staging: &StagingGraph) -> Vec<(String, String, Option<TypeOfContext>)> {
    let nodes = build_node_lookup(staging);
    let mut result = Vec::new();

    for op in staging.operations() {
        if let StagingOp::AddEdge {
            kind: EdgeKind::TypeOf { context, .. },
            source,
            target,
            ..
        } = op
        {
            let source_name = nodes
                .get(source)
                .map(|(n, _)| n.clone())
                .unwrap_or_default();
            let target_name = nodes
                .get(target)
                .map(|(n, _)| n.clone())
                .unwrap_or_default();
            result.push((source_name, target_name, *context));
        }
    }

    result
}

/// Collect `TypeOf` edges filtered by context.
fn collect_typeof_edges_by_context(
    staging: &StagingGraph,
    context: TypeOfContext,
) -> Vec<(String, String)> {
    collect_typeof_edges(staging)
        .into_iter()
        .filter_map(|(source, target, ctx)| {
            if ctx == Some(context) {
                Some((source, target))
            } else {
                None
            }
        })
        .collect()
}

/// Collect `References` edges as `(source_name, target_name)`.
fn collect_reference_edges(staging: &StagingGraph) -> Vec<(String, String)> {
    let nodes = build_node_lookup(staging);
    let mut result = Vec::new();

    for op in staging.operations() {
        if let StagingOp::AddEdge {
            kind: EdgeKind::References,
            source,
            target,
            ..
        } = op
        {
            let source_name = nodes
                .get(source)
                .map(|(n, _)| n.clone())
                .unwrap_or_default();
            let target_name = nodes
                .get(target)
                .map(|(n, _)| n.clone())
                .unwrap_or_default();
            result.push((source_name, target_name));
        }
    }

    result
}

/// Count TypeOf edges in the staging graph.
fn count_typeof_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::TypeOf { .. },
                    ..
                }
            )
        })
        .count()
}

/// Check that a node with the given name exists.
fn find_node(staging: &StagingGraph, name: &str, kind: NodeKind) -> bool {
    let nodes = build_node_lookup(staging);
    nodes
        .values()
        .any(|(node_name, node_kind)| node_name == name && *node_kind == kind)
}

// ============================================================================
// Parameter Type Tests
// ============================================================================

#[test]
fn test_procedure_parameter_builtin_types() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE hire_employee(p_name VARCHAR2, p_salary NUMBER, p_hire_date DATE) IS
  BEGIN
    NULL;
  END hire_employee;
END test_pkg;
"#;

    let staging = build_staging(source);

    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    // Should have TypeOf edges for each parameter
    let type_names: Vec<&str> = param_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        type_names.contains(&"VARCHAR2"),
        "Should have VARCHAR2 parameter type, got: {param_edges:?}"
    );
    assert!(
        type_names.contains(&"NUMBER"),
        "Should have NUMBER parameter type, got: {param_edges:?}"
    );
    assert!(
        type_names.contains(&"DATE"),
        "Should have DATE parameter type, got: {param_edges:?}"
    );

    // Built-in types should NOT generate References edges
    let ref_edges = collect_reference_edges(&staging);
    let ref_targets: Vec<&str> = ref_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        !ref_targets.contains(&"VARCHAR2"),
        "Built-in VARCHAR2 should not generate References edge"
    );
    assert!(
        !ref_targets.contains(&"NUMBER"),
        "Built-in NUMBER should not generate References edge"
    );
}

#[test]
fn test_procedure_parameter_in_out_modes() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE update_record(p_id IN NUMBER, p_name IN OUT VARCHAR2, p_result OUT BOOLEAN) IS
  BEGIN
    NULL;
  END update_record;
END test_pkg;
"#;

    let staging = build_staging(source);

    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    let type_names: Vec<&str> = param_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        type_names.contains(&"NUMBER"),
        "IN parameter should have TypeOf edge, got: {param_edges:?}"
    );
    assert!(
        type_names.contains(&"VARCHAR2"),
        "IN OUT parameter should have TypeOf edge, got: {param_edges:?}"
    );
    assert!(
        type_names.contains(&"BOOLEAN"),
        "OUT parameter should have TypeOf edge, got: {param_edges:?}"
    );
}

// ============================================================================
// Function Return Type Tests
// ============================================================================

#[test]
fn test_function_return_type() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  FUNCTION get_salary(p_emp_id NUMBER) RETURN NUMBER IS
  BEGIN
    RETURN 0;
  END get_salary;
END test_pkg;
"#;

    let staging = build_staging(source);

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);

    assert!(
        !return_edges.is_empty(),
        "Should have at least one Return TypeOf edge, got: {return_edges:?}"
    );

    let type_names: Vec<&str> = return_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        type_names.contains(&"NUMBER"),
        "Function should have NUMBER return type, got: {return_edges:?}"
    );
}

#[test]
fn test_function_return_type_varchar2() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  FUNCTION get_name(p_id NUMBER) RETURN VARCHAR2 IS
  BEGIN
    RETURN 'test';
  END get_name;
END test_pkg;
"#;

    let staging = build_staging(source);

    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);

    let type_names: Vec<&str> = return_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        type_names.contains(&"VARCHAR2"),
        "Function should have VARCHAR2 return type, got: {return_edges:?}"
    );
}

// ============================================================================
// Variable Declaration Tests
// ============================================================================

#[test]
fn test_variable_declarations_builtin_types() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE do_work IS
    v_name VARCHAR2(100);
    v_count NUMBER;
    v_active BOOLEAN;
  BEGIN
    NULL;
  END do_work;
END test_pkg;
"#;

    let staging = build_staging(source);

    let var_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);

    let type_names: Vec<&str> = var_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        type_names.contains(&"VARCHAR2"),
        "Should have VARCHAR2 variable type, got: {var_edges:?}"
    );
    assert!(
        type_names.contains(&"NUMBER"),
        "Should have NUMBER variable type, got: {var_edges:?}"
    );
    assert!(
        type_names.contains(&"BOOLEAN"),
        "Should have BOOLEAN variable type, got: {var_edges:?}"
    );
}

#[test]
fn test_variable_declaration_with_default() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE do_work IS
    v_count NUMBER := 0;
    v_name VARCHAR2(100) := 'default';
  BEGIN
    NULL;
  END do_work;
END test_pkg;
"#;

    let staging = build_staging(source);

    let var_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);

    // Should still extract types even with defaults
    let type_names: Vec<&str> = var_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        type_names.contains(&"NUMBER"),
        "Should extract NUMBER type from declaration with default, got: {var_edges:?}"
    );
}

// ============================================================================
// %TYPE and %ROWTYPE Tests
// ============================================================================

#[test]
fn test_pct_type_variable_declaration() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE do_work IS
    v_name employees.name%TYPE;
  BEGIN
    NULL;
  END do_work;
END test_pkg;
"#;

    let staging = build_staging(source);

    let var_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);

    // TypeOf edge should point to the full %TYPE reference
    let type_names: Vec<&str> = var_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        type_names.iter().any(|t| t.contains("%TYPE")),
        "Should have %TYPE TypeOf edge, got: {var_edges:?}"
    );

    // References edge should point to the table name
    let ref_edges = collect_reference_edges(&staging);
    let ref_targets: Vec<&str> = ref_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        ref_targets.contains(&"employees"),
        "Should have References edge to 'employees' table, got: {ref_edges:?}"
    );
}

#[test]
fn test_pct_rowtype_variable_declaration() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE do_work IS
    v_emp employees%ROWTYPE;
  BEGIN
    NULL;
  END do_work;
END test_pkg;
"#;

    let staging = build_staging(source);

    let var_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);

    // TypeOf edge should point to the full %ROWTYPE reference
    let type_names: Vec<&str> = var_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        type_names.iter().any(|t| t.contains("%ROWTYPE")),
        "Should have %ROWTYPE TypeOf edge, got: {var_edges:?}"
    );

    // References edge should point to the table/cursor name
    let ref_edges = collect_reference_edges(&staging);
    let ref_targets: Vec<&str> = ref_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        ref_targets.contains(&"employees"),
        "Should have References edge to 'employees' for %ROWTYPE, got: {ref_edges:?}"
    );
}

// ============================================================================
// User-Defined Type Tests
// ============================================================================

#[test]
fn test_user_defined_type_generates_references() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE do_work IS
    v_rec employee_record;
  BEGIN
    NULL;
  END do_work;
END test_pkg;
"#;

    let staging = build_staging(source);

    let var_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);

    // TypeOf edge for user-defined type
    let type_names: Vec<&str> = var_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        type_names.contains(&"employee_record"),
        "Should have TypeOf edge for user-defined type, got: {var_edges:?}"
    );

    // References edge for user-defined type
    let ref_edges = collect_reference_edges(&staging);
    let ref_targets: Vec<&str> = ref_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        ref_targets.contains(&"employee_record"),
        "Should have References edge for user-defined type, got: {ref_edges:?}"
    );
}

#[test]
fn test_user_defined_type_in_parameter() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE do_work(p_rec IN employee_record) IS
  BEGIN
    NULL;
  END do_work;
END test_pkg;
"#;

    let staging = build_staging(source);

    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    // TypeOf edge for user-defined parameter type
    let type_names: Vec<&str> = param_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        type_names.contains(&"employee_record"),
        "Should have TypeOf edge for user-defined parameter type, got: {param_edges:?}"
    );

    // References edge for user-defined type
    let ref_edges = collect_reference_edges(&staging);
    let ref_targets: Vec<&str> = ref_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        ref_targets.contains(&"employee_record"),
        "Should have References edge for user-defined parameter type, got: {ref_edges:?}"
    );
}

// ============================================================================
// Multiple Parameters Test
// ============================================================================

#[test]
fn test_multiple_parameters() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE process(p_id NUMBER, p_name VARCHAR2, p_date DATE, p_active BOOLEAN) IS
  BEGIN
    NULL;
  END process;
END test_pkg;
"#;

    let staging = build_staging(source);

    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    // Should have 4 parameter TypeOf edges
    assert!(
        param_edges.len() >= 4,
        "Should have at least 4 parameter TypeOf edges, got {}: {param_edges:?}",
        param_edges.len()
    );
}

// ============================================================================
// Coexistence with Existing Edges Tests
// ============================================================================

#[test]
fn test_typeof_coexists_with_table_edges() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE process_employee(p_id NUMBER, p_name VARCHAR2) IS
    v_salary NUMBER;
  BEGIN
    SELECT salary INTO v_salary FROM employees WHERE id = p_id;
    INSERT INTO audit_log VALUES (p_id, SYSDATE);
  END process_employee;
END test_pkg;
"#;

    let staging = build_staging(source);

    // TypeOf edges should exist
    let typeof_count = count_typeof_edges(&staging);
    assert!(
        typeof_count >= 3,
        "Should have at least 3 TypeOf edges (2 params + 1 var), got {typeof_count}"
    );

    // Table edges should still exist
    let has_table_read = staging.operations().iter().any(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::TableRead { .. },
                ..
            }
        )
    });
    assert!(has_table_read, "Should still have TableRead edges");

    let has_table_write = staging.operations().iter().any(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::TableWrite { .. },
                ..
            }
        )
    });
    assert!(has_table_write, "Should still have TableWrite edges");

    // Function node should still exist
    assert!(
        find_node(&staging, "test_pkg::process_employee", NodeKind::Function),
        "Function node should still exist"
    );
}

#[test]
fn test_typeof_coexists_with_call_edges() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  FUNCTION get_count RETURN NUMBER IS
  BEGIN
    RETURN 0;
  END get_count;

  PROCEDURE do_work(p_id NUMBER) IS
  BEGIN
    NULL;
  END do_work;
END test_pkg;
"#;

    let staging = build_staging(source);

    // TypeOf edges should exist for both parameter and return type
    let typeof_count = count_typeof_edges(&staging);
    assert!(
        typeof_count >= 2,
        "Should have at least 2 TypeOf edges (1 param + 1 return), got {typeof_count}"
    );

    // Both function/procedure nodes should still exist
    assert!(
        find_node(&staging, "test_pkg::get_count", NodeKind::Function),
        "get_count function should still exist"
    );
    assert!(
        find_node(&staging, "test_pkg::do_work", NodeKind::Function),
        "do_work procedure should still exist"
    );
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn test_pct_type_in_parameter() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE do_work(p_name IN employees.name%TYPE) IS
  BEGIN
    NULL;
  END do_work;
END test_pkg;
"#;

    let staging = build_staging(source);

    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    // Should extract the %TYPE parameter
    let type_names: Vec<&str> = param_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        type_names.iter().any(|t| t.contains("%TYPE")),
        "Should have %TYPE parameter TypeOf edge, got: {param_edges:?}"
    );

    // Should have References edge to the table
    let ref_edges = collect_reference_edges(&staging);
    let ref_targets: Vec<&str> = ref_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        ref_targets.contains(&"employees"),
        "Should have References edge to 'employees', got: {ref_edges:?}"
    );
}

#[test]
fn test_constant_variable_declaration() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE do_work IS
    c_max CONSTANT NUMBER := 100;
  BEGIN
    NULL;
  END do_work;
END test_pkg;
"#;

    let staging = build_staging(source);

    let var_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);

    let type_names: Vec<&str> = var_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        type_names.contains(&"NUMBER"),
        "Should extract type from CONSTANT declaration, got: {var_edges:?}"
    );
}

#[test]
fn test_multiline_parameter_list() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE hire_employee(
    p_name VARCHAR2,
    p_salary NUMBER,
    p_dept_id INTEGER
  ) IS
  BEGIN
    NULL;
  END hire_employee;
END test_pkg;
"#;

    let staging = build_staging(source);

    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    let type_names: Vec<&str> = param_edges.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        type_names.contains(&"VARCHAR2"),
        "Should extract VARCHAR2 from multiline params, got: {param_edges:?}"
    );
    assert!(
        type_names.contains(&"NUMBER"),
        "Should extract NUMBER from multiline params, got: {param_edges:?}"
    );
    assert!(
        type_names.contains(&"INTEGER"),
        "Should extract INTEGER from multiline params, got: {param_edges:?}"
    );
}

#[test]
fn test_empty_source_no_typeof_edges() {
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE do_nothing IS
  BEGIN
    NULL;
  END do_nothing;
END test_pkg;
"#;

    let staging = build_staging(source);

    // Procedure without params or vars should have no TypeOf edges
    let typeof_count = count_typeof_edges(&staging);
    assert_eq!(
        typeof_count, 0,
        "Procedure with no params/vars should have 0 TypeOf edges, got {typeof_count}"
    );
}

// ============================================================================
// Comma-in-parentheses regression tests
// ============================================================================

#[test]
fn test_plsql_param_number_with_precision() {
    // NUMBER(10,2) contains a comma inside parens — must NOT split into two params
    let source = b"
CREATE OR REPLACE PROCEDURE calc_total(
    p_amount IN NUMBER(10,2),
    p_tax    IN NUMBER
) IS
BEGIN
    NULL;
END;
";
    let staging = build_staging(source);
    let params = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    // Should have exactly 2 parameters, not 3
    // (PL/SQL TypeOf edges use callable as source, param name in metadata)
    assert_eq!(
        params.len(),
        2,
        "NUMBER(10,2) should be treated as a single parameter, got {params:?}"
    );

    // Both parameters should have NUMBER-based types
    assert!(
        params
            .iter()
            .all(|(_, ty)| ty.to_uppercase().contains("NUMBER")),
        "Both params should have NUMBER types, got {params:?}"
    );
}

#[test]
fn test_plsql_param_varchar2_with_size() {
    // VARCHAR2(100) has parentheses but no comma — should still work
    let source = b"
CREATE OR REPLACE PROCEDURE process_name(
    p_first  IN VARCHAR2(100),
    p_last   IN VARCHAR2(200),
    p_age    IN NUMBER(3,0)
) IS
BEGIN
    NULL;
END;
";
    let staging = build_staging(source);
    let params = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    assert_eq!(
        params.len(),
        3,
        "Should have exactly 3 parameters, got {params:?}"
    );
}

// ============================================================================
// Per-source References dedup test
// ============================================================================

#[test]
fn test_plsql_reference_dedup_per_callable() {
    // Two parameters with the same user-defined type should produce only ONE References edge
    // per callable, not two.
    let source = br#"
CREATE OR REPLACE PACKAGE BODY test_pkg AS
  PROCEDURE process_pair(p_first employee_record, p_second employee_record) IS
  BEGIN
    NULL;
  END process_pair;
END test_pkg;
"#;
    let staging = build_staging(source);

    // Should have 2 TypeOf(Parameter) edges (one per param)
    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    assert_eq!(
        param_edges.len(),
        2,
        "Should have 2 parameter TypeOf edges, got {param_edges:?}"
    );

    // Should have only 1 References edge to employee_record (deduped within the callable)
    let ref_edges = collect_reference_edges(&staging);
    let employee_refs: Vec<_> = ref_edges
        .iter()
        .filter(|(_, t)| t == "employee_record")
        .collect();
    assert_eq!(
        employee_refs.len(),
        1,
        "Should have exactly 1 References edge to employee_record (dedup), got {employee_refs:?}"
    );
}
