//! Comprehensive tests for C `TypeOf` and Reference edge implementation.
//!
//! This test suite verifies that `TypeOf` and Reference edges are correctly created
//! for all C type constructs including variables, function parameters/returns,
//! struct/union fields, and typedef declarations.

use sqry_core::graph::unified::build::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_c::CPlugin;
use std::collections::HashMap;
use std::path::Path;

/// Helper function to build a graph from C code
fn build_graph_from_c_code(code: &str) -> StagingGraph {
    let plugin = CPlugin::default();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .expect("Failed to set language");

    let tree = parser.parse(code, None).expect("Failed to parse code");
    let mut staging = StagingGraph::new();

    plugin
        .graph_builder()
        .expect("Failed to get graph builder")
        .build_graph(&tree, code.as_bytes(), Path::new("test.c"), &mut staging)
        .expect("Failed to build graph");

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
                let name_idx = entry.qualified_name.unwrap_or(entry.name).index();
                let name = strings
                    .get(&name_idx)
                    .cloned()
                    .unwrap_or_else(|| format!("<string:{name_idx}>"));
                Some((node_idx, name))
            } else {
                None
            }
        })
        .collect()
}

/// Helper to check if a `TypeOf` edge exists
fn assert_typeof_edge_exists(
    graph: &StagingGraph,
    source_name: &str,
    target_type: &str,
    context: TypeOfContext,
) {
    let node_names = build_node_name_lookup(graph);

    let found = graph.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::TypeOf { context: ctx, .. },
            ..
        } = op
        {
            let from_name = node_names
                .get(&source.index())
                .map_or("", std::string::String::as_str);
            let to_name = node_names
                .get(&target.index())
                .map_or("", std::string::String::as_str);

            from_name.contains(source_name)
                && to_name.contains(target_type)
                && *ctx == Some(context)
        } else {
            false
        }
    });

    assert!(
        found,
        "Expected TypeOf edge from '{source_name}' to '{target_type}' with context {context:?}"
    );
}

/// Helper to check Reference edge exists
fn assert_reference_edge_exists(graph: &StagingGraph, source_name: &str, target_type: &str) {
    let node_names = build_node_name_lookup(graph);

    let found = graph.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            source,
            target,
            kind: EdgeKind::References,
            ..
        } = op
        {
            let from_name = node_names
                .get(&source.index())
                .map_or("", std::string::String::as_str);
            let to_name = node_names
                .get(&target.index())
                .map_or("", std::string::String::as_str);

            from_name.contains(source_name) && to_name.contains(target_type)
        } else {
            false
        }
    });

    assert!(
        found,
        "Expected Reference edge from '{source_name}' to '{target_type}'"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Variable TypeOf Edges (8 tests)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_typeof_variable_primitive_types() {
    let code = r"
        int count;
        char letter;
        float price;
        double precision;
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "count", "int", TypeOfContext::Variable);
    assert_typeof_edge_exists(&graph, "letter", "char", TypeOfContext::Variable);
    assert_typeof_edge_exists(&graph, "price", "float", TypeOfContext::Variable);
    assert_typeof_edge_exists(&graph, "precision", "double", TypeOfContext::Variable);
}

#[test]
fn test_typeof_variable_pointer_types() {
    let code = r"
        int* ptr;
        char** argv;
        void* generic;
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "ptr", "int", TypeOfContext::Variable);
    assert_typeof_edge_exists(&graph, "argv", "char", TypeOfContext::Variable);
    assert_typeof_edge_exists(&graph, "generic", "void", TypeOfContext::Variable);
}

#[test]
fn test_typeof_variable_array_types() {
    let code = r"
        int numbers[10];
        char buffer[256];
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "numbers", "int", TypeOfContext::Variable);
    assert_typeof_edge_exists(&graph, "buffer", "char", TypeOfContext::Variable);
}

#[test]
fn test_typeof_variable_struct_types() {
    let code = r"
        struct User {
            int id;
        };
        struct User user;
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "user", "User", TypeOfContext::Variable);
    assert_reference_edge_exists(&graph, "user", "User");
}

#[test]
fn test_typeof_variable_union_types() {
    let code = r"
        union Data {
            int i;
            float f;
        };
        union Data data;
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "data", "Data", TypeOfContext::Variable);
    assert_reference_edge_exists(&graph, "data", "Data");
}

#[test]
fn test_typeof_variable_enum_types() {
    let code = r"
        enum Status { PENDING, ACTIVE };
        enum Status status;
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "status", "Status", TypeOfContext::Variable);
    assert_reference_edge_exists(&graph, "status", "Status");
}

#[test]
fn test_typeof_variable_typedef_types() {
    let code = r"
        typedef int MyInt;
        MyInt value;
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "value", "MyInt", TypeOfContext::Variable);
    assert_reference_edge_exists(&graph, "value", "MyInt");
}

#[test]
fn test_typeof_variable_const_volatile() {
    let code = r"
        const int constant;
        volatile char flag;
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "constant", "int", TypeOfContext::Variable);
    assert_typeof_edge_exists(&graph, "flag", "char", TypeOfContext::Variable);
}

// ═══════════════════════════════════════════════════════════════════════════
// Function Parameter TypeOf Edges (8 tests)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_typeof_function_parameter_primitives() {
    let code = r"
        void process(int x, char c, float f) {
        }
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "process", "int", TypeOfContext::Parameter);
    assert_typeof_edge_exists(&graph, "process", "char", TypeOfContext::Parameter);
    assert_typeof_edge_exists(&graph, "process", "float", TypeOfContext::Parameter);
}

#[test]
fn test_typeof_function_parameter_pointers() {
    let code = r"
        void update(int* ptr, char** argv, void* data) {
        }
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "update", "int", TypeOfContext::Parameter);
    assert_typeof_edge_exists(&graph, "update", "char", TypeOfContext::Parameter);
    assert_typeof_edge_exists(&graph, "update", "void", TypeOfContext::Parameter);
}

#[test]
fn test_typeof_function_parameter_arrays() {
    let code = r"
        void fill(int arr[], char buffer[256]) {
        }
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "fill", "int", TypeOfContext::Parameter);
    assert_typeof_edge_exists(&graph, "fill", "char", TypeOfContext::Parameter);
}

#[test]
fn test_typeof_function_parameter_structs() {
    let code = r"
        struct User {
            int id;
        };
        void process_user(struct User user, struct User* ptr) {
        }
    ";

    let graph = build_graph_from_c_code(code);

    // Should have TypeOf edges for parameters
    assert_typeof_edge_exists(&graph, "process_user", "User", TypeOfContext::Parameter);

    // Should have Reference edges to User
    assert_reference_edge_exists(&graph, "process_user", "User");
}

#[test]
fn test_typeof_function_parameter_unnamed() {
    let code = r"
        void func(int, char*);
    ";

    let graph = build_graph_from_c_code(code);

    // Even unnamed parameters should have TypeOf edges
    assert_typeof_edge_exists(&graph, "func", "int", TypeOfContext::Parameter);
    assert_typeof_edge_exists(&graph, "func", "char", TypeOfContext::Parameter);
}

#[test]
fn test_typeof_function_parameter_const_qualified() {
    let code = r"
        void process(const int x, const char* str) {
        }
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "process", "int", TypeOfContext::Parameter);
    assert_typeof_edge_exists(&graph, "process", "char", TypeOfContext::Parameter);
}

#[test]
fn test_typeof_function_parameter_multiple_same_type() {
    let code = r"
        void compare(int a, int b, int c) {
        }
    ";

    let graph = build_graph_from_c_code(code);

    // Should have multiple TypeOf edges for the same type
    let count = graph
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::TypeOf {
                        context: Some(TypeOfContext::Parameter),
                        ..
                    },
                    ..
                }
            )
        })
        .count();

    assert!(
        count >= 3,
        "Expected at least 3 parameter TypeOf edges, got {count}"
    );
}

#[test]
fn test_typeof_function_parameter_complex_types() {
    let code = r"
        struct Point { int x; int y; };
        void draw(struct Point* points[], int count) {
        }
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "draw", "Point", TypeOfContext::Parameter);
    assert_typeof_edge_exists(&graph, "draw", "int", TypeOfContext::Parameter);
    assert_reference_edge_exists(&graph, "draw", "Point");
}

// ═══════════════════════════════════════════════════════════════════════════
// Function Return TypeOf Edges (6 tests)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_typeof_function_return_primitive() {
    let code = r"
        int calculate(void) {
            return 42;
        }
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "calculate", "int", TypeOfContext::Return);
}

#[test]
fn test_typeof_function_return_pointer() {
    let code = r"
        char* get_string(void) {
            return NULL;
        }
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "get_string", "char", TypeOfContext::Return);
}

#[test]
fn test_typeof_function_return_struct() {
    let code = r"
        struct User {
            int id;
        };
        struct User get_user(void) {
            struct User u;
            return u;
        }
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "get_user", "User", TypeOfContext::Return);
    assert_reference_edge_exists(&graph, "get_user", "User");
}

#[test]
fn test_typeof_function_return_struct_pointer() {
    let code = r"
        struct User {
            int id;
        };
        struct User* find_user(int id) {
            return NULL;
        }
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "find_user", "User", TypeOfContext::Return);
    assert_reference_edge_exists(&graph, "find_user", "User");
}

#[test]
fn test_typeof_function_return_typedef() {
    let code = r"
        typedef int Status;
        Status check_status(void) {
            return 0;
        }
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "check_status", "Status", TypeOfContext::Return);
}

#[test]
fn test_typeof_function_return_const_pointer() {
    let code = r#"
        const char* get_message(void) {
            return "Hello";
        }
    "#;

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "get_message", "char", TypeOfContext::Return);
}

// ═══════════════════════════════════════════════════════════════════════════
// Void Edge Cases (3 tests)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_typeof_void_return_no_edge() {
    let code = r"
        void process(int x) {
            // Pure void return - should NOT create TypeOf edge
        }
    ";

    let graph = build_graph_from_c_code(code);

    // Verify NO return TypeOf edge for void
    let has_return_edge = graph.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            kind: EdgeKind::TypeOf { context, .. },
            ..
        } = op
        {
            matches!(context, Some(TypeOfContext::Return))
        } else {
            false
        }
    });

    assert!(
        !has_return_edge,
        "void return should not create TypeOf edge"
    );
}

#[test]
fn test_typeof_void_param_no_edge() {
    let code = r"
        int calculate(void) {
            return 42;
        }
    ";

    let graph = build_graph_from_c_code(code);

    // Verify NO parameter TypeOf edge for f(void)
    let has_param_edge = graph.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            kind: EdgeKind::TypeOf { context, .. },
            ..
        } = op
        {
            matches!(context, Some(TypeOfContext::Parameter))
        } else {
            false
        }
    });

    assert!(
        !has_param_edge,
        "f(void) should not create parameter TypeOf edge"
    );
}

#[test]
fn test_typeof_void_pointer_return() {
    let code = r"
        void* allocate(size_t size) {
            return 0;
        }
    ";

    let graph = build_graph_from_c_code(code);

    // void* should create a TypeOf edge (it's not pure void)
    assert_typeof_edge_exists(&graph, "allocate", "void", TypeOfContext::Return);
}

// ═══════════════════════════════════════════════════════════════════════════
// Struct Field TypeOf Edges (6 tests)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_typeof_struct_field_primitives() {
    let code = r"
        struct Point {
            int x;
            int y;
            float z;
        };
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "Point", "int", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "Point", "float", TypeOfContext::Field);
}

#[test]
fn test_typeof_struct_field_pointers() {
    let code = r"
        struct User {
            char* name;
            int* scores;
        };
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "User", "char", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "User", "int", TypeOfContext::Field);
}

#[test]
fn test_typeof_struct_field_arrays() {
    let code = r"
        struct Buffer {
            char data[256];
            int numbers[10];
        };
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "Buffer", "char", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "Buffer", "int", TypeOfContext::Field);
}

#[test]
fn test_typeof_struct_field_nested_struct() {
    let code = r"
        struct Address {
            int zip;
        };
        struct User {
            struct Address addr;
            struct Address* addr_ptr;
        };
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "User", "Address", TypeOfContext::Field);
    assert_reference_edge_exists(&graph, "User", "Address");
}

#[test]
fn test_typeof_struct_field_mixed_types() {
    let code = r"
        struct Record {
            int id;
            char* name;
            float value;
            double precision;
        };
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "Record", "int", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "Record", "char", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "Record", "float", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "Record", "double", TypeOfContext::Field);
}

#[test]
fn test_typeof_struct_field_typedef() {
    let code = r"
        typedef int UserId;
        struct User {
            UserId id;
            char* name;
        };
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "User", "UserId", TypeOfContext::Field);
    assert_reference_edge_exists(&graph, "User", "UserId");
}

// ═══════════════════════════════════════════════════════════════════════════
// Union Field TypeOf Edges (4 tests)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_typeof_union_field_primitives() {
    let code = r"
        union Data {
            int i;
            float f;
            char c;
        };
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "Data", "int", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "Data", "float", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "Data", "char", TypeOfContext::Field);
}

#[test]
fn test_typeof_union_field_pointers() {
    let code = r"
        union Pointer {
            int* int_ptr;
            char* char_ptr;
            void* generic_ptr;
        };
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "Pointer", "int", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "Pointer", "char", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "Pointer", "void", TypeOfContext::Field);
}

#[test]
fn test_typeof_union_field_structs() {
    let code = r"
        struct A { int x; };
        struct B { float y; };
        union Either {
            struct A a;
            struct B b;
        };
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "Either", "A", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "Either", "B", TypeOfContext::Field);
    assert_reference_edge_exists(&graph, "Either", "A");
    assert_reference_edge_exists(&graph, "Either", "B");
}

#[test]
fn test_typeof_union_field_mixed() {
    let code = r"
        union Value {
            int integer;
            float floating;
            char* string;
        };
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "Value", "int", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "Value", "float", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "Value", "char", TypeOfContext::Field);
}

// ═══════════════════════════════════════════════════════════════════════════
// Typedef TypeOf Edges (4 tests)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_typeof_typedef_simple() {
    let code = r"
        typedef int MyInt;
        typedef char* String;
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "MyInt", "int", TypeOfContext::Variable);
    assert_typeof_edge_exists(&graph, "String", "char", TypeOfContext::Variable);
}

#[test]
fn test_typeof_typedef_struct() {
    let code = r"
        typedef struct User {
            int id;
        } User;
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "User", "User", TypeOfContext::Variable);
}

#[test]
fn test_typeof_typedef_pointer() {
    let code = r"
        typedef int* IntPtr;
        typedef char** StringArray;
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "IntPtr", "int", TypeOfContext::Variable);
    assert_typeof_edge_exists(&graph, "StringArray", "char", TypeOfContext::Variable);
}

#[test]
fn test_typeof_typedef_array() {
    let code = r"
        typedef int IntArray[10];
        typedef char CharBuffer[256];
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "IntArray", "int", TypeOfContext::Variable);
    assert_typeof_edge_exists(&graph, "CharBuffer", "char", TypeOfContext::Variable);
}

// ═══════════════════════════════════════════════════════════════════════════
// Complex Type Tests (8+ tests)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_typeof_complex_multidimensional_array() {
    let code = r"
        int matrix[5][10];
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "matrix", "int", TypeOfContext::Variable);
}

#[test]
fn test_typeof_complex_array_of_pointers() {
    let code = r"
        int* array_of_ptrs[10];
        char** argv;
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "array_of_ptrs", "int", TypeOfContext::Variable);
    assert_typeof_edge_exists(&graph, "argv", "char", TypeOfContext::Variable);
}

#[test]
fn test_typeof_complex_pointer_to_array() {
    let code = r"
        int (*ptr_to_array)[10];
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "ptr_to_array", "int", TypeOfContext::Variable);
}

#[test]
fn test_typeof_complex_function_returning_pointer() {
    let code = r"
        int* get_number(void);
        struct User { int id; };
        struct User* find_user(int id);
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "get_number", "int", TypeOfContext::Return);
    assert_typeof_edge_exists(&graph, "find_user", "User", TypeOfContext::Return);
}

#[test]
fn test_typeof_complex_const_pointer_combinations() {
    let code = r"
        const int* ptr1;
        int* const ptr2;
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "ptr1", "int", TypeOfContext::Variable);
    assert_typeof_edge_exists(&graph, "ptr2", "int", TypeOfContext::Variable);
}

#[test]
fn test_typeof_reference_edges_struct_usage() {
    let code = r"
        struct User {
            int id;
        };
        struct Post {
            struct User* author;
            struct User* reviewer;
        };
    ";

    let graph = build_graph_from_c_code(code);

    // Post should have multiple references to User
    assert_reference_edge_exists(&graph, "Post", "User");
}

#[test]
fn test_typeof_reference_edges_typedef_usage() {
    let code = r"
        typedef int MyInt;
        typedef MyInt MySpecialInt;
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "MyInt", "int", TypeOfContext::Variable);
    assert_reference_edge_exists(&graph, "MyInt", "int");
}

#[test]
fn test_typeof_complex_nested_structs() {
    let code = r"
        struct Inner {
            int value;
        };
        struct Outer {
            struct Inner inner;
            struct Inner* ptr;
        };
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "Outer", "Inner", TypeOfContext::Field);
    assert_reference_edge_exists(&graph, "Outer", "Inner");
}

#[test]
fn test_typeof_complex_function_with_struct_params_and_return() {
    let code = r"
        struct Point { int x; int y; };
        struct Point add_points(struct Point p1, struct Point p2) {
            struct Point result;
            return result;
        }
    ";

    let graph = build_graph_from_c_code(code);

    assert_typeof_edge_exists(&graph, "add_points", "Point", TypeOfContext::Parameter);
    assert_typeof_edge_exists(&graph, "add_points", "Point", TypeOfContext::Return);
    assert_reference_edge_exists(&graph, "add_points", "Point");
}

#[test]
fn test_typeof_complex_mixed_declarations() {
    let code = r"
        typedef int UserId;
        struct User {
            UserId id;
            char* name;
        };
        struct User create_user(UserId id, const char* name);
    ";

    let graph = build_graph_from_c_code(code);

    // Struct field edges
    assert_typeof_edge_exists(&graph, "User", "UserId", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "User", "char", TypeOfContext::Field);

    // Function parameter edges
    assert_typeof_edge_exists(&graph, "create_user", "UserId", TypeOfContext::Parameter);
    assert_typeof_edge_exists(&graph, "create_user", "char", TypeOfContext::Parameter);

    // Function return edge
    assert_typeof_edge_exists(&graph, "create_user", "User", TypeOfContext::Return);

    // Reference edges
    assert_reference_edge_exists(&graph, "User", "UserId");
    assert_reference_edge_exists(&graph, "create_user", "UserId");
    assert_reference_edge_exists(&graph, "create_user", "User");
}

#[test]
fn test_typeof_complex_function_pointers_in_struct() {
    let code = r"
        struct Callbacks {
            int (*on_init)(void);
            void (*on_update)(int);
        };
    ";

    let graph = build_graph_from_c_code(code);

    // Should have field edges for the function pointer return/parameter types
    assert_typeof_edge_exists(&graph, "Callbacks", "int", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "Callbacks", "void", TypeOfContext::Field);
}

#[test]
fn test_typeof_comprehensive_coverage() {
    let code = r"
        // Primitive types
        int global_int;

        // Typedef
        typedef int MyInt;
        MyInt typed_value;

        // Struct with fields
        struct Data {
            int id;
            char* name;
        };

        // Union with fields
        union Value {
            int i;
            float f;
        };

        // Function with parameters and return
        struct Data* process(int id, const char* name, struct Data* input) {
            return NULL;
        }
    ";

    let graph = build_graph_from_c_code(code);

    // Variable edges
    assert_typeof_edge_exists(&graph, "global_int", "int", TypeOfContext::Variable);
    assert_typeof_edge_exists(&graph, "typed_value", "MyInt", TypeOfContext::Variable);

    // Typedef edges
    assert_typeof_edge_exists(&graph, "MyInt", "int", TypeOfContext::Variable);

    // Struct field edges
    assert_typeof_edge_exists(&graph, "Data", "int", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "Data", "char", TypeOfContext::Field);

    // Union field edges
    assert_typeof_edge_exists(&graph, "Value", "int", TypeOfContext::Field);
    assert_typeof_edge_exists(&graph, "Value", "float", TypeOfContext::Field);

    // Function parameter edges
    assert_typeof_edge_exists(&graph, "process", "int", TypeOfContext::Parameter);
    assert_typeof_edge_exists(&graph, "process", "char", TypeOfContext::Parameter);
    assert_typeof_edge_exists(&graph, "process", "Data", TypeOfContext::Parameter);

    // Function return edge
    assert_typeof_edge_exists(&graph, "process", "Data", TypeOfContext::Return);

    // Reference edges
    assert_reference_edge_exists(&graph, "process", "Data");
}
