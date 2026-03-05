use sqry_core::graph::GraphBuilder;
/// Tests for TypeOf edges, Reference edges, and visibility metadata
use sqry_core::graph::unified::build::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_lang_typescript::relations::TypeScriptGraphBuilder;
use sqry_test_support::graph_helpers::build_string_lookup;
use std::path::Path;
use tree_sitter::Parser;

fn parse_typescript(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        .expect("Error loading TypeScript grammar");
    parser.parse(source, None).expect("Error parsing")
}

fn build_test_graph(source: &str, file_name: &str) -> StagingGraph {
    let tree = parse_typescript(source);
    let file = Path::new(file_name);
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    staging
}

/// Helper to collect edges of a specific kind with resolved names
fn collect_edges_by_kind(
    staging: &StagingGraph,
    edge_kind_filter: impl Fn(&EdgeKind) -> bool,
) -> Vec<(String, String)> {
    let strings = build_string_lookup(staging);

    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind,
                ..
            } = op
                && edge_kind_filter(kind)
            {
                let from_name = staging.operations().iter().find_map(|op| {
                    if let StagingOp::AddNode {
                        expected_id: Some(id),
                        entry,
                        ..
                    } = op
                        && id == source
                    {
                        return strings.get(&entry.name.index()).cloned();
                    }
                    None
                });
                let to_name = staging.operations().iter().find_map(|op| {
                    if let StagingOp::AddNode {
                        expected_id: Some(id),
                        entry,
                        ..
                    } = op
                        && id == target
                    {
                        return strings.get(&entry.name.index()).cloned();
                    }
                    None
                });
                return Some((from_name?, to_name?));
            }
            None
        })
        .collect()
}

#[test]
fn test_typeof_edge_for_typed_variable() {
    let source = r"
const user: User = getUser();
const count: number = 42;
";

    let staging = build_test_graph(source, "test_typeof.ts");

    // Find TypeOf edges
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "user" && typ == "User"),
        "Expected TypeOf edge from user to User, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "count" && typ == "number"),
        "Expected TypeOf edge from count to number, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_reference_edge_for_typed_variable() {
    let source = r"
const user: User = getUser();
";

    let staging = build_test_graph(source, "test_reference.ts");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "user" && typ == "User"),
        "Expected Reference edge from user to User, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_typeof_edge_with_generic_type() {
    let source = r"
const users: Array<User> = [];
const result: Promise<string> = fetch('/api');
";

    let staging = build_test_graph(source, "test_generic.ts");

    // Find TypeOf edges
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Should extract full generic type signature (Array<User>, Promise<string>)
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "users" && typ == "Array<User>"),
        "Expected TypeOf edge from users to Array<User>, got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "result" && typ == "Promise<string>"),
        "Expected TypeOf edge from result to Promise<string>, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_visibility_metadata_for_class_members() {
    let source = r"
class Widget {
    public getName(): string {
        return this.name;
    }

    private validate(): boolean {
        return true;
    }

    protected process(): void {
        this.validate();
    }
}
";

    let staging = build_test_graph(source, "test_visibility.ts");
    let strings = build_string_lookup(&staging);

    // Find methods and their visibility
    let methods: Vec<_> = staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                let name = strings.get(&entry.name.index())?;
                if name.contains('.') {
                    // It's a method
                    let visibility = entry
                        .visibility
                        .and_then(|id| strings.get(&id.index()).cloned());
                    return Some((name.clone(), visibility));
                }
            }
            None
        })
        .collect();

    assert!(
        methods
            .iter()
            .any(|(name, vis)| name == "Widget.getName" && vis.as_deref() == Some("public")),
        "Expected Widget.getName with public visibility, got: {:?}",
        methods
    );
    assert!(
        methods
            .iter()
            .any(|(name, vis)| name == "Widget.validate" && vis.as_deref() == Some("private")),
        "Expected Widget.validate with private visibility, got: {:?}",
        methods
    );
    assert!(
        methods
            .iter()
            .any(|(name, vis)| name == "Widget.process" && vis.as_deref() == Some("protected")),
        "Expected Widget.process with protected visibility, got: {:?}",
        methods
    );
}

#[test]
fn test_typeof_edge_with_array_syntax() {
    let source = r"
const items: string[] = [];
const matrix: number[][] = [];
";

    let staging = build_test_graph(source, "test_array.ts");

    // Find TypeOf edges
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Should extract full array type syntax (string[], number[][])
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "items" && typ == "string[]"),
        "Expected TypeOf edge from items to string[], got: {:?}",
        typeof_edges
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "matrix" && typ == "number[][]"),
        "Expected TypeOf edge from matrix to number[][], got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_export_clause_visibility() {
    let source = r"
function internalFunc() {
    return 'internal';
}

function publicFunc() {
    return 'public';
}

export { publicFunc };

export default internalFunc;
";

    let staging = build_test_graph(source, "test_export_clause.ts");
    let strings = build_string_lookup(&staging);

    // Find functions and their visibility
    let functions: Vec<_> = staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                let name = strings.get(&entry.name.index())?;
                // Top-level functions (no dot in name)
                if !name.contains('.') && !name.starts_with("_") {
                    let visibility = entry
                        .visibility
                        .and_then(|id| strings.get(&id.index()).cloned());
                    return Some((name.clone(), visibility));
                }
            }
            None
        })
        .collect();

    // publicFunc should be marked as public (from export clause)
    assert!(
        functions
            .iter()
            .any(|(name, vis)| name == "publicFunc" && vis.as_deref() == Some("public")),
        "Expected publicFunc with visibility=public from export clause, got: {:?}",
        functions
    );

    // internalFunc should also be public (from export default)
    assert!(
        functions
            .iter()
            .any(|(name, vis)| name == "internalFunc" && vis.as_deref() == Some("public")),
        "Expected internalFunc with visibility=public from export default, got: {:?}",
        functions
    );
}

#[test]
fn test_arrow_and_function_expression_parameters() {
    let source = r"
const arrowFn = (x: number, y: string) => {
    return x.toString() + y;
};

const funcExpr = function(a: boolean, b: number): string {
    return a ? b.toString() : '';
};
";

    let staging = build_test_graph(source, "test_arrow_params.ts");

    // Find TypeOf edges for parameters
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Arrow function parameters
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "x" && typ == "number"),
        "Expected TypeOf edge from arrow parameter x to number, got: {:?}",
        typeof_edges
    );

    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "y" && typ == "string"),
        "Expected TypeOf edge from arrow parameter y to string, got: {:?}",
        typeof_edges
    );

    // Function expression parameters
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "a" && typ == "boolean"),
        "Expected TypeOf edge from function expression parameter a to boolean, got: {:?}",
        typeof_edges
    );

    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "b" && typ == "number"),
        "Expected TypeOf edge from function expression parameter b to number, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_reexport_does_not_mark_local_as_exported() {
    let source = r"
// Local function with same name as re-export
function Foo() {
    return 'local';
}

// Re-export from another module
export { Foo } from './other';
";

    let staging = build_test_graph(source, "test_reexport.ts");
    let strings = build_string_lookup(&staging);

    // Find functions and their visibility
    let functions: Vec<_> = staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                let name = strings.get(&entry.name.index())?;
                // Top-level functions (no dot in name)
                if !name.contains('.') && name == "Foo" {
                    let visibility = entry
                        .visibility
                        .and_then(|id| strings.get(&id.index()).cloned());
                    return Some((name.clone(), visibility));
                }
            }
            None
        })
        .collect();

    // Local Foo should NOT be marked as exported (re-export doesn't count)
    assert!(
        functions
            .iter()
            .any(|(name, vis)| name == "Foo" && vis.is_none()),
        "Expected local Foo to have visibility=None (not exported due to re-export), got: {:?}",
        functions
    );
}

#[test]
fn test_function_type_annotation_creates_typeof_edge() {
    let source = r"
// Function type annotation
const handler: (x: number) => string = (x) => x.toString();

// Constructor type annotation
const factory: new (id: number) => Object = class { constructor(id: number) {} };
";

    let staging = build_test_graph(source, "test_function_types.ts");

    // Find TypeOf edges
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // handler should have TypeOf edge to full function type signature
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "handler" && typ == "(x: number) => string"),
        "Expected TypeOf edge from handler to (x: number) => string, got: {:?}",
        typeof_edges
    );

    // factory should have TypeOf edge to full constructor type signature
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "factory" && typ == "new (id: number) => Object"),
        "Expected TypeOf edge from factory to new (id: number) => Object, got: {:?}",
        typeof_edges
    );
}

#[test]
fn test_function_type_annotation_creates_reference_edges_for_nested_types() {
    let source = r"
const handler: (x: number, y: string) => boolean = (x, y) => true;
const factory: new (id: number) => User = class { constructor(id: number) {} };
";

    let staging = build_test_graph(source, "test_function_type_refs.ts");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // handler should have Reference edges to Function, number, string, and boolean
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "handler" && typ == "Function"),
        "Expected Reference edge from handler to Function, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "handler" && typ == "number"),
        "Expected Reference edge from handler to number (parameter type), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "handler" && typ == "string"),
        "Expected Reference edge from handler to string (parameter type), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "handler" && typ == "boolean"),
        "Expected Reference edge from handler to boolean (return type), got: {:?}",
        reference_edges
    );

    // factory should have Reference edges to Function, number, and User
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "factory" && typ == "Function"),
        "Expected Reference edge from factory to Function, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "factory" && typ == "number"),
        "Expected Reference edge from factory to number (parameter type), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "factory" && typ == "User"),
        "Expected Reference edge from factory to User (return type), got: {:?}",
        reference_edges
    );
}

#[test]
fn test_higher_order_function_type_extracts_nested_types() {
    let source = r"
const createHandler: () => (x: number) => string = () => (x) => x.toString();
const factory: () => (SomeType) = () => null as any;
";

    let staging = build_test_graph(source, "test_higher_order.ts");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // createHandler should have Reference edges to Function, number, and string
    // Including the nested function type's parameter and return types
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "createHandler" && typ == "Function"),
        "Expected Reference edge from createHandler to Function, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "createHandler" && typ == "number"),
        "Expected Reference edge from createHandler to number (nested function parameter), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "createHandler" && typ == "string"),
        "Expected Reference edge from createHandler to string (nested function return type), got: {:?}",
        reference_edges
    );

    // factory should have Reference edges to Function and SomeType (parenthesized)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "factory" && typ == "Function"),
        "Expected Reference edge from factory to Function, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "factory" && typ == "SomeType"),
        "Expected Reference edge from factory to SomeType (parenthesized return type), got: {:?}",
        reference_edges
    );
}

#[test]
fn test_object_type_extracts_property_types() {
    let source = r"
const user: { name: string; age: number; active: boolean } = { name: 'test', age: 42, active: true };
const config: { readonly id: string; optional?: number } = { id: '123' };
";

    let staging = build_test_graph(source, "test_object_types.ts");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // user should have Reference edges to all property types
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "user" && typ == "string"),
        "Expected Reference edge from user to string (property type), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "user" && typ == "number"),
        "Expected Reference edge from user to number (property type), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "user" && typ == "boolean"),
        "Expected Reference edge from user to boolean (property type), got: {:?}",
        reference_edges
    );

    // config should have Reference edges to property types (including readonly and optional)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "config" && typ == "string"),
        "Expected Reference edge from config to string (readonly property), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "config" && typ == "number"),
        "Expected Reference edge from config to number (optional property), got: {:?}",
        reference_edges
    );
}

#[test]
fn test_tuple_type_extracts_element_types() {
    let source = r"
const pair: [string, number] = ['test', 42];
const triple: [boolean, string, number] = [true, 'test', 42];
";

    let staging = build_test_graph(source, "test_tuple_types.ts");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // pair should have Reference edges to string and number
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "pair" && typ == "string"),
        "Expected Reference edge from pair to string (tuple element), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "pair" && typ == "number"),
        "Expected Reference edge from pair to number (tuple element), got: {:?}",
        reference_edges
    );

    // triple should have Reference edges to boolean, string, and number
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "triple" && typ == "boolean"),
        "Expected Reference edge from triple to boolean (tuple element), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "triple" && typ == "string"),
        "Expected Reference edge from triple to string (tuple element), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "triple" && typ == "number"),
        "Expected Reference edge from triple to number (tuple element), got: {:?}",
        reference_edges
    );
}

#[test]
fn test_conditional_type_extracts_all_branches() {
    let source = r"
type IsString<T> = T extends string ? number : boolean;
const result: IsString<string> = 42;
";

    let staging = build_test_graph(source, "test_conditional_types.ts");

    // Find Reference edges from the type alias
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // IsString should have Reference edges to string (check type), number (true branch), and boolean (false branch)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "IsString" && typ == "string"),
        "Expected Reference edge from IsString to string (extends check), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "IsString" && typ == "number"),
        "Expected Reference edge from IsString to number (true branch), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "IsString" && typ == "boolean"),
        "Expected Reference edge from IsString to boolean (false branch), got: {:?}",
        reference_edges
    );
}

#[test]
fn test_indexed_access_type_extracts_base_type() {
    let source = r"
type User = { name: string; age: number };
const userName: User['name'] = 'test';
";

    let staging = build_test_graph(source, "test_indexed_access.ts");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // userName should have Reference edge to User (base type of indexed access)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "userName" && typ == "User"),
        "Expected Reference edge from userName to User (indexed access base), got: {:?}",
        reference_edges
    );
}

#[test]
fn test_mapped_type_extracts_value_type() {
    let source = r"
type StringMap<T> = { [K in keyof T]: string };
type User = { name: string; age: number };
const mapped: StringMap<User> = { name: 'test', age: '42' };
";

    let staging = build_test_graph(source, "test_mapped_types.ts");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // StringMap should have Reference edge to string (mapped value type)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "StringMap" && typ == "string"),
        "Expected Reference edge from StringMap to string (mapped value type), got: {:?}",
        reference_edges
    );
}

#[test]
fn test_nested_complex_types() {
    let source = r"
const complex: { users: [string, number]; config: { enabled: boolean } } = {
    users: ['test', 42],
    config: { enabled: true }
};
";

    let staging = build_test_graph(source, "test_nested_complex.ts");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // complex should have Reference edges to all nested types
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "complex" && typ == "string"),
        "Expected Reference edge from complex to string (nested in tuple), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "complex" && typ == "number"),
        "Expected Reference edge from complex to number (nested in tuple), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "complex" && typ == "boolean"),
        "Expected Reference edge from complex to boolean (nested in object), got: {:?}",
        reference_edges
    );
}

#[test]
fn test_generic_type_arguments_extracted() {
    let source = r"
const users: Array<User> = [];
const result: Promise<Result> = fetch('/api');
const map: Map<string, number> = new Map();
";

    let staging = build_test_graph(source, "test_generic_args.ts");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // users should have Reference edges to both Array AND User
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "users" && typ == "Array"),
        "Expected Reference edge from users to Array (base type), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "users" && typ == "User"),
        "Expected Reference edge from users to User (type argument), got: {:?}",
        reference_edges
    );

    // result should have Reference edges to both Promise AND Result
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "result" && typ == "Promise"),
        "Expected Reference edge from result to Promise (base type), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "result" && typ == "Result"),
        "Expected Reference edge from result to Result (type argument), got: {:?}",
        reference_edges
    );

    // map should have Reference edges to Map, string, AND number
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "map" && typ == "Map"),
        "Expected Reference edge from map to Map (base type), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "map" && typ == "string"),
        "Expected Reference edge from map to string (first type argument), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "map" && typ == "number"),
        "Expected Reference edge from map to number (second type argument), got: {:?}",
        reference_edges
    );
}

#[test]
fn test_template_literal_type_extracts_type_params() {
    let source = r"
type Prefix<T> = `prefix-${T}`;
type EventName = `on${string}`;
const handler: `click-${string}` = 'click-test';
";

    let staging = build_test_graph(source, "test_template_literal.ts");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Prefix type alias should reference T from template substitution
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "Prefix" && typ == "T"),
        "Expected Reference edge from Prefix to T (template substitution), got: {:?}",
        reference_edges
    );

    // EventName should reference string
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "EventName" && typ == "string"),
        "Expected Reference edge from EventName to string (template substitution), got: {:?}",
        reference_edges
    );

    // handler should reference string
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "handler" && typ == "string"),
        "Expected Reference edge from handler to string (template substitution), got: {:?}",
        reference_edges
    );

    // Negative assertion: string fragments should NOT create type references
    // "prefix-", "on", "click-" are literal text, not type names
    assert!(
        !reference_edges
            .iter()
            .any(|(_, typ)| typ == "prefix-" || typ == "on" || typ == "click-"),
        "String fragments should NOT create type references, got: {:?}",
        reference_edges
    );
}

#[test]
fn test_tuple_rest_and_optional_types() {
    let source = r"
type RestTuple = [string, ...number[]];
type OptionalTuple = [string, number?];
const mixed: [boolean, ...string[], number?] = [true, 'a', 'b'];
";

    let staging = build_test_graph(source, "test_tuple_modifiers.ts");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // RestTuple should reference string and number (not "...number[]" as text)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "RestTuple" && typ == "string"),
        "Expected Reference edge from RestTuple to string, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "RestTuple" && typ == "number"),
        "Expected Reference edge from RestTuple to number (from rest type), got: {:?}",
        reference_edges
    );

    // OptionalTuple should reference string and number (not "number?" as text)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "OptionalTuple" && typ == "string"),
        "Expected Reference edge from OptionalTuple to string, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "OptionalTuple" && typ == "number"),
        "Expected Reference edge from OptionalTuple to number (from optional type), got: {:?}",
        reference_edges
    );

    // mixed should reference boolean, string, number
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "mixed" && typ == "boolean"),
        "Expected Reference edge from mixed to boolean, got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "mixed" && typ == "string"),
        "Expected Reference edge from mixed to string (from rest), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "mixed" && typ == "number"),
        "Expected Reference edge from mixed to number (from optional), got: {:?}",
        reference_edges
    );
}

#[test]
fn test_keyof_operator_in_mapped_types() {
    let source = r"
type Readonly<T> = { readonly [K in keyof T]: T[K] };
type Partial<T> = { [K in keyof T]?: T[K] };
";

    let staging = build_test_graph(source, "test_keyof_operator.ts");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Readonly should reference T (from keyof T constraint and from T[K] value)
    let readonly_t_count = reference_edges
        .iter()
        .filter(|(var, typ)| var == "Readonly" && typ == "T")
        .count();
    assert!(
        readonly_t_count >= 1,
        "Expected at least one Reference edge from Readonly to T (from keyof T or T[K]), got: {:?}",
        reference_edges
    );

    // Partial should reference T (from keyof T constraint and from T[K] value)
    let partial_t_count = reference_edges
        .iter()
        .filter(|(var, typ)| var == "Partial" && typ == "T")
        .count();
    assert!(
        partial_t_count >= 1,
        "Expected at least one Reference edge from Partial to T (from keyof T or T[K]), got: {:?}",
        reference_edges
    );
}

#[test]
fn test_mapped_type_constraint_without_value_reference() {
    let source = r"
type Keys<T> = { [K in keyof T]: never };
type Props<T> = { readonly [K in keyof T]: K };
";

    let staging = build_test_graph(source, "test_mapped_constraint_only.ts");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Keys should reference T even though value type is never (from keyof T constraint)
    let keys_t_count = reference_edges
        .iter()
        .filter(|(var, typ)| var == "Keys" && typ == "T")
        .count();
    assert!(
        keys_t_count >= 1,
        "Expected at least one Reference edge from Keys to T (from keyof T constraint), got: {:?}",
        reference_edges
    );

    // Props should reference T from both constraint (keyof T) and value (K references T via lookup)
    let props_t_count = reference_edges
        .iter()
        .filter(|(var, typ)| var == "Props" && typ == "T")
        .count();
    assert!(
        props_t_count >= 1,
        "Expected at least one Reference edge from Props to T (from keyof T constraint), got: {:?}",
        reference_edges
    );
}

#[test]
fn test_object_type_method_signatures() {
    let source = r"
type WithMethods = {
    bar(x: number): string;
    baz<T>(item: T): boolean;
};

type Callable = {
    (x: number): string;
};
";

    let staging = build_test_graph(source, "test_method_signatures.ts");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // WithMethods should reference parameter and return types from method signatures
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "WithMethods" && typ == "number"),
        "Expected Reference edge from WithMethods to number (method parameter), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "WithMethods" && typ == "string"),
        "Expected Reference edge from WithMethods to string (method return type), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "WithMethods" && typ == "boolean"),
        "Expected Reference edge from WithMethods to boolean (generic method return type), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "WithMethods" && typ == "T"),
        "Expected Reference edge from WithMethods to T (generic method parameter), got: {:?}",
        reference_edges
    );

    // Callable should reference call signature parameter and return types
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "Callable" && typ == "number"),
        "Expected Reference edge from Callable to number (call signature parameter), got: {:?}",
        reference_edges
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "Callable" && typ == "string"),
        "Expected Reference edge from Callable to string (call signature return type), got: {:?}",
        reference_edges
    );
}
