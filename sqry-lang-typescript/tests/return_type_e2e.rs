//! End-to-end integration tests for TypeScript return type extraction.
//!
//! These tests verify return type extraction works through the graph builder.

#[path = "support/mod.rs"]
mod support;

use sqry_core::graph::Language;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::NodeEntry;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_typescript::TypeScriptPlugin;
use std::collections::HashMap;
use support::unique_ts_path;

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn find_node_entry<'a>(
    staging: &'a StagingGraph,
    canonical_name: &str,
    kind: NodeKind,
) -> Option<&'a NodeEntry> {
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == kind
            && staging.resolve_node_canonical_name(entry) == Some(canonical_name)
        {
            return Some(entry);
        }
    }
    None
}

fn assert_node_display_name(
    staging: &StagingGraph,
    entry: &NodeEntry,
    expected_display_name: &str,
) {
    let display_name = staging.resolve_node_display_name(Language::TypeScript, entry);
    assert_eq!(
        display_name.as_deref(),
        Some(expected_display_name),
        "Incorrect display name for node"
    );
}

fn get_signature(entry: &NodeEntry, strings: &HashMap<u32, String>) -> Option<String> {
    entry
        .signature
        .and_then(|id| strings.get(&id.index()).cloned())
}

fn build_graph_from_source(source: &[u8], label: &str) -> StagingGraph {
    let plugin = TypeScriptPlugin::default();
    let file = unique_ts_path(label);
    let tree = plugin.parse_ast(source).expect("Failed to parse AST");
    let builder = plugin.graph_builder().expect("graph builder");
    let mut staging = StagingGraph::new();

    builder
        .build_graph(&tree, source, &file, &mut staging)
        .expect("build graph");

    staging
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Integration test covers a broad set of return type patterns."
)]
fn test_return_type_extraction_integration() {
    // Comprehensive TypeScript source with various return type patterns
    let source = br#"
// Simple return types
function getString(): string {
    return "hello";
}

function getNumber(): number {
    return 42;
}

// Generic return types
function getPromise(): Promise<User> {
    return Promise.resolve({} as User);
}

function getArray(): Array<string> {
    return ["a", "b"];
}

function getDeferred():
    Promise<string> {
    return Promise.resolve("ok");
}

// Union types
function getNullable(): string | null {
    return null;
}

function getMultiUnion(): string | number | boolean {
    return "mixed";
}

// Type parameters
function identity<T>(x: T): T {
    return x;
}

function pair<A, B>(a: A, b: B): [A, B] {
    return [a, b];
}

// Method return types
class MyClass {
    public getPublic(): boolean {
        return true;
    }

    private getPrivate(): void {
        console.log("private");
    }

    protected getProtected(): this {
        return this;
    }
}

// Complex nested types
function getComplex(): { id: number, data: Array<{ name: string }> } {
    return { id: 1, data: [{ name: "test" }] };
}

// Function without return type
function noReturnType() {
    console.log("no annotation");
}
"#;

    let staging = build_graph_from_source(source, "return_type_extraction_integration");
    let strings = build_string_lookup(&staging);

    // Test simple return types
    let get_string =
        find_node_entry(&staging, "getString", NodeKind::Function).expect("getString not found");
    assert_eq!(
        get_signature(get_string, &strings).as_deref(),
        Some("string")
    );

    let get_number =
        find_node_entry(&staging, "getNumber", NodeKind::Function).expect("getNumber not found");
    assert_eq!(
        get_signature(get_number, &strings).as_deref(),
        Some("number")
    );

    // Test generic return types
    let get_promise =
        find_node_entry(&staging, "getPromise", NodeKind::Function).expect("getPromise not found");
    assert_eq!(
        get_signature(get_promise, &strings).as_deref(),
        Some("Promise<User>")
    );

    let get_array =
        find_node_entry(&staging, "getArray", NodeKind::Function).expect("getArray not found");
    assert_eq!(
        get_signature(get_array, &strings).as_deref(),
        Some("Array<string>")
    );

    // Test leading/trailing whitespace normalization
    let get_deferred = find_node_entry(&staging, "getDeferred", NodeKind::Function)
        .expect("getDeferred not found");
    assert_eq!(
        get_signature(get_deferred, &strings).as_deref(),
        Some("Promise<string>")
    );

    // Test union types
    let get_nullable = find_node_entry(&staging, "getNullable", NodeKind::Function)
        .expect("getNullable not found");
    assert_eq!(
        get_signature(get_nullable, &strings).as_deref(),
        Some("string | null")
    );

    let get_multi_union = find_node_entry(&staging, "getMultiUnion", NodeKind::Function)
        .expect("getMultiUnion not found");
    assert_eq!(
        get_signature(get_multi_union, &strings).as_deref(),
        Some("string | number | boolean")
    );

    // Test type parameters
    let identity =
        find_node_entry(&staging, "identity", NodeKind::Function).expect("identity not found");
    assert_eq!(get_signature(identity, &strings).as_deref(), Some("T"));

    let pair = find_node_entry(&staging, "pair", NodeKind::Function).expect("pair not found");
    assert_eq!(get_signature(pair, &strings).as_deref(), Some("[A, B]"));

    // Test method return types (canonical graph names plus TypeScript display names)
    let get_public = find_node_entry(&staging, "MyClass::getPublic", NodeKind::Method)
        .expect("getPublic not found");
    assert_node_display_name(&staging, get_public, "MyClass.getPublic");
    assert_eq!(
        get_signature(get_public, &strings).as_deref(),
        Some("boolean")
    );

    let get_private = find_node_entry(&staging, "MyClass::getPrivate", NodeKind::Method)
        .expect("getPrivate not found");
    assert_node_display_name(&staging, get_private, "MyClass.getPrivate");
    assert_eq!(
        get_signature(get_private, &strings).as_deref(),
        Some("void")
    );

    let get_protected = find_node_entry(&staging, "MyClass::getProtected", NodeKind::Method)
        .expect("getProtected not found");
    assert_node_display_name(&staging, get_protected, "MyClass.getProtected");
    assert_eq!(
        get_signature(get_protected, &strings).as_deref(),
        Some("this")
    );

    // Test complex nested types (preserves full type string)
    let get_complex =
        find_node_entry(&staging, "getComplex", NodeKind::Function).expect("getComplex not found");
    let type_str = get_signature(get_complex, &strings).expect("getComplex signature");
    assert!(type_str.contains("id"));
    assert!(type_str.contains("number"));
    assert!(type_str.contains("data"));
    assert!(type_str.contains("Array"));

    // Test function without return type annotation
    let no_return_type = find_node_entry(&staging, "noReturnType", NodeKind::Function)
        .expect("noReturnType not found");
    assert_eq!(get_signature(no_return_type, &strings), None);
}

#[test]
fn test_return_type_with_arrow_functions() {
    let source = br#"
const arrowSimple = (): string => "hello";
const arrowGeneric = (): Promise<number> => Promise.resolve(42);
const arrowUnion = (): string | number => "test";
const arrowNoType = () => "implicit";
"#;

    let staging = build_graph_from_source(source, "return_type_arrow_functions");
    let strings = build_string_lookup(&staging);

    let arrow_simple = find_node_entry(&staging, "arrowSimple", NodeKind::Function)
        .expect("arrowSimple not found");
    assert_eq!(
        get_signature(arrow_simple, &strings).as_deref(),
        Some("string")
    );

    let arrow_generic = find_node_entry(&staging, "arrowGeneric", NodeKind::Function)
        .expect("arrowGeneric not found");
    assert_eq!(
        get_signature(arrow_generic, &strings).as_deref(),
        Some("Promise<number>")
    );

    let arrow_union =
        find_node_entry(&staging, "arrowUnion", NodeKind::Function).expect("arrowUnion not found");
    assert_eq!(
        get_signature(arrow_union, &strings).as_deref(),
        Some("string | number")
    );

    let arrow_no_type = find_node_entry(&staging, "arrowNoType", NodeKind::Function)
        .expect("arrowNoType not found");
    assert_eq!(get_signature(arrow_no_type, &strings), None);
}

#[test]
fn test_return_type_does_not_extract_for_non_functions() {
    let source = br"
class MyClass {}
interface MyInterface {}
enum MyEnum { A, B }
const myVar = 42;
";

    let staging = build_graph_from_source(source, "return_type_non_functions");
    let strings = build_string_lookup(&staging);

    let class_node =
        find_node_entry(&staging, "MyClass", NodeKind::Class).expect("MyClass not found");
    assert_eq!(get_signature(class_node, &strings), None);

    let interface_node = find_node_entry(&staging, "MyInterface", NodeKind::Interface)
        .expect("MyInterface not found");
    assert_eq!(get_signature(interface_node, &strings), None);

    let enum_node = find_node_entry(&staging, "MyEnum", NodeKind::Enum).expect("MyEnum not found");
    assert_eq!(get_signature(enum_node, &strings), None);

    let variable_node =
        find_node_entry(&staging, "myVar", NodeKind::Variable).expect("myVar not found");
    assert_eq!(get_signature(variable_node, &strings), None);
}
