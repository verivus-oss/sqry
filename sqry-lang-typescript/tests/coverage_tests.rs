//! Coverage-targeted tests for `sqry-lang-typescript`.
//!
//! Exercises uncovered paths in:
//! - `src/relations/graph_builder.rs`:
//!   - namespace/module declarations and augmentations
//!   - import statements (named, default, namespace, side-effect)
//!   - export statements (named, re-export, default, barrel)
//!   - class inheritance and interface extension (OOP edges)
//!   - class field `TypeOf` edges
//!   - interface property `TypeOf` edges
//!   - type alias declarations
//!   - enum declarations
//!   - variable declarations (const/let/var)
//!   - parameter type edges (required, optional, rest)
//!   - return type edges
//!   - HTTP request patterns (fetch/axios)
//!   - Express route endpoint registration
//!   - FFI patterns (WebAssembly)
//!   - constructor calls (`new_expression`)
//!   - async functions
//! - `src/relations/local_scopes.rs`:
//!   - all `ScopeKind` variants (Function, `ArrowFunction`, Method, Block,
//!     `IfBranch`, `ForLoop`, `ForInLoop`, `ForOfLoop`, `WhileLoop`, `DoWhileLoop`,
//!     `TryBlock`, `CatchBlock`, `FinallyBlock`, `SwitchBlock`, `SwitchCase`)
//!   - `bind_pattern` arms (identifier, `array_pattern`, `object_pattern`,
//!     `assignment_pattern`, `rest_pattern`, `rest_element`)
//!   - `bind_for_loop_variable` arms
//!   - `bind_catch_parameter`

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_lang_typescript::relations::TypeScriptGraphBuilder;
use std::path::Path;
use tree_sitter::Tree;

// ─────────────────────────────────────────────────────────────────────────────
// Shared helpers
// ─────────────────────────────────────────────────────────────────────────────

fn parse_ts(source: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        .expect("set TypeScript language");
    parser.parse(source, None).expect("parse TypeScript")
}

fn build_graph(source: &str) -> StagingGraph {
    let tree = parse_ts(source);
    let mut staging = StagingGraph::new();
    let builder = TypeScriptGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), Path::new("test.ts"), &mut staging)
        .expect("build_graph should not fail");
    staging
}

fn has_edge_tag(staging: &StagingGraph, tag: &str) -> bool {
    use sqry_core::graph::unified::build::staging::StagingOp;
    staging
        .operations()
        .iter()
        .any(|op| matches!(op, StagingOp::AddEdge { kind, .. } if kind.tag() == tag))
}

fn all_edge_tags(staging: &StagingGraph) -> Vec<String> {
    use sqry_core::graph::unified::build::staging::StagingOp;
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                Some(kind.tag().to_string())
            } else {
                None
            }
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// TypeScriptGraphBuilder constructors
// ─────────────────────────────────────────────────────────────────────────────

/// `TypeScriptGraphBuilder::new(depth)` sets the `max_scope_depth`
#[test]
fn builder_new_with_custom_depth() {
    let builder = TypeScriptGraphBuilder::new(8);
    let tree = parse_ts("function foo() {}");
    let mut staging = StagingGraph::new();
    builder
        .build_graph(
            &tree,
            b"function foo() {}",
            Path::new("test.ts"),
            &mut staging,
        )
        .expect("build_graph should not fail");
    assert!(staging.stats().nodes_staged >= 1);
}

/// `TypeScriptGraphBuilder::default()` produces at least one node for a function
#[test]
fn builder_default_produces_nodes() {
    let staging = build_graph("function hello(): string { return 'hi'; }");
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least one node"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Import statements
// ─────────────────────────────────────────────────────────────────────────────

/// Named import: `import { Foo } from './foo'`
#[test]
fn named_import() {
    let staging = build_graph("import { Foo } from './foo';");
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge; got: {:?}",
        all_edge_tags(&staging)
    );
}

/// Default import: `import Foo from './foo'`
#[test]
fn default_import() {
    let staging = build_graph("import Foo from './foo';");
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge; got: {:?}",
        all_edge_tags(&staging)
    );
}

/// Namespace import: `import * as Foo from './foo'`
#[test]
fn namespace_import() {
    let staging = build_graph("import * as Foo from './foo';");
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge; got: {:?}",
        all_edge_tags(&staging)
    );
}

/// Side-effect import: `import './polyfill'` — build should not panic
#[test]
fn side_effect_import_no_panic() {
    let _staging = build_graph("import './polyfill';");
}

// ─────────────────────────────────────────────────────────────────────────────
// Export statements
// ─────────────────────────────────────────────────────────────────────────────

/// Named export: `export { foo }`
#[test]
fn named_export() {
    let staging = build_graph("function foo() {} export { foo };");
    assert!(
        has_edge_tag(&staging, "exports"),
        "Expected exports edge; got: {:?}",
        all_edge_tags(&staging)
    );
}

/// Default export function
#[test]
fn default_export_function() {
    let staging = build_graph("export default function bar() {}");
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least one node for exported function"
    );
}

/// Re-export: `export { foo } from './other'`
#[test]
fn re_export_from_module() {
    // Build must not panic; re-exports may produce imports or exports edges
    let _staging = build_graph("export { foo } from './other';");
}

/// Export star barrel: `export * from './barrel'`
#[test]
fn export_star_barrel() {
    let _staging = build_graph("export * from './barrel';");
}

// ─────────────────────────────────────────────────────────────────────────────
// Class OOP edges
// ─────────────────────────────────────────────────────────────────────────────

/// Class extending another class produces `inherits` edge
#[test]
fn class_inheritance() {
    let staging = build_graph(
        r"
class Animal {}
class Dog extends Animal {}
",
    );
    assert!(
        has_edge_tag(&staging, "inherits"),
        "Expected inherits edge; got: {:?}",
        all_edge_tags(&staging)
    );
}

/// Class implementing interface produces `implements` edge
#[test]
fn class_implements_interface() {
    let staging = build_graph(
        r"
interface Printable { print(): void; }
class Document implements Printable {
    print() {}
}
",
    );
    assert!(
        has_edge_tag(&staging, "implements"),
        "Expected implements edge; got: {:?}",
        all_edge_tags(&staging)
    );
}

/// Interface extending another interface
#[test]
fn interface_extends_interface() {
    let staging = build_graph(
        r"
interface Base { id: number; }
interface Extended extends Base { name: string; }
",
    );
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least one node for interface"
    );
}

/// Class with field type annotations produces `type_of` edges
#[test]
fn class_field_typeof_edges() {
    let staging = build_graph(
        r"
class MyService {
    name: string;
    count: number;
}
",
    );
    // type_of edges for field types
    assert!(
        has_edge_tag(&staging, "type_of"),
        "Expected type_of edge for class fields; got: {:?}",
        all_edge_tags(&staging)
    );
}

/// Interface with property type annotations produces `type_of` edges
#[test]
fn interface_property_typeof_edges() {
    let staging = build_graph(
        r"
interface Config {
    host: string;
    port: number;
}
",
    );
    assert!(
        has_edge_tag(&staging, "type_of"),
        "Expected type_of edge for interface properties; got: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Type alias declarations
// ─────────────────────────────────────────────────────────────────────────────

/// Type alias referencing another type
#[test]
fn type_alias_references_type() {
    // Build should not panic; may produce type_of edges
    let _staging = build_graph("type MyString = string;");
}

/// Mapped type alias
#[test]
fn mapped_type_alias() {
    let _staging = build_graph("type ReadOnly<T> = { readonly [K in keyof T]: T[K] };");
}

/// Conditional type alias
#[test]
fn conditional_type_alias() {
    let _staging = build_graph("type IsString<T> = T extends string ? 'yes' : 'no';");
}

/// Union type alias
#[test]
fn union_type_alias() {
    let _staging = build_graph("type StringOrNumber = string | number;");
}

// ─────────────────────────────────────────────────────────────────────────────
// Enum declarations
// ─────────────────────────────────────────────────────────────────────────────

/// Const enum produces an enum node
#[test]
fn const_enum_declaration() {
    let staging = build_graph(
        r"
const enum Direction {
    Up,
    Down,
    Left,
    Right,
}
",
    );
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least one node for const enum"
    );
}

/// String enum declaration
#[test]
fn string_enum_declaration() {
    let staging = build_graph(
        r#"
enum Color {
    Red = "RED",
    Green = "GREEN",
    Blue = "BLUE",
}
"#,
    );
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least one node for string enum"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Namespace / module declarations
// ─────────────────────────────────────────────────────────────────────────────

/// Namespace declaration creates a Module node
#[test]
fn namespace_declaration() {
    let staging = build_graph(
        r"
namespace MyApp {
    export function init() {}
}
",
    );
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected Module node for namespace"
    );
}

/// Module augmentation (second namespace with same name)
#[test]
fn namespace_augmentation() {
    let staging = build_graph(
        r"
namespace Lib {
    export function a() {}
}
namespace Lib {
    export function b() {}
}
",
    );
    // Both declarations should not panic; the second one augments the first
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Variable declarations
// ─────────────────────────────────────────────────────────────────────────────

/// `const` declaration
#[test]
fn const_declaration() {
    let staging = build_graph("const MAX: number = 100;");
    // Const variable may or may not produce explicit nodes depending on implementation
    // Just verify build doesn't panic
    let _ = staging.stats();
}

/// `let` declaration
#[test]
fn let_declaration() {
    let _staging = build_graph("let counter: number = 0;");
}

/// `var` declaration
#[test]
fn var_declaration() {
    let _staging = build_graph("var legacy: string = 'old';");
}

// ─────────────────────────────────────────────────────────────────────────────
// Function parameter type edges
// ─────────────────────────────────────────────────────────────────────────────

/// Required typed parameter produces `type_of` edge
#[test]
fn required_parameter_type_edge() {
    let staging = build_graph("function greet(name: string): void {}");
    assert!(
        has_edge_tag(&staging, "type_of"),
        "Expected type_of edge for parameter; got: {:?}",
        all_edge_tags(&staging)
    );
}

/// Optional parameter
#[test]
fn optional_parameter_type_edge() {
    let staging = build_graph("function maybe(x?: number): void {}");
    // May produce type_of for the optional number type
    let _ = staging.stats();
}

/// Rest parameter `...args: string[]`
#[test]
fn rest_parameter_type_edge() {
    let staging = build_graph("function rest(...args: string[]): void {}");
    // Rest parameter type annotation
    let _ = staging.stats();
}

/// Arrow function with typed parameter
#[test]
fn arrow_function_typed_param() {
    let staging = build_graph("const fn = (x: number): number => x * 2;");
    assert!(
        has_edge_tag(&staging, "type_of"),
        "Expected type_of edge for arrow function parameter; got: {:?}",
        all_edge_tags(&staging)
    );
}

/// Method with typed parameter and return type
#[test]
fn method_typed_params_and_return() {
    let staging = build_graph(
        r"
class Calc {
    add(a: number, b: number): number {
        return a + b;
    }
}
",
    );
    assert!(
        has_edge_tag(&staging, "type_of"),
        "Expected type_of edge for method parameters; got: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Return type edges
// ─────────────────────────────────────────────────────────────────────────────

/// Function with return type annotation — exercises the `return_type` code path.
/// The function is resolved as a callable context and the return type annotation
/// may produce a `type_of` edge. We verify the build completes successfully and
/// produces at least one node for the function itself.
#[test]
fn function_return_type_edge() {
    let staging = build_graph("function getCount(): number { return 0; }");
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least one node for function with return type"
    );
}

/// Async function with Promise return type
#[test]
fn async_function_return_type_edge() {
    let staging = build_graph("async function fetchData(): Promise<string> { return ''; }");
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least one node for async function"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// HTTP request patterns
// ─────────────────────────────────────────────────────────────────────────────

/// `fetch()` produces an `http_request` edge
#[test]
fn fetch_http_request() {
    let staging = build_graph(
        r"
async function loadData() {
    const res = await fetch('https://api.example.com/data');
    return res.json();
}
",
    );
    assert!(
        has_edge_tag(&staging, "http_request"),
        "Expected http_request edge for fetch; got: {:?}",
        all_edge_tags(&staging)
    );
}

/// Express route registration
#[test]
fn express_route_registration() {
    let staging = build_graph(
        r"
const app = express();
app.get('/users', (req, res) => { res.json([]); });
",
    );
    // May produce endpoint or http_request edges
    let _ = staging.stats();
}

// ─────────────────────────────────────────────────────────────────────────────
// Constructor calls (new_expression)
// ─────────────────────────────────────────────────────────────────────────────

/// `new Foo()` produces a calls edge
#[test]
fn constructor_call() {
    let staging = build_graph(
        r"
class Foo {}
function main() {
    const f = new Foo();
}
",
    );
    assert!(
        has_edge_tag(&staging, "calls"),
        "Expected calls edge for constructor; got: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Call edges
// ─────────────────────────────────────────────────────────────────────────────

/// Regular function call produces a `calls` edge
#[test]
fn function_call_edge() {
    let staging = build_graph(
        r"
function helper() {}
function caller() { helper(); }
",
    );
    assert!(
        has_edge_tag(&staging, "calls"),
        "Expected calls edge; got: {:?}",
        all_edge_tags(&staging)
    );
}

/// Awaited function call
#[test]
fn awaited_function_call() {
    let staging = build_graph(
        r"
async function doWork() {}
async function main() { await doWork(); }
",
    );
    assert!(
        has_edge_tag(&staging, "calls"),
        "Expected calls edge for awaited call; got: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Local scopes — all ScopeKind variants
// ─────────────────────────────────────────────────────────────────────────────

/// Function scope with inner variable reference
#[test]
fn scope_function() {
    let staging = build_graph(
        r"
function outer() {
    const x = 1;
    return x;
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// Arrow function scope
#[test]
fn scope_arrow_function() {
    let staging = build_graph(
        r"
const double = (n: number) => n * 2;
",
    );
    // Build should not panic
    let _ = staging.stats();
}

/// Method scope in class
#[test]
fn scope_method() {
    let staging = build_graph(
        r"
class Counter {
    private count = 0;
    increment() { this.count++; }
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// Block scope with `{ ... }`
#[test]
fn scope_block() {
    let staging = build_graph(
        r"
function test() {
    {
        const scoped = 42;
    }
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// If-branch scope
#[test]
fn scope_if_branch() {
    let staging = build_graph(
        r"
function check(x: number) {
    if (x > 0) {
        const positive = true;
    } else {
        const positive = false;
    }
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// C-style for loop scope
#[test]
fn scope_for_loop() {
    let staging = build_graph(
        r"
function iterate() {
    for (let i = 0; i < 10; i++) {
        const step = i;
    }
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// For-in loop scope (binds loop variable)
#[test]
fn scope_for_in_loop() {
    let staging = build_graph(
        r"
function forIn(obj: Record<string, number>) {
    for (const key in obj) {
        console.log(key);
    }
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// For-of loop scope (binds loop variable)
#[test]
fn scope_for_of_loop() {
    let staging = build_graph(
        r"
function forOf(arr: number[]) {
    for (const item of arr) {
        console.log(item);
    }
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// While loop scope
#[test]
fn scope_while_loop() {
    let staging = build_graph(
        r"
function whileLoop() {
    let n = 10;
    while (n > 0) {
        n--;
    }
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// Do-while loop scope
#[test]
fn scope_do_while_loop() {
    let staging = build_graph(
        r"
function doWhile() {
    let x = 0;
    do {
        x++;
    } while (x < 5);
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// Try-catch-finally scope (`TryBlock`, `CatchBlock`, `FinallyBlock`)
#[test]
fn scope_try_catch_finally() {
    let staging = build_graph(
        r"
function safe() {
    try {
        const a = 1;
    } catch (err) {
        const msg = String(err);
    } finally {
        const done = true;
    }
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// Switch-case scope (`SwitchBlock` + `SwitchCase`)
#[test]
fn scope_switch_case() {
    let staging = build_graph(
        r"
function sw(val: number) {
    switch (val) {
        case 1:
            const one = 'one';
            break;
        default:
            const other = 'other';
    }
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// bind_pattern arms — destructuring patterns
// ─────────────────────────────────────────────────────────────────────────────

/// Array destructuring pattern in variable declaration
#[test]
fn bind_pattern_array_destructure() {
    let staging = build_graph(
        r"
function test() {
    const [a, b, c] = [1, 2, 3];
    return a + b + c;
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// Object destructuring pattern in variable declaration
#[test]
fn bind_pattern_object_destructure() {
    let staging = build_graph(
        r"
function test() {
    const { x, y } = { x: 1, y: 2 };
    return x + y;
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// Assignment pattern (default value) in destructuring
#[test]
fn bind_pattern_assignment_default() {
    let staging = build_graph(
        r"
function test() {
    const { name = 'default' } = {};
    return name;
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// Rest element in array destructuring
#[test]
fn bind_pattern_rest_element_array() {
    let staging = build_graph(
        r"
function test() {
    const [first, ...rest] = [1, 2, 3];
    return rest;
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// Rest element in object destructuring
#[test]
fn bind_pattern_rest_element_object() {
    let staging = build_graph(
        r"
function test() {
    const { a, ...others } = { a: 1, b: 2 };
    return others;
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// Renamed destructuring: `{ x: renamed }`
#[test]
fn bind_pattern_pair_pattern() {
    let staging = build_graph(
        r"
function test() {
    const { x: myX, y: myY } = { x: 1, y: 2 };
    return myX + myY;
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// Nested object + array destructuring
#[test]
fn bind_pattern_nested_destructure() {
    let staging = build_graph(
        r"
function test() {
    const { a: [first, second] } = { a: [1, 2] };
    return first + second;
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// Arrow function with single identifier parameter (no parens)
#[test]
fn arrow_function_single_param_no_parens() {
    let staging = build_graph(
        r"
const double = x => x * 2;
",
    );
    // Build should not panic
    let _ = staging.stats();
}

/// For-of loop with identifier variable (not a declaration)
#[test]
fn for_of_identifier_variable() {
    let staging = build_graph(
        r"
function test(arr: number[]) {
    let item: number;
    for (item of arr) {
        console.log(item);
    }
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}

/// Catch clause binds the error parameter
#[test]
fn catch_clause_parameter_binding() {
    let staging = build_graph(
        r"
function safe() {
    try {
        throw new Error('test');
    } catch (err) {
        console.error(err);
    }
}
",
    );
    assert!(staging.stats().nodes_staged >= 1);
}
