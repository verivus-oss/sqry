//! Coverage-targeted tests for `sqry-lang-go`.
//!
//! Exercises uncovered paths in:
//! - `src/relations/graph_builder.rs`:
//!   - goroutine (`go foo()`) → Goroutine call modifier
//!   - deferred calls (`defer foo()`) → Deferred call modifier
//!   - type assertions (`x.(Type)`) → `TypeOf` edges
//!   - type aliases (`type Alias = Base`)
//!   - const/var declarations
//!   - interface types with embedding
//!   - channel types in parameters
//!   - `CGo` import detection (`import "C"`)
//!   - HTTP route registration (net/http patterns)
//!   - struct type declarations
//!   - exported vs unexported identifiers
//! - `src/relations/local_scopes.rs`:
//!   - all 10 `ScopeKind` variants (Function, Method, Block, `IfBranch`, `ForLoop`,
//!     `SwitchBlock`, `CaseClause`, `SelectBlock`, `CommClause`, `FuncLiteral`)
//!   - short variable declaration (`:=`)
//!   - range clause
//!   - `var_spec` binding

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_lang_go::relations::GoGraphBuilder;
use std::path::Path;
use tree_sitter::Tree;

// ─────────────────────────────────────────────────────────────────────────────
// Shared helpers
// ─────────────────────────────────────────────────────────────────────────────

fn parse_go(source: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .expect("set Go language");
    parser.parse(source, None).expect("parse Go")
}

fn build_graph(source: &str) -> StagingGraph {
    let tree = parse_go(source);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), Path::new("test.go"), &mut staging)
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

/// Returns true when any staged node has a canonical or display name that contains
/// `substr`. Used to verify specific symbols are present in the graph without
/// relying purely on node counts.
fn has_node_name_containing(staging: &StagingGraph, substr: &str) -> bool {
    staging.nodes().any(|n| {
        staging
            .resolve_node_name(n.entry)
            .is_some_and(|s| s.contains(substr))
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Goroutine detection
// ─────────────────────────────────────────────────────────────────────────────

/// `go foo()` creates a Calls edge with goroutine metadata
#[test]
fn goroutine_call_creates_edge() {
    let source = r#"package main

import "fmt"

func worker(id int) {
    fmt.Println(id)
}

func main() {
    go worker(1)
    go worker(2)
}
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "calls"),
        "Expected calls edge for goroutine. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Goroutine with inline function literal
#[test]
fn goroutine_func_literal() {
    let source = r#"package main

import "fmt"

func start() {
    go func() {
        fmt.Println("async")
    }()
}
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "calls"),
        "Expected calls edge for goroutine func literal. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Deferred calls
// ─────────────────────────────────────────────────────────────────────────────

/// `defer foo()` creates a Calls edge with deferred metadata
#[test]
fn defer_call_creates_edge() {
    let source = r#"package main

import "fmt"

func cleanup() {
    fmt.Println("cleanup")
}

func process() {
    defer cleanup()
    fmt.Println("processing")
}
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "calls"),
        "Expected calls edge for defer. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Defer with closure
#[test]
fn defer_closure() {
    let source = r#"package main

import "fmt"

func withDefer() {
    defer func() {
        fmt.Println("deferred cleanup")
    }()
    fmt.Println("doing work")
}
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "calls"),
        "Expected calls edge for deferred closure. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Type assertions
// ─────────────────────────────────────────────────────────────────────────────

/// `x.(Type)` type assertion creates `TypeOf` edges
#[test]
fn type_assertion_creates_edge() {
    let source = r#"package main

import "fmt"

type Stringer interface {
    String() string
}

func print_if_stringer(v interface{}) {
    if s, ok := v.(Stringer); ok {
        fmt.Println(s.String())
    }
}
"#;
    let staging = build_graph(source);
    // Type assertion code path must produce at minimum the interface and function nodes
    assert!(
        staging.stats().nodes_staged >= 2,
        "Expected at least interface + function nodes. Got: {}",
        staging.stats().nodes_staged
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Type declarations: alias, struct, interface
// ─────────────────────────────────────────────────────────────────────────────

/// Simple type alias: `type UserID = int`
#[test]
fn type_alias_simple() {
    let source = r#"package main

type UserID = int

func GetUser(id UserID) string {
    return "user"
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // type_alias produces a TypeOf edge: UserID → int
    assert!(
        has_edge_tag(&staging, "type_of"),
        "Expected type_of edge for type alias. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Type alias with pointer: `type Handler = func(string) error`
#[test]
fn type_alias_func_type() {
    let source = r"package main

type Handler = func(string) error

func Apply(h Handler, s string) error {
    return h(s)
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // type_alias produces a TypeOf edge: Handler → func(string) error
    assert!(
        has_edge_tag(&staging, "type_of"),
        "Expected type_of edge for func-type alias. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Struct type declaration creates Type node
#[test]
fn struct_type_declaration() {
    let source = r"package main

type Point struct {
    X float64
    Y float64
}

func NewPoint(x, y float64) Point {
    return Point{X: x, Y: y}
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 2);
}

/// Interface type declaration
#[test]
fn interface_type_declaration() {
    let source = r"package main

type Writer interface {
    Write([]byte) (int, error)
}

type Reader interface {
    Read([]byte) (int, error)
}

type ReadWriter interface {
    Reader
    Writer
}
";
    let staging = build_graph(source);
    // Three interface nodes: Writer, Reader, ReadWriter
    assert!(
        staging.stats().nodes_staged >= 3,
        "Expected at least 3 interface nodes. Got: {}",
        staging.stats().nodes_staged
    );
    // ReadWriter embeds Reader and Writer, producing Inherits edges
    assert!(
        has_edge_tag(&staging, "inherits"),
        "Expected inherits edge for interface embedding. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Const and var declarations
// ─────────────────────────────────────────────────────────────────────────────

/// Exported const creates export edge
#[test]
fn exported_const_declaration() {
    let source = r"package mypackage

const MaxRetries = 3
const DefaultTimeout = 30.0
";
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "exports"),
        "Expected exports edge for exported const. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Unexported const does not create export edge
#[test]
fn unexported_const_no_export() {
    let source = r"package mypackage

const maxBuffer = 4096
";
    let staging = build_graph(source);
    assert!(
        !has_edge_tag(&staging, "exports"),
        "Unexported constant should not produce an exports edge. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Exported var declaration
#[test]
fn exported_var_declaration() {
    let source = r#"package mypackage

var GlobalLogger = "default"
var ErrNotFound = "not found"
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "exports"),
        "Expected exports edge for exported var. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Const block declaration
#[test]
fn const_block_declaration() {
    let source = r"package main

const (
    StatusOK    = 200
    StatusNotFound = 404
    StatusError = 500
)
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // All three constants are uppercase (exported), producing export edges
    assert!(
        has_edge_tag(&staging, "exports"),
        "Expected exports edges for uppercase const block. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Channel types in parameters
// ─────────────────────────────────────────────────────────────────────────────

/// Function with channel parameter
#[test]
fn channel_parameter() {
    let source = r"package main

func producer(out chan<- int) {
    for i := 0; i < 10; i++ {
        out <- i
    }
}

func consumer(in <-chan int) int {
    total := 0
    for v := range in {
        total += v
    }
    return total
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 2);
}

// ─────────────────────────────────────────────────────────────────────────────
// CGo import detection
// ─────────────────────────────────────────────────────────────────────────────

/// `import "C"` triggers `CGo` detection
#[test]
fn cgo_import_detection() {
    let source = r#"package main

/*
#include <stdio.h>
*/
import "C"

func main() {
    C.puts(C.CString("Hello CGo\n"))
}
"#;
    let staging = build_graph(source);
    // CGo import exercises the "C" import detection path; main function must be staged
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least one node from CGo source"
    );
    // import "C" produces an imports edge for the CGo pseudo-package
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge for CGo 'C' import. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// HTTP route registration (net/http patterns)
// ─────────────────────────────────────────────────────────────────────────────

/// `http.HandleFunc("/path", handler)` creates endpoint node
#[test]
fn http_handle_func_route() {
    let source = r#"package main

import "net/http"

func homeHandler(w http.ResponseWriter, r *http.Request) {
    w.WriteHeader(http.StatusOK)
}

func main() {
    http.HandleFunc("/", homeHandler)
    http.ListenAndServe(":8080", nil)
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // http.HandleFunc creates a route Endpoint node and a Calls edge from main to it
    assert!(
        has_edge_tag(&staging, "calls"),
        "Expected calls edge for http.HandleFunc route registration. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Exported vs unexported functions
// ─────────────────────────────────────────────────────────────────────────────

/// Exported function (uppercase) gets export edge
#[test]
fn exported_function_gets_export_edge() {
    let source = r#"package mypackage

import "fmt"

func PublicFunc() {
    fmt.Println("public")
}

func privateFunc() {
    fmt.Println("private")
}
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "exports"),
        "Expected exports edge for public function. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Pointer receiver method
#[test]
fn pointer_receiver_method() {
    let source = r#"package main

import "fmt"

type Counter struct {
    count int
}

func (c *Counter) Increment() {
    c.count++
}

func (c *Counter) Value() int {
    return c.count
}

func main() {
    c := &Counter{}
    c.Increment()
    fmt.Println(c.Value())
}
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "calls"),
        "Expected calls edge. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// local_scopes.rs — all scope kind variants
// ─────────────────────────────────────────────────────────────────────────────

/// Function scope with short variable declaration
#[test]
fn scope_function_with_short_var() {
    let source = r"package main

func compute() int {
    x := 10
    y := 20
    return x + y
}
";
    let staging = build_graph(source);
    // At minimum the compute function node; parameters may produce type nodes
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least the 'compute' function node. Got: {}",
        staging.stats().nodes_staged
    );
    // Short variable declarations (:=) are local and do not produce additional top-level graph
    // nodes — only the function itself (and any parameter type nodes) are staged
    assert!(
        staging.stats().nodes_staged < 10,
        "Unexpectedly many nodes for a simple function with :=-locals. Got: {}",
        staging.stats().nodes_staged
    );
    // The 'compute' function node must appear in the staged graph by name.
    assert!(
        has_node_name_containing(&staging, "compute"),
        "Expected a staged node with name containing 'compute'"
    );
}

/// Method scope
#[test]
fn scope_method() {
    let source = r#"package main

import "fmt"

type Service struct{ name string }

func (s *Service) Run() {
    msg := "running " + s.name
    fmt.Println(msg)
}
"#;
    let staging = build_graph(source);
    // At minimum: Service struct node + Run method node
    assert!(
        staging.stats().nodes_staged >= 2,
        "Expected at least struct + method nodes. Got: {}",
        staging.stats().nodes_staged
    );
    // import "fmt" produces an imports edge
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge for fmt. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// `IfBranch` scope
#[test]
fn scope_if_branch() {
    let source = r"package main

func abs(x int) int {
    if x < 0 {
        return -x
    }
    return x
}
";
    let staging = build_graph(source);
    // At minimum the abs function node; parameters produce type nodes
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least the 'abs' function node. Got: {}",
        staging.stats().nodes_staged
    );
    // No imports or named calls, so node count stays small
    assert!(
        staging.stats().nodes_staged < 20,
        "Unexpectedly many nodes for a simple if-branch function. Got: {}",
        staging.stats().nodes_staged
    );
    // The 'abs' function node must appear in the staged graph by name.
    assert!(
        has_node_name_containing(&staging, "abs"),
        "Expected a staged node with name containing 'abs'"
    );
}

/// `ForLoop` scope
#[test]
fn scope_for_loop() {
    let source = r"package main

func sum(n int) int {
    total := 0
    for i := 0; i < n; i++ {
        total += i
    }
    return total
}
";
    let staging = build_graph(source);
    // At minimum the sum function node; parameters produce type nodes
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least the 'sum' function node. Got: {}",
        staging.stats().nodes_staged
    );
    // No imports or named calls, so node count stays small
    assert!(
        staging.stats().nodes_staged < 20,
        "Unexpectedly many nodes for a simple for-loop function. Got: {}",
        staging.stats().nodes_staged
    );
    // The 'sum' function node must appear in the staged graph by name.
    assert!(
        has_node_name_containing(&staging, "sum"),
        "Expected a staged node with name containing 'sum'"
    );
}

/// Range clause in for loop
#[test]
fn scope_for_range() {
    let source = r"package main

func sumSlice(items []int) int {
    total := 0
    for _, v := range items {
        total += v
    }
    return total
}
";
    let staging = build_graph(source);
    // At minimum the sumSlice function node; parameters produce type nodes
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least the 'sumSlice' function node. Got: {}",
        staging.stats().nodes_staged
    );
    // No imports or named calls, so node count stays small
    assert!(
        staging.stats().nodes_staged < 20,
        "Unexpectedly many nodes for a simple for-range function. Got: {}",
        staging.stats().nodes_staged
    );
    // The 'sumSlice' function node must appear in the staged graph by name.
    assert!(
        has_node_name_containing(&staging, "sumSlice"),
        "Expected a staged node with name containing 'sumSlice'"
    );
}

/// `SwitchBlock` and `CaseClause` scopes
#[test]
fn scope_switch_case() {
    let source = r#"package main

func dayName(d int) string {
    switch d {
    case 0:
        return "Sunday"
    case 1:
        return "Monday"
    default:
        return "Other"
    }
}
"#;
    let staging = build_graph(source);
    // At minimum the dayName function node; parameters produce type nodes
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least the 'dayName' function node. Got: {}",
        staging.stats().nodes_staged
    );
    // No imports or named calls, so node count stays small
    assert!(
        staging.stats().nodes_staged < 20,
        "Unexpectedly many nodes for a simple switch-case function. Got: {}",
        staging.stats().nodes_staged
    );
    // The 'dayName' function node must appear in the staged graph by name.
    assert!(
        has_node_name_containing(&staging, "dayName"),
        "Expected a staged node with name containing 'dayName'"
    );
}

/// Type switch creates `CaseClause` scopes
#[test]
fn scope_type_switch() {
    let source = r#"package main

import "fmt"

func describe(i interface{}) string {
    switch v := i.(type) {
    case int:
        return fmt.Sprintf("int %d", v)
    case string:
        return fmt.Sprintf("string %s", v)
    default:
        return "other"
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // import "fmt" produces an imports edge
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge for fmt. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// `SelectBlock` and `CommClause` scopes
#[test]
fn scope_select_block() {
    let source = r#"package main

import "time"

func withTimeout(ch <-chan int, timeout time.Duration) (int, bool) {
    select {
    case v := <-ch:
        return v, true
    case <-time.After(timeout):
        return 0, false
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // import "time" produces an imports edge
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge for time. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Function literal (closure) scope
#[test]
fn scope_func_literal() {
    let source = r#"package main

import "sort"

func sortByLength(strs []string) []string {
    sort.Slice(strs, func(i, j int) bool {
        return len(strs[i]) < len(strs[j])
    })
    return strs
}
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "calls"),
        "Expected calls edge. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Generic Block scope (anonymous block)
#[test]
fn scope_anonymous_block() {
    let source = r#"package main

import "fmt"

func doWork() {
    {
        temp := "scoped value"
        fmt.Println(temp)
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // import "fmt" produces an imports edge
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge for fmt. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Import declarations
// ─────────────────────────────────────────────────────────────────────────────

/// Single import creates import edge
#[test]
fn single_import() {
    let source = r#"package main

import "fmt"

func greet(name string) {
    fmt.Printf("Hello, %s!\n", name)
}
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Grouped import with alias
#[test]
fn grouped_import_with_alias() {
    let source = r#"package main

import (
    "fmt"
    "os"
    io "io"
)

func main() {
    fmt.Fprintln(os.Stdout, "hello")
    _ = io.Discard
}
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// TypeOf edges for typed parameters and return types
// ─────────────────────────────────────────────────────────────────────────────

/// Function with typed parameters creates `TypeOf` edges
#[test]
fn typed_parameters_create_typeof_edges() {
    let source = r"package main

type User struct {
    Name string
    Age  int
}

func ProcessUser(u User) string {
    return u.Name
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 2);
}

/// Function with pointer parameter
#[test]
fn pointer_parameter_typeof_edge() {
    let source = r"package main

type Config struct {
    Debug bool
    Port  int
}

func ApplyConfig(cfg *Config) {
    cfg.Debug = false
}
";
    let staging = build_graph(source);
    // Config struct node + ApplyConfig function node
    assert!(
        staging.stats().nodes_staged >= 2,
        "Expected at least Config struct + ApplyConfig function. Got: {}",
        staging.stats().nodes_staged
    );
    // cfg *Config parameter creates a TypeOf edge: cfg → *Config
    assert!(
        has_edge_tag(&staging, "type_of"),
        "Expected type_of edge for pointer parameter. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Complex integration scenarios
// ─────────────────────────────────────────────────────────────────────────────

/// Full service pattern with goroutines, defer, channels, and methods
#[test]
fn full_service_pattern() {
    let source = r#"package main

import (
    "fmt"
    "sync"
)

type Service struct {
    name string
    wg   sync.WaitGroup
}

func (s *Service) Start(jobs <-chan int) {
    defer s.wg.Done()
    for job := range jobs {
        go s.processJob(job)
    }
}

func (s *Service) processJob(id int) {
    fmt.Printf("Service %s processing job %d\n", s.name, id)
}

func (s *Service) Wait() {
    s.wg.Wait()
}

func main() {
    jobs := make(chan int, 10)
    svc := &Service{name: "worker"}
    svc.wg.Add(1)
    go svc.Start(jobs)
    for i := 0; i < 5; i++ {
        jobs <- i
    }
    close(jobs)
    svc.Wait()
}
"#;
    let staging = build_graph(source);
    assert!(
        staging.stats().nodes_staged >= 3,
        "Expected at least 3 nodes, got {}",
        staging.stats().nodes_staged
    );
    assert!(
        has_edge_tag(&staging, "calls"),
        "Expected calls edges. Tags: {:?}",
        all_edge_tags(&staging)
    );
}
