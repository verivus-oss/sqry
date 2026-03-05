/// Integration tests for TypeScript `GraphBuilder`
/// Run with: cargo test -p sqry-lang-typescript --test `graph_builder_tests`
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::{EdgeKind, HttpMethod};
use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
use sqry_lang_typescript::relations::TypeScriptGraphBuilder;
use sqry_test_support::graph_helpers::{assert_has_call_edge, collect_call_edges};
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_typescript(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        .expect("Error loading TypeScript grammar");
    parser.parse(source, None).expect("Error parsing")
}

#[test]
fn graph_builder_extracts_function_calls() {
    let source = r"
function helper(): number {
    return 1;
}

function entry(): number {
    const result = helper();
    return result;
}
";

    let tree = parse_typescript(source);
    let file = Path::new("test_graph_builder_extracts_function_calls.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let stats = staging.stats();
    let node_count = stats.nodes_staged;
    let edge_count = stats.edges_staged;

    assert!(
        node_count >= 2,
        "Expected at least 2 nodes (helper, entry), got {node_count}"
    );
    assert!(
        edge_count >= 1,
        "Expected at least 1 call edge (entry->helper), got {edge_count}"
    );

    let call_edges = collect_call_edges(&staging);
    let call_edge_count = call_edges.len();
    assert!(
        call_edge_count > 0,
        "Expected at least 1 call edge, got {call_edge_count}"
    );

    assert_has_call_edge(&staging, "entry", "helper");
}

#[test]
fn graph_builder_handles_method_calls() {
    let source = r"
class Widget {
    helper(): number {
        return 42;
    }

    process(): number {
        return this.helper();
    }
}
";

    let tree = parse_typescript(source);
    let file = Path::new("test_methods.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "Widget.process", "Widget.helper");
}

#[test]
fn graph_builder_handles_arrow_functions() {
    let source = r"
const helper = (): number => 1;

const entry = (): number => {
    return helper();
};
";

    let tree = parse_typescript(source);
    let file = Path::new("test_arrow.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "entry", "helper");
}

#[test]
fn graph_builder_handles_async_functions() {
    let source = r#"
async function fetchData(): Promise<string> {
    return "data";
}

async function process(): Promise<string> {
    const data = await fetchData();
    return data;
}
"#;

    let tree = parse_typescript(source);
    let file = Path::new("test_async.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let async_call = assert_has_call_edge(&staging, "process", "fetchData");
    assert!(
        async_call.is_async,
        "Expected async call to have is_async=true metadata"
    );
}

#[test]
fn graph_builder_handles_namespace_functions() {
    let source = r"
namespace Utils {
    function helper(): number {
        return 1;
    }

    function process(): number {
        return helper();
    }
}
";

    let tree = parse_typescript(source);
    let file = Path::new("test_namespace.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "Utils.process", "helper");
}

#[test]
fn graph_builder_handles_top_level_calls() {
    let source = r#"
function bootstrap(): string {
    return "initialized";
}

// Top-level call
const result = bootstrap();
"#;

    let tree = parse_typescript(source);
    let file = Path::new("test_toplevel.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "<module>", "bootstrap");
}

#[test]
fn graph_builder_handles_constructor_calls() {
    let source = r"
function createWidget() {
    return new Widget();
}
";

    let tree = parse_typescript(source);
    let file = Path::new("test_constructor.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    // Should have constructor call edge
    let stats = staging.stats();
    assert!(stats.edges_staged >= 1, "Expected constructor call edge");
}

#[test]
fn graph_builder_handles_interface_implementations() {
    let source = r"
interface Calculator {
    add(x: number): number;
}

class SimpleCalculator implements Calculator {
    add(x: number): number {
        return x + 1;
    }

    process(x: number): number {
        return this.add(x);
    }
}
";

    let tree = parse_typescript(source);
    let file = Path::new("test_interface.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "SimpleCalculator.process", "SimpleCalculator.add");
}

#[test]
fn graph_builder_handles_generic_functions() {
    let source = r"
function identity<T>(value: T): T {
    return value;
}

function useGeneric(): number {
    return identity<number>(42);
}
";

    let tree = parse_typescript(source);
    let file = Path::new("test_generic.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "useGeneric", "identity");
}

#[test]
fn graph_builder_extracts_multiple_calls_in_function() {
    let source = r"
function helper1(): number {
    return 1;
}

function helper2(): number {
    return 2;
}

function caller(): number {
    const a = helper1();
    const b = helper2();
    return a + b;
}
";

    let tree = parse_typescript(source);
    let file = Path::new("test_multiple.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let call_edges = collect_call_edges(&staging);

    // Should have at least 2 call sites: caller->helper1, caller->helper2
    let call_edge_count = call_edges.len();
    assert!(
        call_edge_count >= 2,
        "Expected at least 2 call edges, got {call_edge_count}"
    );

    assert_has_call_edge(&staging, "caller", "helper1");
    assert_has_call_edge(&staging, "caller", "helper2");
}

#[test]
fn graph_builder_detects_typescript_imports() {
    let source = r"
import React from 'react';
import { Component, Props } from 'react';
import * as Utils from './utils';
import type { TypeDef } from './types';

class MyComponent extends Component {
    render() {
        return null;
    }
}
";

    let tree = parse_typescript(source);
    let file = Path::new("test_graph_builder_detects_typescript_imports.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    // The staging graph should have collected import operations
    let stats = staging.stats();
    // Verify staging graph captured operations
    assert!(
        stats.edges_staged > 0,
        "Expected staged edges for TypeScript imports"
    );
}

// ============================================================================
// EXPORT EDGE TESTS (Wave 1 Team 2)
// ============================================================================

/// Helper to count edges of a specific kind from staging operations
fn count_export_edges(staging: &StagingGraph) -> usize {
    use sqry_core::graph::unified::EdgeKind;
    use sqry_core::graph::unified::build::StagingOp;
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Exports { .. },
                    ..
                }
            )
        })
        .count()
}

fn count_inherits_edges(staging: &StagingGraph) -> usize {
    use sqry_core::graph::unified::EdgeKind;
    use sqry_core::graph::unified::build::StagingOp;
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Inherits,
                    ..
                }
            )
        })
        .count()
}

fn count_implements_edges(staging: &StagingGraph) -> usize {
    use sqry_core::graph::unified::EdgeKind;
    use sqry_core::graph::unified::build::StagingOp;
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Implements,
                    ..
                }
            )
        })
        .count()
}

#[test]
fn graph_builder_handles_default_export() {
    let source = r"
export default function main() {
    return 42;
}
";
    let tree = parse_typescript(source);
    let file = Path::new("test_default_export.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 1,
        "Expected at least 1 export edge for default export, got {export_count}"
    );
}

#[test]
fn graph_builder_handles_named_exports() {
    let source = r"
function helper() { return 1; }
const value = 42;

export { helper, value };
";
    let tree = parse_typescript(source);
    let file = Path::new("test_named_exports.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 2,
        "Expected at least 2 export edges for named exports, got {export_count}"
    );
}

#[test]
fn graph_builder_handles_aliased_exports() {
    use sqry_core::graph::unified::EdgeKind;
    use sqry_core::graph::unified::build::StagingOp;

    let source = r"
function internalHelper() { return 1; }

export { internalHelper as helper };
";
    let tree = parse_typescript(source);
    let file = Path::new("test_aliased_exports.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 1,
        "Expected at least 1 export edge for aliased export, got {export_count}"
    );

    // Verify the alias is captured in the export edge
    let has_alias = staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            kind: EdgeKind::Exports { alias, .. },
            ..
        } = op
        {
            alias.is_some()
        } else {
            false
        }
    });
    assert!(has_alias, "Expected export edge to have alias");
}

#[test]
fn graph_builder_handles_re_exports() {
    use sqry_core::graph::unified::EdgeKind;
    use sqry_core::graph::unified::build::StagingOp;
    use sqry_core::graph::unified::edge::ExportKind;

    let source = r#"
export { foo } from "./module";
export { bar as baz } from "./other";
"#;
    let tree = parse_typescript(source);
    let file = Path::new("test_re_exports.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 2,
        "Expected at least 2 export edges for re-exports, got {export_count}"
    );

    // Verify re-export kind is set
    let has_reexport = staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            kind: EdgeKind::Exports { kind, .. },
            ..
        } = op
        {
            *kind == ExportKind::Reexport
        } else {
            false
        }
    });
    assert!(has_reexport, "Expected re-export edge with Reexport kind");
}

#[test]
fn graph_builder_handles_wildcard_re_export() {
    let source = r#"
export * from "./utils";
"#;
    let tree = parse_typescript(source);
    let file = Path::new("test_wildcard_re_export.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 1,
        "Expected at least 1 export edge for wildcard re-export, got {export_count}"
    );
}

#[test]
fn graph_builder_handles_namespace_re_export() {
    use sqry_core::graph::unified::EdgeKind;
    use sqry_core::graph::unified::build::StagingOp;
    use sqry_core::graph::unified::edge::ExportKind;

    let source = r#"
export * as utils from "./utils";
"#;
    let tree = parse_typescript(source);
    let file = Path::new("test_namespace_re_export.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 1,
        "Expected at least 1 export edge for namespace re-export, got {export_count}"
    );

    // Verify namespace export has either Namespace or Reexport kind
    // (namespace re-exports use special AST structure that may vary)
    let has_export = staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            kind: EdgeKind::Exports { kind, .. },
            ..
        } = op
        {
            matches!(kind, ExportKind::Namespace | ExportKind::Reexport)
        } else {
            false
        }
    });
    assert!(has_export, "Expected namespace or reexport edge");
}

#[test]
fn graph_builder_handles_type_exports() {
    let source = r"
interface User {
    name: string;
}

type Config = {
    debug: boolean;
};

export type { User, Config };
";
    let tree = parse_typescript(source);
    let file = Path::new("test_type_exports.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 2,
        "Expected at least 2 export edges for type exports, got {export_count}"
    );
}

#[test]
fn graph_builder_handles_declaration_exports() {
    let source = r"
export function helper() { return 1; }
export class Widget {}
export interface IWidget {}
export type MyType = string;
export enum Status { Active, Inactive }
export const value = 42;
";
    let tree = parse_typescript(source);
    let file = Path::new("test_declaration_exports.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 6,
        "Expected at least 6 export edges for declaration exports, got {export_count}"
    );
}

#[test]
fn graph_builder_handles_mixed_type_value_exports() {
    let source = r"
function getValue() { return 1; }
interface Handler {}

export { type Handler, getValue };
";
    let tree = parse_typescript(source);
    let file = Path::new("test_mixed_exports.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 2,
        "Expected at least 2 export edges for mixed exports, got {export_count}"
    );
}

// ============================================================================
// OOP EDGE TESTS (Wave 1 Team 2)
// ============================================================================

#[test]
fn graph_builder_handles_class_extends() {
    let source = r"
class Animal {
    name: string;
}

class Dog extends Animal {
    bark() { }
}
";
    let tree = parse_typescript(source);
    let file = Path::new("test_class_extends.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let inherits_count = count_inherits_edges(&staging);
    assert!(
        inherits_count >= 1,
        "Expected at least 1 Inherits edge for class extends, got {inherits_count}"
    );
}

#[test]
fn graph_builder_handles_class_implements() {
    let source = r"
interface Runnable {
    run(): void;
}

class Task implements Runnable {
    run() { }
}
";
    let tree = parse_typescript(source);
    let file = Path::new("test_class_implements.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let implements_count = count_implements_edges(&staging);
    assert!(
        implements_count >= 1,
        "Expected at least 1 Implements edge for class implements, got {implements_count}"
    );
}

#[test]
fn graph_builder_handles_class_extends_and_implements() {
    let source = r"
class Base { }
interface IHandler { handle(): void; }
interface IProcessor { process(): void; }

class Worker extends Base implements IHandler, IProcessor {
    handle() { }
    process() { }
}
";
    let tree = parse_typescript(source);
    let file = Path::new("test_class_extends_implements.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let inherits_count = count_inherits_edges(&staging);
    let implements_count = count_implements_edges(&staging);

    assert!(
        inherits_count >= 1,
        "Expected at least 1 Inherits edge, got {inherits_count}"
    );
    assert!(
        implements_count >= 2,
        "Expected at least 2 Implements edges, got {implements_count}"
    );
}

#[test]
fn graph_builder_handles_interface_extends() {
    let source = r"
interface IBase {
    id: string;
}

interface IExtended extends IBase {
    name: string;
}
";
    let tree = parse_typescript(source);
    let file = Path::new("test_interface_extends.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let inherits_count = count_inherits_edges(&staging);
    assert!(
        inherits_count >= 1,
        "Expected at least 1 Inherits edge for interface extends, got {inherits_count}"
    );
}

#[test]
fn graph_builder_handles_interface_multiple_extends() {
    let source = r"
interface IA { a(): void; }
interface IB { b(): void; }
interface IC { c(): void; }

interface ICombined extends IA, IB, IC {
    combined(): void;
}
";
    let tree = parse_typescript(source);
    let file = Path::new("test_interface_multiple_extends.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let inherits_count = count_inherits_edges(&staging);
    assert!(
        inherits_count >= 3,
        "Expected at least 3 Inherits edges for interface multiple extends, got {inherits_count}"
    );
}

#[test]
fn graph_builder_handles_generic_extends() {
    let source = r"
class Container<T> { }

class StringContainer extends Container<string> { }
";
    let tree = parse_typescript(source);
    let file = Path::new("test_generic_extends.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let inherits_count = count_inherits_edges(&staging);
    assert!(
        inherits_count >= 1,
        "Expected at least 1 Inherits edge for generic extends, got {inherits_count}"
    );
}

#[test]
fn graph_builder_handles_generic_implements() {
    let source = r"
interface ICollection<T> {
    add(item: T): void;
}

class StringCollection implements ICollection<string> {
    add(item: string) { }
}
";
    let tree = parse_typescript(source);
    let file = Path::new("test_generic_implements.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let implements_count = count_implements_edges(&staging);
    assert!(
        implements_count >= 1,
        "Expected at least 1 Implements edge for generic implements, got {implements_count}"
    );
}

#[test]
fn graph_builder_combined_exports_and_oop() {
    let source = r"
export interface IBase { }
export class Base implements IBase { }
export class Derived extends Base { }
";
    let tree = parse_typescript(source);
    let file = Path::new("test_combined_exports_oop.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let export_count = count_export_edges(&staging);
    let inherits_count = count_inherits_edges(&staging);
    let implements_count = count_implements_edges(&staging);

    assert!(
        export_count >= 3,
        "Expected at least 3 export edges, got {export_count}"
    );
    assert!(
        inherits_count >= 1,
        "Expected at least 1 Inherits edge, got {inherits_count}"
    );
    assert!(
        implements_count >= 1,
        "Expected at least 1 Implements edge, got {implements_count}"
    );
}

// ============================================================================
// FFI EDGE TESTS (WebAssembly and Native Addons)
// ============================================================================

/// Helper to count `WebAssemblyCall` edges from staging operations
fn count_webassembly_edges(staging: &StagingGraph) -> usize {
    use sqry_core::graph::unified::EdgeKind;
    use sqry_core::graph::unified::build::StagingOp;
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::WebAssemblyCall,
                    ..
                }
            )
        })
        .count()
}

/// Helper to count `FfiCall` edges from staging operations
fn count_ffi_edges(staging: &StagingGraph) -> usize {
    use sqry_core::graph::unified::EdgeKind;
    use sqry_core::graph::unified::build::StagingOp;
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::FfiCall { .. },
                    ..
                }
            )
        })
        .count()
}

#[test]
fn graph_builder_handles_webassembly_instantiate() {
    let source = r"
async function loadWasm(): Promise<WebAssembly.Instance> {
    const response = await fetch('./module.wasm');
    const buffer = await response.arrayBuffer();
    const result = await WebAssembly.instantiate(buffer);
    return result.instance;
}
";
    let tree = parse_typescript(source);
    let file = Path::new("test_wasm_instantiate.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let wasm_count = count_webassembly_edges(&staging);
    assert!(
        wasm_count >= 1,
        "Expected at least 1 WebAssemblyCall edge for WebAssembly.instantiate, got {wasm_count}"
    );
}

#[test]
fn graph_builder_handles_webassembly_instantiate_streaming() {
    let source = r"
async function loadWasmStreaming(): Promise<WebAssembly.Instance> {
    const result = await WebAssembly.instantiateStreaming(fetch('./module.wasm'));
    return result.instance;
}
";
    let tree = parse_typescript(source);
    let file = Path::new("test_wasm_instantiate_streaming.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let wasm_count = count_webassembly_edges(&staging);
    assert!(
        wasm_count >= 1,
        "Expected at least 1 WebAssemblyCall edge for instantiateStreaming, got {wasm_count}"
    );
}

#[test]
fn graph_builder_handles_webassembly_compile() {
    let source = r"
async function compileWasm(buffer: ArrayBuffer): Promise<WebAssembly.Module> {
    return await WebAssembly.compile(buffer);
}
";
    let tree = parse_typescript(source);
    let file = Path::new("test_wasm_compile.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let wasm_count = count_webassembly_edges(&staging);
    assert!(
        wasm_count >= 1,
        "Expected at least 1 WebAssemblyCall edge for WebAssembly.compile, got {wasm_count}"
    );
}

#[test]
fn graph_builder_handles_webassembly_compile_streaming() {
    let source = r"
async function compileWasmStreaming(): Promise<WebAssembly.Module> {
    return await WebAssembly.compileStreaming(fetch('./math.wasm'));
}
";
    let tree = parse_typescript(source);
    let file = Path::new("test_wasm_compile_streaming.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let wasm_count = count_webassembly_edges(&staging);
    assert!(
        wasm_count >= 1,
        "Expected at least 1 WebAssemblyCall edge for compileStreaming, got {wasm_count}"
    );
}

#[test]
fn graph_builder_handles_webassembly_validate() {
    let source = r"
function isValidWasm(buffer: ArrayBuffer): boolean {
    return WebAssembly.validate(buffer);
}
";
    let tree = parse_typescript(source);
    let file = Path::new("test_wasm_validate.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let wasm_count = count_webassembly_edges(&staging);
    assert!(
        wasm_count >= 1,
        "Expected at least 1 WebAssemblyCall edge for WebAssembly.validate, got {wasm_count}"
    );
}

#[test]
fn graph_builder_handles_webassembly_module_constructor() {
    let source = r"
function createModule(buffer: ArrayBuffer): WebAssembly.Module {
    return new WebAssembly.Module(buffer);
}
";
    let tree = parse_typescript(source);
    let file = Path::new("test_wasm_module_constructor.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let wasm_count = count_webassembly_edges(&staging);
    assert!(
        wasm_count >= 1,
        "Expected at least 1 WebAssemblyCall edge for new WebAssembly.Module, got {wasm_count}"
    );
}

#[test]
fn graph_builder_handles_webassembly_instance_constructor() {
    let source = r"
function createInstance(
    module: WebAssembly.Module,
    imports: WebAssembly.Imports
): WebAssembly.Instance {
    return new WebAssembly.Instance(module, imports);
}
";
    let tree = parse_typescript(source);
    let file = Path::new("test_wasm_instance_constructor.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let wasm_count = count_webassembly_edges(&staging);
    assert!(
        wasm_count >= 1,
        "Expected at least 1 WebAssemblyCall edge for new WebAssembly.Instance, got {wasm_count}"
    );
}

#[test]
fn graph_builder_handles_native_addon_require() {
    let source = r"
// Node.js native addon loading
const native = require('./binding.node');
";
    let tree = parse_typescript(source);
    let file = Path::new("test_native_addon_require.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 1,
        "Expected at least 1 FfiCall edge for require .node file, got {ffi_count}"
    );
}

#[test]
fn graph_builder_handles_known_native_package() {
    let source = r"
// Known native addon package
const db = require('better-sqlite3');
";
    let tree = parse_typescript(source);
    let file = Path::new("test_known_native_package.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 1,
        "Expected at least 1 FfiCall edge for known native package, got {ffi_count}"
    );
}

#[test]
fn graph_builder_handles_process_dlopen() {
    let source = r"
// Dynamic loading via process.dlopen
const module: NodeModule = { exports: {} } as any;
process.dlopen(module, './libcrypto.so');
";
    let tree = parse_typescript(source);
    let file = Path::new("test_process_dlopen.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 1,
        "Expected at least 1 FfiCall edge for process.dlopen, got {ffi_count}"
    );
}

#[test]
fn graph_builder_handles_multiple_ffi_edges() {
    let source = r"
async function setupNative(): Promise<void> {
    // Load multiple native resources
    const wasm = await WebAssembly.instantiate(fetch('./compute.wasm'));
    const sqlite = require('better-sqlite3');
    const bcrypt = require('bcrypt');
}
";
    let tree = parse_typescript(source);
    let file = Path::new("test_multiple_ffi.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let wasm_count = count_webassembly_edges(&staging);
    let ffi_count = count_ffi_edges(&staging);

    assert!(
        wasm_count >= 1,
        "Expected at least 1 WebAssemblyCall edge, got {wasm_count}"
    );
    assert!(
        ffi_count >= 2,
        "Expected at least 2 FfiCall edges (sqlite + bcrypt), got {ffi_count}"
    );
}

#[test]
fn graph_builder_skips_regular_require() {
    let source = r"
// Regular module require (not native addon)
const fs = require('fs');
const path = require('path');
const lodash = require('lodash');
";
    let tree = parse_typescript(source);
    let file = Path::new("test_regular_require.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count == 0,
        "Expected 0 FfiCall edges for regular requires, got {ffi_count}"
    );
}

// ========== HTTP Request Edge Detection Tests ==========

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn has_http_request_edge(staging: &StagingGraph, method: HttpMethod, url: Option<&str>) -> bool {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            kind:
                EdgeKind::HttpRequest {
                    method: edge_method,
                    url: edge_url,
                },
            ..
        } = op
        {
            if *edge_method != method {
                continue;
            }

            let url_value = edge_url.and_then(|id| strings.get(&id.index()).cloned());
            if url_value.as_deref() == url {
                return true;
            }
        }
    }
    false
}

#[test]
fn test_fetch_get_request() {
    let source = r#"
async function getData(): Promise<Response> {
    return fetch("/api/data");
}
"#;
    let tree = parse_typescript(source);
    let file = Path::new("test_fetch_get.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert!(
        has_http_request_edge(&staging, HttpMethod::Get, Some("/api/data")),
        "missing HttpRequest edge for GET /api/data"
    );
}

#[test]
fn test_fetch_post_with_options() {
    let source = r#"
async function postData(): Promise<Response> {
    return fetch("/api/users", { method: "POST" });
}
"#;
    let tree = parse_typescript(source);
    let file = Path::new("test_fetch_post.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert!(
        has_http_request_edge(&staging, HttpMethod::Post, Some("/api/users")),
        "missing HttpRequest edge for POST /api/users"
    );
}

#[test]
fn test_axios_get() {
    let source = r#"
async function fetchItems(): Promise<void> {
    const response = await axios.get("/api/items");
}
"#;
    let tree = parse_typescript(source);
    let file = Path::new("test_axios_get.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert!(
        has_http_request_edge(&staging, HttpMethod::Get, Some("/api/items")),
        "missing HttpRequest edge for axios GET /api/items"
    );
}

#[test]
fn test_axios_post() {
    let source = r#"
async function createUser(): Promise<void> {
    await axios.post("/api/users", { name: "John" });
}
"#;
    let tree = parse_typescript(source);
    let file = Path::new("test_axios_post.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert!(
        has_http_request_edge(&staging, HttpMethod::Post, Some("/api/users")),
        "missing HttpRequest edge for axios POST /api/users"
    );
}

#[test]
fn test_axios_config_object() {
    let source = r#"
async function callApi(): Promise<void> {
    await axios({ method: "put", url: "/api/resource" });
}
"#;
    let tree = parse_typescript(source);
    let file = Path::new("test_axios_config.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert!(
        has_http_request_edge(&staging, HttpMethod::Put, Some("/api/resource")),
        "missing HttpRequest edge for axios config PUT /api/resource"
    );
}

#[test]
fn test_fetch_with_type_annotation() {
    let source = r#"
interface ApiResponse {
    data: string[];
}

async function fetchData(): Promise<ApiResponse> {
    const response: Response = await fetch("/api/typed");
    return response.json();
}
"#;
    let tree = parse_typescript(source);
    let file = Path::new("test_fetch_typed.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert!(
        has_http_request_edge(&staging, HttpMethod::Get, Some("/api/typed")),
        "missing HttpRequest edge for GET /api/typed with type annotations"
    );
}

#[test]
fn test_http_alongside_regular_call() {
    let source = r#"
function helper(): string { return "ok"; }

async function doWork(): Promise<void> {
    const result = helper();
    await fetch("/api/endpoint");
}
"#;
    let tree = parse_typescript(source);
    let file = Path::new("test_http_and_call.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert!(
        has_http_request_edge(&staging, HttpMethod::Get, Some("/api/endpoint")),
        "missing HttpRequest edge alongside regular call"
    );
    assert_has_call_edge(&staging, "doWork", "helper");
}

#[test]
fn test_no_http_for_custom_fetch() {
    let source = r#"
function myFetch(url: string): void {}
function doStuff(): void {
    myFetch("/not-http");
}
"#;
    let tree = parse_typescript(source);
    let file = Path::new("test_no_http_custom.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert!(
        !has_http_request_edge(&staging, HttpMethod::Get, Some("/not-http")),
        "should NOT create HttpRequest edge for custom myFetch"
    );
}

#[test]
fn test_http_in_async_function() {
    let source = r#"
async function loadData(): Promise<string> {
    const res = await fetch("/api/async-data", { method: "DELETE" });
    return res.text();
}
"#;
    let tree = parse_typescript(source);
    let file = Path::new("test_http_async.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert!(
        has_http_request_edge(&staging, HttpMethod::Delete, Some("/api/async-data")),
        "missing HttpRequest edge for async DELETE /api/async-data"
    );
}

#[test]
fn test_http_in_class_method() {
    let source = r#"
class ApiClient {
    async getUsers(): Promise<void> {
        await fetch("/api/users");
    }
    async updateUser(): Promise<void> {
        await axios.patch("/api/users/1", { name: "Jane" });
    }
}
"#;
    let tree = parse_typescript(source);
    let file = Path::new("test_http_class.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert!(
        has_http_request_edge(&staging, HttpMethod::Get, Some("/api/users")),
        "missing HttpRequest edge for class method GET /api/users"
    );
    assert!(
        has_http_request_edge(&staging, HttpMethod::Patch, Some("/api/users/1")),
        "missing HttpRequest edge for class method PATCH /api/users/1"
    );
}

// ========== Express .all() Endpoint Tests ==========

use sqry_core::graph::unified::node::NodeKind;

/// Helper: check if staging has an Endpoint node whose resolved name contains the given substring.
fn has_endpoint_with_name(staging: &StagingGraph, name_substring: &str) -> bool {
    staging.operations().iter().any(|op| {
        if let StagingOp::AddNode { entry, .. } = op
            && matches!(entry.kind, NodeKind::Endpoint)
            && let Some(resolved) = staging.resolve_local_string(entry.name)
        {
            return resolved.contains(name_substring);
        }
        false
    })
}

/// Helper: collect all Endpoint node names from staging.
fn collect_endpoint_names(staging: &StagingGraph) -> Vec<String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, .. } = op
                && matches!(entry.kind, NodeKind::Endpoint)
            {
                return staging.resolve_local_string(entry.name).map(String::from);
            }
            None
        })
        .collect()
}

#[test]
fn test_ts_express_all_creates_endpoint() {
    let source = r#"
import express from "express";
const app = express();

app.all("/health", (req, res) => {
    res.json({ status: "ok" });
});
"#;

    let tree = parse_typescript(source);
    let file = Path::new("test_ts_express_all.ts");
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert!(
        has_endpoint_with_name(&staging, "route::ALL::/health"),
        "Expected Endpoint 'route::ALL::/health', found: {:?}",
        collect_endpoint_names(&staging)
    );
    // Verify it's NOT route::GET (that was the old bug — TS was mapping .all() to GET)
    assert!(
        !has_endpoint_with_name(&staging, "route::GET::/health"),
        "app.all() should NOT create route::GET, found: {:?}",
        collect_endpoint_names(&staging)
    );
}
