// Phase 4 Tests: Type Aliases and Generics
// Tests for Go 1.9+ type aliases and Go 1.18+ generics

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::node::Language;
use sqry_core::graph::unified::build::StagingGraph;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_lang_go::relations::GoGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_go_file(content: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .expect("Failed to set Go language");
    parser
        .parse(content.as_bytes(), None)
        .expect("Failed to parse Go code")
}

fn build_test_graph(source: &str, filename: &str) -> StagingGraph {
    let tree = parse_go_file(source);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    let file = Path::new(filename);

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build_graph should succeed");

    staging
}

fn build_node_display_name_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let sqry_core::graph::unified::build::StagingOp::AddNode { entry, expected_id } = op
            {
                let expected_id = expected_id.as_ref()?;
                let node_idx = expected_id.index();
                let name = staging.resolve_node_display_name(Language::Go, entry)?;
                Some((node_idx, name))
            } else {
                None
            }
        })
        .collect()
}

fn collect_edges_by_kind<F>(staging: &StagingGraph, predicate: F) -> Vec<(String, String)>
where
    F: Fn(&EdgeKind) -> bool,
{
    let node_names = build_node_display_name_lookup(staging);
    let mut edges = Vec::new();

    for op in staging.operations() {
        if let sqry_core::graph::unified::build::StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
            && predicate(kind)
        {
            let from_name = node_names
                .get(&source.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", source.index()));

            let to_name = node_names
                .get(&target.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", target.index()));

            edges.push((from_name, to_name));
        }
    }

    edges
}

fn collect_typeof_edges_by_context(
    staging: &StagingGraph,
    expected_context: TypeOfContext,
) -> Vec<(String, String)> {
    collect_edges_by_kind(staging, |kind| {
        matches!(
            kind,
            EdgeKind::TypeOf {
                context: Some(ctx),
                ..
            } if *ctx == expected_context
        )
    })
}

fn collect_reference_edges(staging: &StagingGraph) -> Vec<(String, String)> {
    collect_edges_by_kind(staging, |kind| matches!(kind, EdgeKind::References))
}

// ============================================================================
// Type Alias Tests (4 tests)
// ============================================================================

#[test]
fn test_type_alias_simple() {
    let source = r#"package main

type UserID = int
type Status = string
"#;

    let staging = build_test_graph(source, "test.go");
    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::TypeParameter);
    let ref_edges = collect_reference_edges(&staging);

    // TypeOf edges: alias → target (TypeParameter context for type-level relationship)
    assert!(
        typeof_edges
            .iter()
            .any(|(s, t)| s == "main.UserID" && t == "int"),
        "Expected TypeOf: UserID→int"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(s, t)| s == "main.Status" && t == "string"),
        "Expected TypeOf: Status→string"
    );

    // Reference edges
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.UserID" && t == "int"),
        "Expected Reference: UserID→int"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Status" && t == "string"),
        "Expected Reference: Status→string"
    );
}

#[test]
fn test_type_alias_pointer() {
    let source = r#"package main

type User struct {
    ID int
}

type UserPtr = *User
"#;

    let staging = build_test_graph(source, "test.go");
    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::TypeParameter);
    let ref_edges = collect_reference_edges(&staging);

    // TypeOf edge: UserPtr → *User
    assert!(
        typeof_edges
            .iter()
            .any(|(s, t)| s == "main.UserPtr" && t == "*User"),
        "Expected TypeOf: UserPtr→*User"
    );

    // Reference edge: UserPtr → User (extracts from pointer)
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.UserPtr" && t == "User"),
        "Expected Reference: UserPtr→User"
    );
}

#[test]
fn test_type_alias_function() {
    let source = r#"package main

import "context"

type HandlerFunc = func(context.Context) error
"#;

    let staging = build_test_graph(source, "test.go");
    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::TypeParameter);
    let ref_edges = collect_reference_edges(&staging);

    // TypeOf edge: HandlerFunc → func(context.Context) error
    assert!(
        typeof_edges.iter().any(|(s, t)| s == "main.HandlerFunc"
            && t.contains("context.Context")
            && t.contains("error")),
        "Expected TypeOf: HandlerFunc→func signature"
    );

    // Reference edges for nested types
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.HandlerFunc" && t == "context.Context"),
        "Expected Reference: HandlerFunc→context.Context"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.HandlerFunc" && t == "error"),
        "Expected Reference: HandlerFunc→error"
    );
}

#[test]
fn test_type_alias_complex() {
    let source = r#"package main

type User struct {
    ID int
}

type Cache = map[string]*User
"#;

    let staging = build_test_graph(source, "test.go");
    let typeof_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::TypeParameter);
    let ref_edges = collect_reference_edges(&staging);

    // TypeOf edge: Cache → map[string]*User
    assert!(
        typeof_edges
            .iter()
            .any(|(s, t)| s == "main.Cache" && t.contains("map") && t.contains("string")),
        "Expected TypeOf: Cache→map[string]*User"
    );

    // Reference edges for nested types
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Cache" && t == "string"),
        "Expected Reference: Cache→string"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Cache" && t == "User"),
        "Expected Reference: Cache→User"
    );
}

// ============================================================================
// Generic Type Tests (4 tests)
// ============================================================================

#[test]
fn test_generic_single_param() {
    let source = r#"package main

type List[T any] struct {
    items []T
}
"#;

    let staging = build_test_graph(source, "test.go");
    let constraint_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Constraint);
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);

    // TypeParameter node: List.T created
    // TypeOf edge: List.T → any (Constraint context)
    assert!(
        constraint_edges
            .iter()
            .any(|(s, t)| s == "main.List.T" && t == "any"),
        "Expected TypeOf: List.T→any with Constraint context"
    );

    // Field TypeOf: items → []T
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.List" && t.contains("[]T")),
        "Expected Field TypeOf: List.items→[]T"
    );
}

#[test]
fn test_generic_multiple_params() {
    let source = r#"package main

type Map[K comparable, V any] struct {
    data map[K]V
}
"#;

    let staging = build_test_graph(source, "test.go");
    let constraint_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Constraint);

    // TypeParameter nodes: Map.K and Map.V
    assert!(
        constraint_edges
            .iter()
            .any(|(s, t)| s == "main.Map.K" && t == "comparable"),
        "Expected TypeOf: Map.K→comparable"
    );
    assert!(
        constraint_edges
            .iter()
            .any(|(s, t)| s == "main.Map.V" && t == "any"),
        "Expected TypeOf: Map.V→any"
    );
}

#[test]
fn test_generic_interface_constraint() {
    let source = r#"package main

import "io"

type Processor[T io.Reader] struct {
    input T
}
"#;

    let staging = build_test_graph(source, "test.go");
    let constraint_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Constraint);
    let ref_edges = collect_reference_edges(&staging);

    // TypeOf: Processor.T → io.Reader
    assert!(
        constraint_edges
            .iter()
            .any(|(s, t)| s == "main.Processor.T" && t == "io.Reader"),
        "Expected TypeOf: Processor.T→io.Reader"
    );

    // Reference edge to interface
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Processor.T" && t == "io.Reader"),
        "Expected Reference: Processor.T→io.Reader"
    );
}

#[test]
fn test_generic_union_constraint() {
    let source = r#"package main

type Number[T int | float64] struct {
    value T
}
"#;

    let staging = build_test_graph(source, "test.go");
    let constraint_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Constraint);
    let ref_edges = collect_reference_edges(&staging);

    // TypeOf edge for the full union
    assert!(
        constraint_edges
            .iter()
            .any(|(s, t)| s == "main.Number.T" && t.contains("int") && t.contains("float64")),
        "Expected TypeOf: Number.T→union with Constraint context"
    );

    // Reference edges for each variant
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Number.T" && t == "int"),
        "Expected Reference: Number.T→int"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Number.T" && t == "float64"),
        "Expected Reference: Number.T→float64"
    );
}

// ============================================================================
// Instantiated Generic Tests (4 tests)
// ============================================================================

#[test]
fn test_instantiated_variable() {
    let source = r#"package main

type List[T any] struct {
    items []T
}

type User struct {
    ID int
}

var users List[User]
"#;

    let staging = build_test_graph(source, "test.go");
    let var_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);
    let ref_edges = collect_reference_edges(&staging);

    // TypeOf: users → List[User]
    assert!(
        var_edges
            .iter()
            .any(|(s, t)| s == "main.users" && t.contains("List") && t.contains("User")),
        "Expected TypeOf: users→List[User]"
    );

    // Reference edges: List and User
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.users" && t == "List"),
        "Expected Reference: users→List"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.users" && t == "User"),
        "Expected Reference: users→User"
    );
}

#[test]
fn test_instantiated_field() {
    let source = r#"package main

type Map[K comparable, V any] struct {
    data map[K]V
}

type User struct {
    ID int
}

type Cache struct {
    data Map[string, User]
}
"#;

    let staging = build_test_graph(source, "test.go");
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    let ref_edges = collect_reference_edges(&staging);

    // Field TypeOf: Cache.data → Map[string, User]
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.Cache" && t.contains("Map")),
        "Expected Field TypeOf: Cache→Map[...]"
    );

    // Reference edges
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Cache" && t == "Map"),
        "Expected Reference: Cache→Map"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Cache" && t == "string"),
        "Expected Reference: Cache→string"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Cache" && t == "User"),
        "Expected Reference: Cache→User"
    );
}

#[test]
fn test_instantiated_parameter() {
    let source = r#"package main

type Map[K comparable, V any] struct {
    data map[K]V
}

type User struct {
    ID int
}

func process(cache Map[string, User]) {
}
"#;

    let staging = build_test_graph(source, "test.go");
    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    let ref_edges = collect_reference_edges(&staging);

    // Parameter TypeOf: process → Map[string, User]
    assert!(
        param_edges
            .iter()
            .any(|(s, t)| s == "main.process" && t.contains("Map")),
        "Expected Parameter TypeOf: process→Map[...]"
    );

    // Reference edges
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.process" && t == "Map"),
        "Expected Reference: process→Map"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.process" && t == "string"),
        "Expected Reference: process→string"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.process" && t == "User"),
        "Expected Reference: process→User"
    );
}

#[test]
fn test_nested_generic() {
    let source = r#"package main

type List[T any] struct {
    items []T
}

type User struct {
    ID int
}

var data map[string]List[User]
"#;

    let staging = build_test_graph(source, "test.go");
    let var_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);
    let ref_edges = collect_reference_edges(&staging);

    // TypeOf: data → map[string]List[User]
    assert!(
        var_edges
            .iter()
            .any(|(s, t)| s == "main.data" && t.contains("map") && t.contains("List")),
        "Expected TypeOf: data→map[string]List[User]"
    );

    // All reference edges
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.data" && t == "string"),
        "Expected Reference: data→string"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.data" && t == "List"),
        "Expected Reference: data→List"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.data" && t == "User"),
        "Expected Reference: data→User"
    );
}

// ============================================================================
// Edge Case Tests (3 tests)
// ============================================================================

#[test]
fn test_generic_function_alias() {
    let source = r#"package main

type Transform[T any] func(T) T
"#;

    let staging = build_test_graph(source, "test.go");
    let constraint_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Constraint);

    // TypeParameter: Transform.T → any
    assert!(
        constraint_edges
            .iter()
            .any(|(s, t)| s == "main.Transform.T" && t == "any"),
        "Expected TypeOf: Transform.T→any"
    );
}

#[test]
fn test_empty_type_params() {
    // Edge case: Empty type parameters (should not crash)
    let source = r#"package main

type Invalid[] struct {
    data int
}
"#;

    let staging = build_test_graph(source, "test.go");
    // Should not crash, just skip empty params
    let constraint_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Constraint);

    // No constraint edges expected for invalid syntax
    assert!(
        !constraint_edges
            .iter()
            .any(|(s, _)| s.starts_with("main.Invalid.")),
        "Should not create TypeParameter for empty params"
    );
}

#[test]
fn test_anonymous_constraint() {
    let source = r#"package main

type Handler[T interface{ Close() error }] struct {
    resource T
}
"#;

    let staging = build_test_graph(source, "test.go");
    let constraint_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Constraint);

    // TypeOf: Handler.T → anonymous interface
    assert!(
        constraint_edges
            .iter()
            .any(|(s, t)| s == "main.Handler.T" && t.contains("interface")),
        "Expected TypeOf: Handler.T→interface constraint"
    );
}

// ============================================================================
// Fix Verification Tests (3 tests)
// Tests for the 3 blocking findings from Codex review iteration 1
// ============================================================================

/// Helper: Collect all Export edges
fn collect_export_edges(staging: &StagingGraph) -> Vec<(String, String)> {
    collect_edges_by_kind(staging, |kind| matches!(kind, EdgeKind::Exports { .. }))
}

/// Helper: Check if a node with the given Go-native qualified display name exists.
fn node_exists(staging: &StagingGraph, display_name: &str) -> bool {
    let node_names = build_node_display_name_lookup(staging);
    node_names.values().any(|name| name == display_name)
}

/// Test Fix 1 (HIGH): Type parameter references resolve to qualified nodes
///
/// **Before**: Fields using type parameter `T` created Reference edges to bare `T`,
/// leaving TypeParameter nodes (`main.List.T`) isolated.
///
/// **After**: Fields using `T` create Reference edges to qualified `main.List.T`,
/// properly connecting usage to declaration.
#[test]
fn test_fix1_type_param_qualified_references() {
    let source = r#"package main

type List[T any] struct {
    items []T
    head  *T
}
"#;

    let staging = build_test_graph(source, "test.go");
    let ref_edges = collect_reference_edges(&staging);

    // Fields don't get individual nodes; edges are from the struct
    // The struct should have Reference edges to the qualified type parameter
    let list_to_param_refs = ref_edges
        .iter()
        .filter(|(s, t)| s == "main.List" && t == "main.List.T")
        .count();

    // Should have 2 references (one for `items []T`, one for `head *T`)
    assert!(
        list_to_param_refs >= 2,
        "Expected at least 2 Reference edges: List→List.T (for items and head fields), got {}",
        list_to_param_refs
    );

    // Should NOT have bare `T` references
    assert!(
        !ref_edges.iter().any(|(_, t)| t == "T"),
        "Should not have bare 'T' references - all should be qualified as 'main.List.T'"
    );
}

/// Test Fix 2 (MEDIUM): Export edges are created for exported type aliases
///
/// **Before**: Type aliases didn't create Export edges, even if exported.
///
/// **After**: Exported type aliases (PublicAlias) create Export edges from module,
/// while private aliases do not.
#[test]
fn test_fix2_type_alias_export_edges() {
    let source = r#"package mylib

type PublicAlias = int
type privateAlias = string
"#;

    let staging = build_test_graph(source, "test.go");
    let export_edges = collect_export_edges(&staging);

    // PublicAlias should have export edge
    assert!(
        export_edges
            .iter()
            .any(|(s, t)| s == "mylib" && t == "mylib.PublicAlias"),
        "Expected Export: mylib→mylib.PublicAlias for exported type alias"
    );

    // privateAlias should NOT have export edge
    assert!(
        !export_edges.iter().any(|(_, t)| t == "mylib.privateAlias"),
        "Should not export private type alias"
    );
}

/// Test Fix 3 (MEDIUM): Generic type aliases with type parameters are handled
///
/// **Before**: Generic type aliases like `type Alias[T any] = []T` were treated as
/// non-generic; type parameters were ignored.
///
/// **After**: Generic type aliases process type parameters, creating TypeParameter
/// nodes and constraint edges. References in RHS use qualified parameter names.
#[test]
fn test_fix3_generic_type_alias_with_params() {
    let source = r#"package main

type GenericAlias[T any] = []T
"#;

    let staging = build_test_graph(source, "test.go");
    let constraint_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Constraint);

    // TypeParameter node should exist for GenericAlias.T
    assert!(
        node_exists(&staging, "main.GenericAlias.T"),
        "Expected TypeParameter node: main.GenericAlias.T"
    );

    // Constraint TypeOf edge should exist
    assert!(
        constraint_edges
            .iter()
            .any(|(s, t)| s == "main.GenericAlias.T" && t == "any"),
        "Expected TypeOf constraint: GenericAlias.T→any"
    );

    // The RHS `[]T` should reference the qualified parameter
    let ref_edges = collect_reference_edges(&staging);

    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.GenericAlias" && t == "main.GenericAlias.T"),
        "Expected Reference: GenericAlias→main.GenericAlias.T (qualified in RHS)"
    );
}

/// Test Fix 1 Enhancement: Type parameters in nested function return types
///
/// **Scenario**: Generic type alias with function type that returns the type parameter
/// **Before Fix**: `func() (T, error)` would create Reference to bare `T`
/// **After Fix**: References qualified `main.ResponseFunc.T`
#[test]
fn test_type_param_in_nested_function_returns() {
    let source = r#"package main

type ResponseFunc[T any] = func() (T, error)
"#;

    let staging = build_test_graph(source, "test.go");
    let ref_edges = collect_reference_edges(&staging);

    // The function return type should reference qualified type parameter
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.ResponseFunc" && t == "main.ResponseFunc.T"),
        "Expected Reference: ResponseFunc→main.ResponseFunc.T (qualified in function return)"
    );

    // Should NOT have bare `T` references
    assert!(
        !ref_edges.iter().any(|(_, t)| t == "T"),
        "Should not have bare 'T' references"
    );

    // Should also reference error
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.ResponseFunc" && t == "error"),
        "Expected Reference: ResponseFunc→error"
    );
}

/// Test Fix 2: Type parameters in generic interface method signatures
///
/// **Scenario**: Generic interface with method that uses the type parameter
/// **Before Fix**: `Get() T` would create Reference to bare `T`
/// **After Fix**: References qualified `main.Getter.T`
#[test]
fn test_type_param_in_interface_methods() {
    let source = r#"package main

type Getter[T any] interface {
    Get() T
    Set(T) error
}
"#;

    let staging = build_test_graph(source, "test.go");
    let ref_edges = collect_reference_edges(&staging);

    // Method return type should reference qualified type parameter
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Getter" && t == "main.Getter.T"),
        "Expected Reference: Getter→main.Getter.T (from Get() return type)"
    );

    // Should NOT have bare `T` references
    assert!(
        !ref_edges.iter().any(|(_, t)| t == "T"),
        "Should not have bare 'T' references in interface methods"
    );

    // Should also reference error
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Getter" && t == "error"),
        "Expected Reference: Getter→error (from Set parameter)"
    );
}

/// Test combined scenario: Type parameter in nested struct within alias
#[test]
fn test_type_param_in_nested_struct() {
    let source = r#"package main

type Wrapper[T any] = struct {
    Value T
    Handler func(T) error
}
"#;

    let staging = build_test_graph(source, "test.go");
    let ref_edges = collect_reference_edges(&staging);

    // Struct field and function parameter should both reference qualified type parameter
    let wrapper_to_t_refs = ref_edges
        .iter()
        .filter(|(s, t)| s == "main.Wrapper" && t == "main.Wrapper.T")
        .count();

    assert!(
        wrapper_to_t_refs >= 2,
        "Expected at least 2 References: Wrapper→Wrapper.T (for Value field and Handler parameter), got {}",
        wrapper_to_t_refs
    );

    // Should NOT have bare `T` references
    assert!(
        !ref_edges.iter().any(|(_, t)| t == "T"),
        "Should not have bare 'T' references in nested struct"
    );
}

/// Test Fix 3: Interface method return types with generic_type
///
/// **Scenario**: Generic interface method returning instantiated generic type
/// **Before Fix**: `Get() Result[T]` would create no TypeOf/Reference edges
/// **After Fix**: Creates edges for both Result and T
#[test]
fn test_interface_method_generic_return_type() {
    let source = r#"package main

type Result[T any] struct {
    Value T
    Error error
}

type Getter[T any] interface {
    Get() Result[T]
}
"#;

    let staging = build_test_graph(source, "test.go");
    let ref_edges = collect_reference_edges(&staging);

    // Interface should reference Result in method return type (unqualified)
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Getter" && t == "Result"),
        "Expected Reference: Getter→Result (from Get() return type)"
    );

    // Interface should reference qualified type parameter T used in Result[T]
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Getter" && t == "main.Getter.T"),
        "Expected Reference: Getter→main.Getter.T (from Result[T] type argument)"
    );

    // Should NOT have bare `T` references
    assert!(
        !ref_edges.iter().any(|(_, t)| t == "T"),
        "Should not have bare 'T' references in interface method return type"
    );
}

/// Test Fix 4: Type-set constraint qualification in negated_type/type_term
///
/// **Scenario**: Interface with type-set constraints using `~[]T` or `~T`
/// **Before Fix**: Bare `T` references created instead of qualified `main.Constraint.T`
/// **After Fix**: Type parameters in negated types properly qualified
#[test]
fn test_negated_type_constraint_qualification() {
    let source = r#"package main

type Constraint[T any] interface {
    ~[]T
}
"#;

    let staging = build_test_graph(source, "test.go");
    let ref_edges = collect_reference_edges(&staging);

    // Constraint should reference qualified type parameter T used in ~[]T
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Constraint" && t == "main.Constraint.T"),
        "Expected Reference: Constraint→main.Constraint.T (from ~[]T constraint)"
    );

    // Should NOT have bare `T` references
    assert!(
        !ref_edges.iter().any(|(_, t)| t == "T"),
        "Should not have bare 'T' references in negated type constraint"
    );
}

/// Test Fix 5: Interface literal generic_type return nodes
///
/// **Scenario**: Interface literal used in struct field with method returning generic type
/// **Before Fix**: `generic_type` and `type_union` not recognized in interface literal method returns
/// **After Fix**: Reference edges created for generic types in interface literal method signatures
#[test]
fn test_interface_literal_generic_type_returns() {
    let source = r#"package main

type Result[T any] struct {
    Value T
    Error error
}

type Wrapper[T any] struct {
    Getter interface {
        Get() Result[T]
    }
}
"#;

    let staging = build_test_graph(source, "test.go");
    let ref_edges = collect_reference_edges(&staging);

    // Wrapper should reference Result (from interface literal Get() return type)
    // Note: Interface literals don't create separate nodes - references are from the containing struct
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Wrapper" && t == "Result"),
        "Expected Reference: Wrapper→Result (from interface literal method return)"
    );

    // Wrapper should reference qualified type parameter Wrapper.T
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Wrapper" && t == "main.Wrapper.T"),
        "Expected Reference: Wrapper→main.Wrapper.T (from Result[T] in interface literal)"
    );

    // Should NOT have bare `T` references
    assert!(
        !ref_edges.iter().any(|(_, t)| t == "T"),
        "Should not have bare 'T' references in interface literal"
    );
}

/// Test Fix 6: Cross-parameter constraint qualification
///
/// **Scenario**: Type parameter constraint referencing another type parameter
/// **Before Fix**: `type Foo[U any, T ~[]U]` would create bare `U` reference
/// **After Fix**: Creates qualified reference `main.Foo.U`
#[test]
fn test_cross_parameter_constraint_qualification() {
    let source = r#"package main

type Foo[U any, T ~[]U] struct {}
"#;

    let staging = build_test_graph(source, "test.go");
    let ref_edges = collect_reference_edges(&staging);

    // T parameter should reference qualified U parameter
    assert!(
        ref_edges
            .iter()
            .any(|(s, t)| s == "main.Foo.T" && t == "main.Foo.U"),
        "Expected Reference: main.Foo.T→main.Foo.U (from ~[]U constraint)"
    );

    // Should NOT have bare `U` references
    assert!(
        !ref_edges.iter().any(|(_, t)| t == "U"),
        "Should not have bare 'U' references in type parameter constraint"
    );
}
