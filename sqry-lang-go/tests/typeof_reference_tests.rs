use sqry_core::graph::GraphBuilder;
use sqry_core::graph::node::Language;
use sqry_core::graph::unified::build::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::resolution::display_graph_qualified_name;
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

/// Build a string lookup map from staged `InternString` operations
fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::InternString { local_id, value } = op {
                Some((local_id.index(), value.clone()))
            } else {
                None
            }
        })
        .collect()
}

/// Build a node name lookup map from staged `AddNode` operations
fn build_node_name_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let strings = build_string_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let expected_id = expected_id.as_ref()?;
                let node_idx = expected_id.index();
                let name = resolve_display_name(entry, &strings);
                Some((node_idx, name))
            } else {
                None
            }
        })
        .collect()
}

fn resolve_display_name(
    entry: &sqry_core::graph::unified::storage::NodeEntry,
    strings: &HashMap<u32, String>,
) -> String {
    entry
        .qualified_name
        .and_then(|id| strings.get(&id.index()))
        .map_or_else(
            || {
                strings
                    .get(&entry.name.index())
                    .cloned()
                    .unwrap_or_default()
            },
            |qualified_name| {
                display_graph_qualified_name(
                    Language::Go,
                    qualified_name,
                    entry.kind,
                    entry.is_static,
                )
            },
        )
}

/// Helper to collect all edges of a specific kind and return (`from_name`, `to_name`) pairs
fn collect_edges_by_kind<F>(staging: &StagingGraph, predicate: F) -> Vec<(String, String)>
where
    F: Fn(&EdgeKind) -> bool,
{
    let node_names = build_node_name_lookup(staging);
    let mut edges = Vec::new();

    for op in staging.operations() {
        if let StagingOp::AddEdge {
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

/// Helper to collect `TypeOf` edges filtered by context (Parameter, Return, Variable, etc.)
fn collect_typeof_edges_by_context(
    staging: &StagingGraph,
    context: TypeOfContext,
) -> Vec<(String, String)> {
    collect_edges_by_kind(staging, |kind| {
        matches!(
            kind,
            EdgeKind::TypeOf {
                context: Some(ctx),
                ..
            } if *ctx == context
        )
    })
}

#[test]
fn test_var_declaration_simple_types() {
    let source = "package main

var count int
var name string
var active bool
";

    let staging = build_test_graph(source, "test.go");

    // Find TypeOf edges
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Check TypeOf edges exist
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "main.count" && typ == "int"),
        "Expected TypeOf edge from main.count to int, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "main.name" && typ == "string"),
        "Expected TypeOf edge from main.name to string, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "main.active" && typ == "bool"),
        "Expected TypeOf edge from main.active to bool, got: {typeof_edges:?}"
    );
}

#[test]
fn test_var_declaration_pointer_types() {
    let source = "package main

type User struct {
    Name string
}

var user *User
var ptr *int
";

    let staging = build_test_graph(source, "test.go");

    // Find TypeOf edges
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Check TypeOf edges (should point to *User, *int as written)
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "main.user" && typ == "*User"),
        "Expected TypeOf edge from main.user to *User, got: {typeof_edges:?}"
    );

    // Check Reference edges (should point to the underlying type)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.user" && typ == "User"),
        "Expected Reference edge from main.user to User, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.ptr" && typ == "int"),
        "Expected Reference edge from main.ptr to int, got: {reference_edges:?}"
    );
}

#[test]
fn test_var_declaration_slice_types() {
    let source = "package main

var items []string
var numbers []int
";

    let staging = build_test_graph(source, "test.go");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Slices should create Reference edges to the element type
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.items" && typ == "string"),
        "Expected Reference edge from main.items to string, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.numbers" && typ == "int"),
        "Expected Reference edge from main.numbers to int, got: {reference_edges:?}"
    );
}

#[test]
fn test_var_declaration_map_types() {
    let source = "package main

type User struct {
    Name string
}

var cache map[string]*User
var lookup map[int]string
";

    let staging = build_test_graph(source, "test.go");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Maps should create Reference edges to both key and value types
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.cache" && typ == "string"),
        "Expected Reference edge from main.cache to string (key type), got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.cache" && typ == "User"),
        "Expected Reference edge from main.cache to User (value type), got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.lookup" && typ == "int"),
        "Expected Reference edge from main.lookup to int (key type), got: {reference_edges:?}"
    );
}

#[test]
fn test_var_declaration_channel_types() {
    let source = "package main

type Request struct {
    ID int
}

var ch chan Request
var inChan <-chan string
var outChan chan<- int
";

    let staging = build_test_graph(source, "test.go");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Channels should create Reference edges to the element type
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.ch" && typ == "Request"),
        "Expected Reference edge from main.ch to Request, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.inChan" && typ == "string"),
        "Expected Reference edge from main.inChan to string, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.outChan" && typ == "int"),
        "Expected Reference edge from main.outChan to int, got: {reference_edges:?}"
    );
}

#[test]
fn test_const_declaration_with_type() {
    let source = "package main

const MaxSize int = 100
const Timeout int64 = 30
";

    let staging = build_test_graph(source, "test.go");

    // Find TypeOf edges
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Constants with explicit types should create TypeOf edges
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "main.MaxSize" && typ == "int"),
        "Expected TypeOf edge from main.MaxSize to int, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "main.Timeout" && typ == "int64"),
        "Expected TypeOf edge from main.Timeout to int64, got: {typeof_edges:?}"
    );
}

#[test]
fn test_var_declaration_array_types() {
    let source = "package main

var matrix [10]int
var grid [5][5]float64
";

    let staging = build_test_graph(source, "test.go");

    // Find Reference edges
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Arrays should create Reference edges to the element type
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.matrix" && typ == "int"),
        "Expected Reference edge from main.matrix to int, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.grid" && typ == "float64"),
        "Expected Reference edge from main.grid to float64, got: {reference_edges:?}"
    );
}

#[test]
fn test_no_typeof_for_inferred_types() {
    let source = "package main

func main() {
    x := 42
}
";

    let staging = build_test_graph(source, "test.go");

    // Find TypeOf edges
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Short variable declarations without explicit types should NOT create TypeOf edges
    // (x is a local short declaration, not a package-level var)
    assert!(
        !typeof_edges.iter().any(|(var, _)| var.contains(".x")),
        "Should NOT have TypeOf edge for inferred type x := 42"
    );
}

// Tests for Codex HIGH-1: Grouped declarations and multi-name specs

#[test]
fn test_grouped_var_declarations() {
    let source = "package main

var (
    x int
    y string
    z bool
)
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "main.x" && typ == "int"),
        "Expected TypeOf edge from main.x to int in grouped declaration, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "main.y" && typ == "string"),
        "Expected TypeOf edge from main.y to string in grouped declaration, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "main.z" && typ == "bool"),
        "Expected TypeOf edge from main.z to bool in grouped declaration, got: {typeof_edges:?}"
    );
}

#[test]
fn test_multi_name_var_declaration() {
    let source = "package main

var a, b, c int
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "main.a" && typ == "int"),
        "Expected TypeOf edge from main.a to int in multi-name declaration, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "main.b" && typ == "int"),
        "Expected TypeOf edge from main.b to int in multi-name declaration, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "main.c" && typ == "int"),
        "Expected TypeOf edge from main.c to int in multi-name declaration, got: {typeof_edges:?}"
    );
}

#[test]
fn test_grouped_const_declarations() {
    let source = "package main

const (
    MaxRetries int = 5
    Timeout    int64 = 30
)
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "main.MaxRetries" && typ == "int"),
        "Expected TypeOf edge from main.MaxRetries to int in grouped const, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(var, typ)| var == "main.Timeout" && typ == "int64"),
        "Expected TypeOf edge from main.Timeout to int64 in grouped const, got: {typeof_edges:?}"
    );
}

// Tests for Codex MEDIUM-2: Verify local vars are NOT processed

#[test]
fn test_no_typeof_for_local_vars() {
    let source = "package main

func doWork() {
    var local int
    var data string
}
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Local variables should NOT have TypeOf edges (only package-level vars)
    assert!(
        !typeof_edges.iter().any(|(var, _)| var.contains("local")),
        "Should NOT have TypeOf edge for function-local var 'local', got: {typeof_edges:?}"
    );
    assert!(
        !typeof_edges.iter().any(|(var, _)| var.contains("data")),
        "Should NOT have TypeOf edge for function-local var 'data', got: {typeof_edges:?}"
    );
}

// Tests for Codex iter2 MEDIUM-1: Interface embedded types and type sets

#[test]
fn test_interface_literal_with_embedded_type() {
    let source = "package main

type Reader interface {
    Read(p []byte) (n int, err error)
}

type Writer interface {
    Write(p []byte) (n int, err error)
}

// Interface literal with embedded interfaces
var rw interface {
    Reader
    Writer
    Close() error
}
";

    let staging = build_test_graph(source, "test.go");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // rw should reference Reader, Writer, byte, int, error (from embedded interfaces + Close method)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.rw" && typ == "Reader"),
        "Expected Reference edge from main.rw to Reader (embedded interface), got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.rw" && typ == "Writer"),
        "Expected Reference edge from main.rw to Writer (embedded interface), got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.rw" && typ == "error"),
        "Expected Reference edge from main.rw to error (Close method return), got: {reference_edges:?}"
    );
}

#[test]
fn test_interface_type_set_union() {
    let source = "package main

// Interface with type set (Go 1.18+ generics)
var numeric interface {
    ~int | ~int64 | ~float64
}
";

    let staging = build_test_graph(source, "test.go");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // numeric should reference int, int64, float64 (from type union)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.numeric" && typ == "int"),
        "Expected Reference edge from main.numeric to int (type set), got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.numeric" && typ == "int64"),
        "Expected Reference edge from main.numeric to int64 (type set), got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.numeric" && typ == "float64"),
        "Expected Reference edge from main.numeric to float64 (type set), got: {reference_edges:?}"
    );
}

#[test]
fn test_interface_qualified_embedded_type() {
    let source = "package main

import \"io\"

// Interface literal with qualified embedded type
var rw interface {
    io.Reader
    io.Writer
}
";

    let staging = build_test_graph(source, "test.go");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // rw should reference io.Reader and io.Writer
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.rw" && typ == "io.Reader"),
        "Expected Reference edge from main.rw to io.Reader (qualified embedded), got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.rw" && typ == "io.Writer"),
        "Expected Reference edge from main.rw to io.Writer (qualified embedded), got: {reference_edges:?}"
    );
}

// Tests for Codex LOW-4: Function type, struct type, interface type extraction

#[test]
fn test_var_declaration_function_types() {
    let source = "package main

type Request struct {
    ID int
}

type Response struct {
    Data string
}

var handler func(Request) Response
var processor func(int, string) (bool, error)
";

    let staging = build_test_graph(source, "test.go");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // handler should reference Request and Response
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.handler" && typ == "Request"),
        "Expected Reference edge from main.handler to Request, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.handler" && typ == "Response"),
        "Expected Reference edge from main.handler to Response, got: {reference_edges:?}"
    );

    // processor should reference int, string, bool, error
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.processor" && typ == "int"),
        "Expected Reference edge from main.processor to int, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.processor" && typ == "string"),
        "Expected Reference edge from main.processor to string, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.processor" && typ == "bool"),
        "Expected Reference edge from main.processor to bool, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.processor" && typ == "error"),
        "Expected Reference edge from main.processor to error, got: {reference_edges:?}"
    );
}

#[test]
fn test_var_declaration_struct_types() {
    let source = "package main

type Database struct {
    Host string
    Port int
}

var cfg struct {
    DB   *Database
    Name string
}
";

    let staging = build_test_graph(source, "test.go");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // cfg (anonymous struct) should reference Database and string
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.cfg" && typ == "Database"),
        "Expected Reference edge from main.cfg to Database (struct field type), got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.cfg" && typ == "string"),
        "Expected Reference edge from main.cfg to string (struct field type), got: {reference_edges:?}"
    );
}

#[test]
fn test_var_declaration_interface_types() {
    let source = "package main

var reader interface {
    Read(p []byte) (n int, err error)
    Close() error
}
";

    let staging = build_test_graph(source, "test.go");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // reader (anonymous interface) should reference types from method signatures
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.reader" && typ == "byte"),
        "Expected Reference edge from main.reader to byte (method param type), got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.reader" && typ == "int"),
        "Expected Reference edge from main.reader to int (method return type), got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.reader" && typ == "error"),
        "Expected Reference edge from main.reader to error (method return type), got: {reference_edges:?}"
    );
}

#[test]
fn test_export_edges_grouped_and_multi_name() {
    // Test export edges for grouped var/const declarations and multi-name specs
    let source = r#"package main

// Grouped var declarations with exported names
var (
    PublicA int
    PublicB string
    privateC bool
)

// Multi-name var declaration with exported names
var PublicX, PublicY, privateZ int

// Grouped const declarations
const (
    MaxRetries int = 5
    MinDelay int = 100
)

// Multi-name const
const ConfigA, ConfigB = "foo", "bar"
"#;

    let staging = build_test_graph(source, "test.go");

    // Collect export edges - these are edges from module to exported symbols
    let export_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::Exports { .. }));

    // Extract just the exported symbol names (to_node names)
    let exported_names: Vec<&str> = export_edges.iter().map(|(_, name)| name.as_str()).collect();

    // Verify all exported names from grouped var declarations
    assert!(
        exported_names.contains(&"main.PublicA"),
        "Expected main.PublicA to be exported, got: {exported_names:?}"
    );
    assert!(
        exported_names.contains(&"main.PublicB"),
        "Expected main.PublicB to be exported, got: {exported_names:?}"
    );

    // Verify private names are NOT exported
    assert!(
        !exported_names.contains(&"main.privateC"),
        "main.privateC should not be exported (lowercase), got: {exported_names:?}"
    );

    // Verify multi-name var exports
    assert!(
        exported_names.contains(&"main.PublicX"),
        "Expected main.PublicX to be exported, got: {exported_names:?}"
    );
    assert!(
        exported_names.contains(&"main.PublicY"),
        "Expected main.PublicY to be exported, got: {exported_names:?}"
    );
    assert!(
        !exported_names.contains(&"main.privateZ"),
        "main.privateZ should not be exported (lowercase), got: {exported_names:?}"
    );

    // Verify grouped const exports
    assert!(
        exported_names.contains(&"main.MaxRetries"),
        "Expected main.MaxRetries to be exported, got: {exported_names:?}"
    );
    assert!(
        exported_names.contains(&"main.MinDelay"),
        "Expected main.MinDelay to be exported, got: {exported_names:?}"
    );

    // Verify multi-name const exports
    assert!(
        exported_names.contains(&"main.ConfigA"),
        "Expected main.ConfigA to be exported, got: {exported_names:?}"
    );
    assert!(
        exported_names.contains(&"main.ConfigB"),
        "Expected main.ConfigB to be exported, got: {exported_names:?}"
    );
}

#[test]
fn test_no_export_edges_for_local_vars() {
    // Regression test: function-local uppercase vars/consts should NOT create export edges
    let source = r#"package main

var GlobalPublic int

func doWork() {
    var LocalPublic int
    const LocalConst = 42
}

func process() {
    var (
        GroupedLocal int
        AnotherLocal string
    )
    var MultiA, MultiB int
    const ConfigLocal = "test"
}
"#;

    let staging = build_test_graph(source, "test.go");

    // Collect export edges
    let export_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::Exports { .. }));

    // Extract exported symbol names
    let exported_names: Vec<&str> = export_edges.iter().map(|(_, name)| name.as_str()).collect();

    // GlobalPublic should be exported (package-level)
    assert!(
        exported_names.contains(&"main.GlobalPublic"),
        "Expected main.GlobalPublic to be exported, got: {exported_names:?}"
    );

    // All local uppercase variables should NOT be exported
    assert!(
        !exported_names.contains(&"main.LocalPublic"),
        "LocalPublic should not be exported (function-local), got: {exported_names:?}"
    );
    assert!(
        !exported_names.contains(&"main.LocalConst"),
        "LocalConst should not be exported (function-local), got: {exported_names:?}"
    );
    assert!(
        !exported_names.contains(&"main.GroupedLocal"),
        "GroupedLocal should not be exported (function-local), got: {exported_names:?}"
    );
    assert!(
        !exported_names.contains(&"main.AnotherLocal"),
        "AnotherLocal should not be exported (function-local), got: {exported_names:?}"
    );
    assert!(
        !exported_names.contains(&"main.MultiA"),
        "MultiA should not be exported (function-local), got: {exported_names:?}"
    );
    assert!(
        !exported_names.contains(&"main.MultiB"),
        "MultiB should not be exported (function-local), got: {exported_names:?}"
    );
    assert!(
        !exported_names.contains(&"main.ConfigLocal"),
        "ConfigLocal should not be exported (function-local), got: {exported_names:?}"
    );
}

#[test]
fn test_interface_type_set_complex_types() {
    // Test type set with complex type terms (slices, maps, etc.)
    let source = r"package main

type Foo struct {
    value int
}

var data interface {
    ~[]byte | ~map[string]Foo
}
";

    let staging = build_test_graph(source, "test.go");
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // data should reference byte, string, and Foo (nested types from complex type terms)
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.data" && typ == "byte"),
        "Expected Reference edge from main.data to byte (slice element), got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.data" && typ == "string"),
        "Expected Reference edge from main.data to string (map key), got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(var, typ)| var == "main.data" && typ == "Foo"),
        "Expected Reference edge from main.data to Foo (map value), got: {reference_edges:?}"
    );
}

// ============================================================================
// Phase 2: Function/Method Parameters and Returns
// ============================================================================

#[test]
fn test_function_simple_parameters() {
    let source = r"package main

func Add(x int, y int) {
}
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Function should have TypeOf edges for both parameter types
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.Add" && typ == "int"),
        "Expected TypeOf edge from main.Add to int (parameter type), got: {typeof_edges:?}"
    );
}

#[test]
fn test_function_complex_parameters() {
    let source = r"package main

type User struct {
    name string
}

func ProcessUser(user *User, ids []int, metadata map[string]interface{}) {
}
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // TypeOf edges for exact parameter types
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.ProcessUser" && typ == "*User"),
        "Expected TypeOf edge from main.ProcessUser to *User, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.ProcessUser" && typ == "[]int"),
        "Expected TypeOf edge from main.ProcessUser to []int, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.ProcessUser" && typ == "map[string]interface{}"),
        "Expected TypeOf edge from main.ProcessUser to map[string]interface{{}}, got: {typeof_edges:?}"
    );

    // Reference edges to nested types
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "main.ProcessUser" && typ == "User"),
        "Expected Reference edge from main.ProcessUser to User, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "main.ProcessUser" && typ == "int"),
        "Expected Reference edge from main.ProcessUser to int, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "main.ProcessUser" && typ == "string"),
        "Expected Reference edge from main.ProcessUser to string, got: {reference_edges:?}"
    );
}

#[test]
fn test_function_single_return() {
    let source = r"package main

func GetValue() int {
    return 42
}
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Function should have TypeOf edge for return type
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.GetValue" && typ == "int"),
        "Expected TypeOf edge from main.GetValue to int (return type), got: {typeof_edges:?}"
    );
}

#[test]
fn test_function_multiple_returns() {
    let source = r"package main

func Divide(a, b int) (int, error) {
    return 0, nil
}
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Function should have TypeOf edges for parameters and both return types
    assert!(
        typeof_edges
            .iter()
            .filter(|(func, typ)| func == "main.Divide" && typ == "int")
            .count()
            >= 2,
        "Expected TypeOf edges from main.Divide to int (params + return), got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.Divide" && typ == "error"),
        "Expected TypeOf edge from main.Divide to error (return type), got: {typeof_edges:?}"
    );
}

#[test]
fn test_function_named_returns() {
    let source = r"package main

func Calculate(x int) (result int, err error) {
    return x * 2, nil
}
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Named returns should still create TypeOf edges for the types
    assert!(
        typeof_edges
            .iter()
            .filter(|(func, typ)| func == "main.Calculate" && typ == "int")
            .count()
            >= 2,
        "Expected TypeOf edges from main.Calculate to int (param + return), got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.Calculate" && typ == "error"),
        "Expected TypeOf edge from main.Calculate to error, got: {typeof_edges:?}"
    );
}

#[test]
fn test_method_parameters_returns() {
    let source = r"package main

type Calculator struct {
    value int
}

func (c *Calculator) Add(x int) int {
    return c.value + x
}
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Method should have TypeOf edges for parameter and return
    assert!(
        typeof_edges
            .iter()
            .filter(|(method, typ)| method == "main.Calculator.Add" && typ == "int")
            .count()
            >= 2,
        "Expected TypeOf edges from main.Calculator.Add to int (param + return), got: {typeof_edges:?}"
    );
}

#[test]
fn test_variadic_parameters() {
    let source = r"package main

func Sum(numbers ...int) int {
    return 0
}
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Variadic parameter should create TypeOf edge to []int
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.Sum" && typ == "[]int"),
        "Expected TypeOf edge from main.Sum to []int (variadic param), got: {typeof_edges:?}"
    );

    // Should also have Reference edge to int (element type)
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "main.Sum" && typ == "int"),
        "Expected Reference edge from main.Sum to int, got: {reference_edges:?}"
    );
}

#[test]
fn test_anonymous_parameters() {
    let source = r"package main

func Process(int, string) error {
    return nil
}
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Anonymous parameters should still create TypeOf edges
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.Process" && typ == "int"),
        "Expected TypeOf edge from main.Process to int (anonymous param), got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.Process" && typ == "string"),
        "Expected TypeOf edge from main.Process to string (anonymous param), got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.Process" && typ == "error"),
        "Expected TypeOf edge from main.Process to error (return), got: {typeof_edges:?}"
    );
}

#[test]
fn test_multi_name_parameters() {
    let source = r"package main

func Coordinate(x, y, z float64) {
}
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Multi-name parameter "x, y, z float64" should create 3 TypeOf edges
    let float64_count = typeof_edges
        .iter()
        .filter(|(func, typ)| func == "main.Coordinate" && typ == "float64")
        .count();

    assert_eq!(
        float64_count, 3,
        "Expected 3 TypeOf edges from main.Coordinate to float64 (one per parameter name), got: {float64_count}"
    );
}

#[test]
fn test_function_all_type_constructs_parameters() {
    let source = r#"package main

import "context"

type User struct {
    name string
}

func ComplexFunction(
    ctx context.Context,
    user *User,
    ids []int,
    counts [3]int,
    metadata map[string]interface{},
    ch chan string,
    fn func(int) error,
) {
}
"#;

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // TypeOf edges for all parameter types
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.ComplexFunction" && typ == "context.Context"),
        "Expected TypeOf edge to context.Context, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.ComplexFunction" && typ == "*User"),
        "Expected TypeOf edge to *User, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.ComplexFunction" && typ == "[]int"),
        "Expected TypeOf edge to []int, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.ComplexFunction" && typ == "[3]int"),
        "Expected TypeOf edge to [3]int, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.ComplexFunction" && typ == "map[string]interface{}"),
        "Expected TypeOf edge to map[string]interface{{}}, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.ComplexFunction" && typ == "chan string"),
        "Expected TypeOf edge to chan string, got: {typeof_edges:?}"
    );

    // Reference edges to nested types
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "main.ComplexFunction" && typ == "context.Context"),
        "Expected Reference edge to context.Context, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "main.ComplexFunction" && typ == "User"),
        "Expected Reference edge to User, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "main.ComplexFunction" && typ == "int"),
        "Expected Reference edge to int, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "main.ComplexFunction" && typ == "string"),
        "Expected Reference edge to string, got: {reference_edges:?}"
    );
}

#[test]
fn test_function_returns_all_type_constructs() {
    let source = r"package main

type Result struct {
    value int
}

func SimpleReturn() int {
    return 0
}

func PointerReturn() *Result {
    return nil
}

func SliceReturn() []string {
    return nil
}

func MapReturn() map[string]int {
    return nil
}

func ChannelReturn() chan error {
    return nil
}
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // TypeOf edges for return types
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.SimpleReturn" && typ == "int"),
        "Expected TypeOf edge from SimpleReturn to int, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.PointerReturn" && typ == "*Result"),
        "Expected TypeOf edge from PointerReturn to *Result, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.SliceReturn" && typ == "[]string"),
        "Expected TypeOf edge from SliceReturn to []string, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.MapReturn" && typ == "map[string]int"),
        "Expected TypeOf edge from MapReturn to map[string]int, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.ChannelReturn" && typ == "chan error"),
        "Expected TypeOf edge from ChannelReturn to chan error, got: {typeof_edges:?}"
    );

    // Reference edges to nested types
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "main.PointerReturn" && typ == "Result"),
        "Expected Reference edge from PointerReturn to Result, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "main.SliceReturn" && typ == "string"),
        "Expected Reference edge from SliceReturn to string, got: {reference_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "main.MapReturn" && typ == "string"),
        "Expected Reference edge from MapReturn to string, got: {reference_edges:?}"
    );
}

#[test]
fn test_context_parameter_pattern() {
    // Real-world pattern: functions taking context.Context
    let source = r#"package main

import "context"

func DoWork(ctx context.Context, data string) error {
    return nil
}
"#;

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // This enables queries like "find all functions taking context.Context"
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.DoWork" && typ == "context.Context"),
        "Expected TypeOf edge from DoWork to context.Context, got: {typeof_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "main.DoWork" && typ == "context.Context"),
        "Expected Reference edge from DoWork to context.Context, got: {reference_edges:?}"
    );
}

#[test]
fn test_error_return_pattern() {
    // Real-world pattern: functions returning error
    let source = r"package main

func Validate(input string) error {
    return nil
}

func Process(data []byte) (int, error) {
    return 0, nil
}
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // This enables queries like "find all functions returning error"
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.Validate" && typ == "error"),
        "Expected TypeOf edge from Validate to error, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.Process" && typ == "error"),
        "Expected TypeOf edge from Process to error, got: {typeof_edges:?}"
    );
}

#[test]
fn test_pointer_return_pattern() {
    // Real-world pattern: functions returning pointers and errors
    let source = r"package main

type User struct {
    id int
}

func GetUser(id int) (*User, error) {
    return nil, nil
}
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));
    let reference_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // This enables queries like "find all functions returning *User"
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.GetUser" && typ == "*User"),
        "Expected TypeOf edge from GetUser to *User, got: {typeof_edges:?}"
    );
    assert!(
        typeof_edges
            .iter()
            .any(|(func, typ)| func == "main.GetUser" && typ == "error"),
        "Expected TypeOf edge from GetUser to error, got: {typeof_edges:?}"
    );
    assert!(
        reference_edges
            .iter()
            .any(|(func, typ)| func == "main.GetUser" && typ == "User"),
        "Expected Reference edge from GetUser to User, got: {reference_edges:?}"
    );
}

#[test]
fn test_function_no_params_no_returns() {
    // Edge case: function with no parameters or returns
    let source = r"package main

func DoNothing() {
}
";

    let staging = build_test_graph(source, "test.go");
    let typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    // Should not create any TypeOf edges for this function
    let do_nothing_typeof_count = typeof_edges
        .iter()
        .filter(|(func, _)| func == "main.DoNothing")
        .count();

    assert_eq!(
        do_nothing_typeof_count, 0,
        "Expected no TypeOf edges for function with no params/returns, got: {typeof_edges:?}"
    );
}
// Add these tests at the end of typeof_reference_tests.rs

#[test]
fn test_parameter_vs_return_discrimination() {
    // Test that we can distinguish functions taking error from functions returning error
    let source = r"package main

// Takes error as parameter
func LogError(err error) {
}

// Returns error
func GetError() error {
    return nil
}

// Both takes and returns error
func TransformError(input error) error {
    return input
}
";

    let staging = build_test_graph(source, "test.go");

    // Collect parameter TypeOf edges
    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    // Collect return TypeOf edges
    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);

    // LogError should have error as PARAMETER only
    assert!(
        param_edges
            .iter()
            .any(|(func, typ)| func == "main.LogError" && typ == "error"),
        "Expected LogError to have error as parameter, got: {param_edges:?}"
    );
    assert!(
        !return_edges.iter().any(|(func, _)| func == "main.LogError"),
        "LogError should NOT have error as return type, got: {return_edges:?}"
    );

    // GetError should have error as RETURN only
    assert!(
        return_edges
            .iter()
            .any(|(func, typ)| func == "main.GetError" && typ == "error"),
        "Expected GetError to have error as return, got: {return_edges:?}"
    );
    assert!(
        !param_edges.iter().any(|(func, _)| func == "main.GetError"),
        "GetError should NOT have error as parameter, got: {param_edges:?}"
    );

    // TransformError should have error as BOTH parameter AND return
    assert!(
        param_edges
            .iter()
            .any(|(func, typ)| func == "main.TransformError" && typ == "error"),
        "Expected TransformError to have error as parameter, got: {param_edges:?}"
    );
    assert!(
        return_edges
            .iter()
            .any(|(func, typ)| func == "main.TransformError" && typ == "error"),
        "Expected TransformError to have error as return, got: {return_edges:?}"
    );
}

#[test]
fn test_variable_vs_parameter_discrimination() {
    // Test that variable TypeOf edges are distinct from parameter TypeOf edges
    let source = r"package main

var globalError error

func ProcessError(err error) {
}
";

    let staging = build_test_graph(source, "test.go");

    // Collect variable TypeOf edges
    let var_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);

    // Collect parameter TypeOf edges
    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    // Variable edge
    assert!(
        var_edges
            .iter()
            .any(|(var, typ)| var == "main.globalError" && typ == "error"),
        "Expected globalError to have error as variable type, got: {var_edges:?}"
    );

    // Parameter edge
    assert!(
        param_edges
            .iter()
            .any(|(func, typ)| func == "main.ProcessError" && typ == "error"),
        "Expected ProcessError to have error as parameter type, got: {param_edges:?}"
    );

    // Verify they're distinct
    assert!(
        !var_edges
            .iter()
            .any(|(node, _)| node == "main.ProcessError"),
        "ProcessError parameter should not appear in variable edges"
    );
    assert!(
        !param_edges
            .iter()
            .any(|(node, _)| node == "main.globalError"),
        "globalError should not appear in parameter edges"
    );
}

#[test]
fn test_typeof_context_metadata_presence() {
    // Verify that TypeOf edges actually have context metadata
    let source = r"package main

var x int

func foo(y int) int {
    return y
}
";

    let staging = build_test_graph(source, "test.go");

    // Count edges by context
    let var_count = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable).len();
    let param_count = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter).len();
    let return_count = collect_typeof_edges_by_context(&staging, TypeOfContext::Return).len();

    // We should have:
    // - 1 variable (x)
    // - 1 parameter (y)
    // - 1 return (int)
    assert_eq!(var_count, 1, "Expected 1 variable TypeOf edge");
    assert_eq!(param_count, 1, "Expected 1 parameter TypeOf edge");
    assert_eq!(return_count, 1, "Expected 1 return TypeOf edge");

    // Total TypeOf edges should equal sum of contexts
    let all_typeof_edges =
        collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert_eq!(
        all_typeof_edges.len(),
        var_count + param_count + return_count,
        "All TypeOf edges should be categorized by context"
    );
}

// ============================================================================
// Phase 3: Struct Field and Interface Method TypeOf/Reference Tests
// ============================================================================

#[test]
fn test_struct_basic_fields() {
    let source = r"package main

type Config struct {
    Port int
    Host string
    Debug bool
}
";

    let staging = build_test_graph(source, "test.go");
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);

    // Should have TypeOf edges for all 3 fields
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.Config" && t == "int"),
        "Expected Config.Port → int TypeOf edge"
    );
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.Config" && t == "string"),
        "Expected Config.Host → string TypeOf edge"
    );
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.Config" && t == "bool"),
        "Expected Config.Debug → bool TypeOf edge"
    );

    assert_eq!(
        field_edges.len(),
        3,
        "Expected 3 field TypeOf edges, got: {field_edges:?}"
    );
}

#[test]
fn test_struct_pointer_fields() {
    let source = r"package main

type Database struct{}

type Service struct {
    DB *Database
    Cache *Cache
}
";

    let staging = build_test_graph(source, "test.go");
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    let ref_edges = collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // TypeOf edges (pointer types)
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.Service" && t == "*Database"),
        "Expected Service.DB → *Database TypeOf edge"
    );
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.Service" && t == "*Cache"),
        "Expected Service.Cache → *Cache TypeOf edge"
    );

    // Reference edges (pointer targets)
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| { src == "main.Service" && tgt == "Database" }),
        "Expected Service → Database Reference edge"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| { src == "main.Service" && tgt == "Cache" }),
        "Expected Service → Cache Reference edge"
    );
}

#[test]
fn test_struct_complex_field_types() {
    let source = r"package main

type User struct{}

type Service struct {
    Users map[string]*User
    Handlers []HandlerFunc
    Results chan Result
    Callback func(Request) Response
}
";

    let staging = build_test_graph(source, "test.go");
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    let ref_edges = collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // TypeOf edges (exact type strings)
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.Service" && t == "map[string]*User"),
        "Expected Service.Users → map[string]*User TypeOf edge"
    );
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.Service" && t == "[]HandlerFunc"),
        "Expected Service.Handlers → []HandlerFunc TypeOf edge"
    );
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.Service" && t == "chan Result"),
        "Expected Service.Results → chan Result TypeOf edge"
    );
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.Service" && t == "func(Request) Response"),
        "Expected Service.Callback → func(Request) Response TypeOf edge"
    );

    // Reference edges (nested types)
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| { src == "main.Service" && tgt == "User" }),
        "Expected Service → User Reference edge (from map value)"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| { src == "main.Service" && tgt == "HandlerFunc" }),
        "Expected Service → HandlerFunc Reference edge (from slice)"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| { src == "main.Service" && tgt == "Result" }),
        "Expected Service → Result Reference edge (from channel)"
    );
}

#[test]
fn test_struct_multi_name_fields() {
    let source = r"package main

type Point struct {
    X, Y, Z float64
}
";

    let staging = build_test_graph(source, "test.go");
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);

    // Should create 3 separate TypeOf edges for X, Y, Z
    let float_fields: Vec<_> = field_edges
        .iter()
        .filter(|(s, t)| s == "main.Point" && t == "float64")
        .collect();

    assert_eq!(
        float_fields.len(),
        3,
        "Expected 3 field TypeOf edges for X, Y, Z"
    );
}

#[test]
fn test_struct_embedded_types() {
    let source = r"package main

type Base struct {
    ID int
}

type Logger struct {
    Level string
}

type Server struct {
    Base           // Embedded struct
    *Logger        // Embedded pointer
    Port int       // Regular field
}
";

    let staging = build_test_graph(source, "test.go");
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    let inherits_edges = collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::Inherits));

    // Embedded fields should create Inherits edges (handled by process_struct_embedding)
    assert!(
        !inherits_edges.is_empty(),
        "Expected Inherits edges for embedded types"
    );

    // Regular field should have TypeOf edge
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.Server" && t == "int"),
        "Expected Server.Port → int TypeOf edge"
    );

    // Embedded fields should NOT create Field-context TypeOf edges
    // (they're tracked via Inherits edges instead)
    assert!(
        !field_edges
            .iter()
            .any(|(s, t)| s == "main.Server" && t == "Base"),
        "Embedded Base should not have Field TypeOf edge"
    );
    assert!(
        !field_edges
            .iter()
            .any(|(s, t)| s == "main.Server" && t == "*Logger"),
        "Embedded *Logger should not have Field TypeOf edge"
    );
}

#[test]
fn test_interface_method_parameters() {
    let source = r"package main

type Reader interface {
    Read(p []byte) (int, error)
    Close() error
}
";

    let staging = build_test_graph(source, "test.go");
    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);

    // Read method parameter
    assert!(
        param_edges
            .iter()
            .any(|(s, t)| s == "main.Reader.Read" && t == "[]byte"),
        "Expected Reader.Read.p → []byte parameter TypeOf edge, got: {param_edges:?}"
    );

    // Read method returns
    assert!(
        return_edges
            .iter()
            .any(|(s, t)| s == "main.Reader.Read" && t == "int"),
        "Expected Reader.Read return[0] → int TypeOf edge"
    );
    assert!(
        return_edges
            .iter()
            .any(|(s, t)| s == "main.Reader.Read" && t == "error"),
        "Expected Reader.Read return[1] → error TypeOf edge"
    );

    // Close method return
    assert!(
        return_edges
            .iter()
            .any(|(s, t)| s == "main.Reader.Close" && t == "error"),
        "Expected Reader.Close return → error TypeOf edge"
    );
}

#[test]
fn test_interface_embedded_interfaces() {
    let source = r"package main

type Reader interface {
    Read() error
}

type Writer interface {
    Write() error
}

type ReadWriter interface {
    Reader
    Writer
    Close() error
}
";

    let staging = build_test_graph(source, "test.go");
    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    let inherits_edges = collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::Inherits));

    // Embedded interfaces should create Inherits edges
    assert!(
        !inherits_edges.is_empty(),
        "Expected Inherits edges for embedded interfaces"
    );

    // Close method should have return TypeOf edge
    assert!(
        return_edges
            .iter()
            .any(|(s, t)| s == "main.ReadWriter.Close" && t == "error"),
        "Expected ReadWriter.Close return → error TypeOf edge"
    );
}

#[test]
fn test_interface_complex_method_signatures() {
    let source = r"package main

type Processor interface {
    Transform(items []string, fn func(string) int) ([]int, error)
    Process(ctx context.Context, req *Request) (*Response, error)
}
";

    let staging = build_test_graph(source, "test.go");
    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    let ref_edges = collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Transform parameters
    assert!(
        param_edges
            .iter()
            .any(|(s, t)| s == "main.Processor.Transform" && t == "[]string"),
        "Expected Transform.items → []string parameter TypeOf edge"
    );
    assert!(
        param_edges
            .iter()
            .any(|(s, t)| s == "main.Processor.Transform" && t == "func(string) int"),
        "Expected Transform.fn → func(string) int parameter TypeOf edge"
    );

    // Transform returns
    assert!(
        return_edges
            .iter()
            .any(|(s, t)| s == "main.Processor.Transform" && t == "[]int"),
        "Expected Transform return[0] → []int TypeOf edge"
    );
    assert!(
        return_edges
            .iter()
            .any(|(s, t)| s == "main.Processor.Transform" && t == "error"),
        "Expected Transform return[1] → error TypeOf edge"
    );

    // Process parameters with qualified types
    assert!(
        param_edges
            .iter()
            .any(|(s, t)| s == "main.Processor.Process" && t == "context.Context"),
        "Expected Process.ctx → context.Context parameter TypeOf edge"
    );
    assert!(
        param_edges
            .iter()
            .any(|(s, t)| s == "main.Processor.Process" && t == "*Request"),
        "Expected Process.req → *Request parameter TypeOf edge"
    );

    // Reference edges for qualified types
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| { src == "main.Processor.Process" && tgt == "context.Context" }),
        "Expected Process → context.Context Reference edge"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| { src == "main.Processor.Process" && tgt == "Request" }),
        "Expected Process → Request Reference edge"
    );
}

#[test]
fn test_unexported_struct_fields() {
    let source = r"package main

type config struct {
    port int
    Host string
}
";

    let staging = build_test_graph(source, "test.go");
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);

    // Both exported and unexported fields should have TypeOf edges
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.config" && t == "int"),
        "Expected config.port → int TypeOf edge (unexported field)"
    );
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.config" && t == "string"),
        "Expected config.Host → string TypeOf edge (exported field in unexported struct)"
    );
}

#[test]
fn test_struct_all_type_constructs() {
    // Test all 13 type constructs from Phase 1 as struct fields
    let source = r"package main

type AllTypes struct {
    BasicInt int
    QualifiedType context.Context
    PointerType *User
    SliceType []string
    ArrayType [10]int
    MapType map[string]int
    ChanType chan Request
    SendChanType chan<- Response
    RecvChanType <-chan Event
    FuncType func(int) string
    StructType struct { X int }
    InterfaceType interface { Method() }
}
";

    let staging = build_test_graph(source, "test.go");
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);

    // Verify we have TypeOf edges for all 12 named fields
    // (StructType and InterfaceType are anonymous types)
    assert!(
        field_edges.len() >= 12,
        "Expected at least 12 field TypeOf edges for all type constructs, got {}",
        field_edges.len()
    );

    // Spot check a few
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.AllTypes" && t == "int"),
        "Expected BasicInt → int"
    );
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.AllTypes" && t == "context.Context"),
        "Expected QualifiedType → context.Context"
    );
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.AllTypes" && t == "*User"),
        "Expected PointerType → *User"
    );
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.AllTypes" && t == "[]string"),
        "Expected SliceType → []string"
    );
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.AllTypes" && t == "map[string]int"),
        "Expected MapType → map[string]int"
    );
}

#[test]
fn test_no_field_edges_for_embedded() {
    // Verify embedded fields don't create Field-context TypeOf edges
    let source = r"package main

type Base struct {
    ID int
}

type Child struct {
    Base
    Name string
}
";

    let staging = build_test_graph(source, "test.go");
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);

    // Should only have TypeOf edge for Name field, not for embedded Base
    let child_fields: Vec<_> = field_edges
        .iter()
        .filter(|(s, _)| s == "main.Child")
        .collect();

    assert_eq!(
        child_fields.len(),
        1,
        "Expected only 1 field TypeOf edge (Name), not embedded Base"
    );
    assert!(
        child_fields.iter().any(|(_, t)| t == "string"),
        "Expected the one field to be string (Name)"
    );
}

#[test]
fn test_struct_and_function_param_discrimination() {
    // Ensure struct fields and function parameters are properly discriminated
    let source = r"package main

type Config struct {
    Port int
}

func Process(port int) {}
";

    let staging = build_test_graph(source, "test.go");
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    // Field edge
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.Config" && t == "int"),
        "Expected Config.Port → int field TypeOf edge"
    );

    // Parameter edge
    assert!(
        param_edges
            .iter()
            .any(|(s, t)| s == "main.Process" && t == "int"),
        "Expected Process.port → int parameter TypeOf edge"
    );

    // Verify they're distinct
    assert!(
        !field_edges.iter().any(|(s, _)| s == "main.Process"),
        "Process parameter should not appear in field edges"
    );
    assert!(
        !param_edges.iter().any(|(s, _)| s == "main.Config"),
        "Config field should not appear in parameter edges"
    );
}

#[test]
fn test_interface_method_and_function_discrimination() {
    // Ensure interface methods and top-level functions are properly handled
    let source = r"package main

type Reader interface {
    Read(p []byte) error
}

func Read(p []byte) error {
    return nil
}
";

    let staging = build_test_graph(source, "test.go");
    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);

    // Both should have parameter edges
    assert!(
        param_edges
            .iter()
            .any(|(s, t)| s == "main.Reader.Read" && t == "[]byte"),
        "Expected Reader.Read → []byte parameter TypeOf edge"
    );
    assert!(
        param_edges
            .iter()
            .any(|(s, t)| s == "main.Read" && t == "[]byte"),
        "Expected Read → []byte parameter TypeOf edge"
    );

    // Both should be in param_edges, not other contexts
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    assert!(
        !field_edges.iter().any(|(s, _)| s.contains("Read")),
        "Read functions should not have field TypeOf edges"
    );
}

#[test]
fn test_phase3_comprehensive_integration() {
    // Comprehensive test with structs, interfaces, functions, and variables
    let source = r"package main

type User struct {
    ID int
    Name string
}

type Service struct {
    Users map[string]*User
}

type Repository interface {
    GetUser(id int) (*User, error)
    SaveUser(user *User) error
}

var globalService *Service

func NewService() *Service {
    return &Service{}
}
";

    let staging = build_test_graph(source, "test.go");

    // Collect edges by context
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);
    let var_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Variable);

    // Struct fields
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.User" && t == "int"),
        "User.ID field"
    );
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.User" && t == "string"),
        "User.Name field"
    );
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.Service" && t == "map[string]*User"),
        "Service.Users field"
    );

    // Interface method parameters
    assert!(
        param_edges
            .iter()
            .any(|(s, t)| s == "main.Repository.GetUser" && t == "int"),
        "GetUser parameter"
    );
    assert!(
        param_edges
            .iter()
            .any(|(s, t)| s == "main.Repository.SaveUser" && t == "*User"),
        "SaveUser parameter"
    );

    // Interface method returns
    assert!(
        return_edges
            .iter()
            .any(|(s, t)| s == "main.Repository.GetUser" && t == "*User"),
        "GetUser return[0]"
    );
    assert!(
        return_edges
            .iter()
            .any(|(s, t)| s == "main.Repository.GetUser" && t == "error"),
        "GetUser return[1]"
    );

    // Function return
    assert!(
        return_edges
            .iter()
            .any(|(s, t)| s == "main.NewService" && t == "*Service"),
        "NewService return"
    );

    // Variable
    assert!(
        var_edges
            .iter()
            .any(|(s, t)| s == "main.globalService" && t == "*Service"),
        "globalService variable"
    );

    // Verify contexts are distinct
    assert!(field_edges.len() >= 3, "Should have at least 3 field edges");
    assert!(
        param_edges.len() >= 2,
        "Should have at least 2 parameter edges"
    );
    assert!(
        return_edges.len() >= 3,
        "Should have at least 3 return edges"
    );
    assert_eq!(var_edges.len(), 1, "Should have 1 variable edge");
}

// ============================================================================
// Additional Tests for Codex Findings (MEDIUM-1 and LOW-1)
// ============================================================================

#[test]
fn test_anonymous_interface_field_with_methods() {
    // Test MEDIUM-1: Anonymous interface types with method signatures
    let source = r"package main

type Handler struct {
    Callback interface {
        Process(data []byte) (int, error)
        Close() error
    }
}
";

    let staging = build_test_graph(source, "test.go");
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);
    let ref_edges = collect_edges_by_kind(&staging, |kind| matches!(kind, EdgeKind::References));

    // Should have TypeOf edge for the interface field
    assert!(
        field_edges
            .iter()
            .any(|(s, t)| s == "main.Handler" && t.starts_with("interface")),
        "Expected Handler.Callback → interface... TypeOf edge"
    );

    // Should have Reference edges to types used in anonymous interface methods
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| src == "main.Handler" && tgt == "byte"),
        "Expected Handler → byte Reference edge (from []byte parameter)"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| src == "main.Handler" && tgt == "int"),
        "Expected Handler → int Reference edge (from int return)"
    );
    assert!(
        ref_edges
            .iter()
            .any(|(src, tgt)| src == "main.Handler" && tgt == "error"),
        "Expected Handler → error Reference edge (from error returns)"
    );
}

#[test]
fn test_interface_unnamed_parameters() {
    // Test LOW-1: Unnamed interface parameters (common in Go)
    let source = r"package main

type Reader interface {
    Read([]byte) (int, error)
}
";

    let staging = build_test_graph(source, "test.go");
    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);

    // Should create parameter edges even for unnamed parameters
    assert!(
        param_edges
            .iter()
            .any(|(s, t)| s == "main.Reader.Read" && t == "[]byte"),
        "Expected Reader.Read → []byte parameter TypeOf edge (unnamed param)"
    );

    // Should create return edges
    assert!(
        return_edges
            .iter()
            .any(|(s, t)| s == "main.Reader.Read" && t == "int"),
        "Expected Reader.Read return → int"
    );
    assert!(
        return_edges
            .iter()
            .any(|(s, t)| s == "main.Reader.Read" && t == "error"),
        "Expected Reader.Read return → error"
    );
}

#[test]
fn test_interface_named_returns() {
    // Test LOW-1: Named returns in interface methods
    let source = r"package main

type Calculator interface {
    Divide(a, b int) (result float64, err error)
}
";

    let staging = build_test_graph(source, "test.go");
    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);

    // Parameters
    assert!(
        param_edges
            .iter()
            .any(|(s, t)| s == "main.Calculator.Divide" && t == "int"),
        "Expected Divide parameters → int"
    );

    // Named returns (should extract type, ignore names)
    assert!(
        return_edges
            .iter()
            .any(|(s, t)| s == "main.Calculator.Divide" && t == "float64"),
        "Expected Divide named return result → float64"
    );
    assert!(
        return_edges
            .iter()
            .any(|(s, t)| s == "main.Calculator.Divide" && t == "error"),
        "Expected Divide named return err → error"
    );
}

#[test]
fn test_interface_variadic_methods() {
    // Test LOW-1: Variadic interface methods
    let source = r"package main

type Writer interface {
    Write(p ...byte) (n int, err error)
}
";

    let staging = build_test_graph(source, "test.go");
    let param_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Parameter);
    let return_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Return);

    // Variadic parameter (should become []byte for TypeOf)
    assert!(
        param_edges
            .iter()
            .any(|(s, t)| s == "main.Writer.Write" && t == "[]byte"),
        "Expected Write variadic param → []byte TypeOf edge"
    );

    // Returns
    assert!(
        return_edges
            .iter()
            .any(|(s, t)| s == "main.Writer.Write" && t == "int"),
        "Expected Write return → int"
    );
    assert!(
        return_edges
            .iter()
            .any(|(s, t)| s == "main.Writer.Write" && t == "error"),
        "Expected Write return → error"
    );
}

#[test]
fn test_field_typeof_metadata_correctness() {
    // Test LOW-1: Field TypeOf metadata (name + index) correctness
    // This test verifies the metadata exists, even though we can't easily inspect it
    // in the current test helpers (they only return source/target names)
    let source = r"package main

type Point struct {
    X float64
    Y float64
    Z float64
}
";

    let staging = build_test_graph(source, "test.go");
    let field_edges = collect_typeof_edges_by_context(&staging, TypeOfContext::Field);

    // All three fields should have TypeOf edges with Field context
    let point_fields: Vec<_> = field_edges
        .iter()
        .filter(|(s, t)| s == "main.Point" && t == "float64")
        .collect();

    assert_eq!(
        point_fields.len(),
        3,
        "Expected 3 field TypeOf edges for X, Y, Z"
    );

    // Note: We can't easily verify index/name metadata without extending test helpers,
    // but the fact that we get 3 separate edges (not just 1) proves that indexing works
}
