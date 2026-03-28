//! C++ GraphBuilder tests for import, inheritance, and export edge extraction.
//!
//! Tests the unified graph builder implementation for:
//! - Import edges (#include directives)
//! - Inherits edges (class/struct inheritance)
//! - Export edges (public symbols with external linkage)

use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::{GraphBuilder, Language, unified::StagingGraph};
use sqry_lang_cpp::relations::CppGraphBuilder;
use std::path::Path;
use tree_sitter::Parser;

fn parse_cpp(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .expect("Failed to set C++ language");
    parser
        .parse(source.as_bytes(), None)
        .expect("Failed to parse C++ source")
}

use sqry_core::graph::unified::{NodeEntry, NodeId, StringId};
use std::collections::HashMap;

/// Helper to count import edges in staged operations
fn count_import_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Imports { .. },
                    ..
                }
            )
        })
        .count()
}

/// Helper to count inherits edges in staged operations
fn count_inherits_edges(staging: &StagingGraph) -> usize {
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

/// Helper to count export edges in staged operations
fn count_export_edges(staging: &StagingGraph) -> usize {
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

/// Helper to verify an export edge exists for a symbol with specific name.
fn has_export_edge_for(staging: &StagingGraph, symbol_pattern: &str) -> bool {
    let string_map = build_string_map(staging);

    // Build NodeId -> name map from AddNode operations
    let mut node_names: HashMap<NodeId, String> = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, expected_id } = op
            && let Some(expected_id) = expected_id
            && let Some(name) = get_node_name(entry, &string_map)
        {
            node_names.insert(*expected_id, name);
        }
    }

    // Check if any export edge targets a node with matching name
    staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            kind: EdgeKind::Exports { .. },
            target,
            ..
        } = op
        {
            if let Some(name) = node_names.get(target) {
                // Match by simple name (last component of qualified name)
                name.ends_with(symbol_pattern) || name == symbol_pattern
            } else {
                false
            }
        } else {
            false
        }
    })
}

/// Build a map from StringId to string value from staging operations.
fn build_string_map(staging: &StagingGraph) -> HashMap<StringId, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::InternString { local_id, value } = op {
                Some((*local_id, value.clone()))
            } else {
                None
            }
        })
        .collect()
}

/// Get node name from NodeEntry using string map.
fn get_node_name(entry: &NodeEntry, string_map: &HashMap<StringId, String>) -> Option<String> {
    string_map.get(&entry.name).cloned()
}

/// Get graph-structural node name from `NodeEntry`, preferring canonical qualified identity.
fn get_node_structural_name(
    entry: &NodeEntry,
    string_map: &HashMap<StringId, String>,
) -> Option<String> {
    entry
        .qualified_name
        .and_then(|qualified_name_id| string_map.get(&qualified_name_id).cloned())
        .or_else(|| get_node_name(entry, string_map))
}

/// Helper to verify an import edge exists with specific target name.
fn has_import_edge_to(staging: &StagingGraph, target_pattern: &str) -> bool {
    let string_map = build_string_map(staging);

    // Build NodeId -> name map from AddNode operations
    let mut node_names: HashMap<NodeId, String> = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, expected_id } = op
            && let Some(expected_id) = expected_id
            && let Some(name) = get_node_name(entry, &string_map)
        {
            node_names.insert(*expected_id, name);
        }
    }

    // Check if any import edge targets a node with matching name
    staging.operations().iter().any(|op| {
        if let StagingOp::AddEdge {
            kind: EdgeKind::Imports { .. },
            target,
            ..
        } = op
        {
            node_names
                .get(target)
                .map(|name| name.contains(target_pattern))
                .unwrap_or(false)
        } else {
            false
        }
    })
}

// ==================== Basic Tests ====================

#[test]
fn test_builder_language() {
    let builder = CppGraphBuilder::new();
    assert_eq!(builder.language(), Language::Cpp);
}

#[test]
fn test_empty_file() {
    let content = "";
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("empty.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed for empty file");

    let stats = staging.stats();
    assert_eq!(stats.nodes_staged, 0, "Empty file should have no nodes");
    assert_eq!(stats.edges_staged, 0, "Empty file should have no edges");
}

// ==================== Import Edge Tests ====================

#[test]
fn test_system_headers_import() {
    let content = include_str!("fixtures/imports/system_headers.cpp");
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("tests/fixtures/imports/system_headers.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for system headers"
    );

    // Verify specific import targets
    assert!(
        has_import_edge_to(&staging, "iostream"),
        "expected import edge to iostream"
    );
    assert!(
        has_import_edge_to(&staging, "vector"),
        "expected import edge to vector"
    );
    assert!(
        has_import_edge_to(&staging, "string"),
        "expected import edge to string"
    );
    assert!(
        has_import_edge_to(&staging, "map"),
        "expected import edge to map"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 4,
        "Should have 4 import edges for system headers (iostream, vector, string, map)"
    );
}

#[test]
fn test_local_headers_import() {
    let content = include_str!("fixtures/imports/local_headers.cpp");
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("tests/fixtures/imports/local_headers.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for local headers"
    );

    // Verify specific import targets
    assert!(
        has_import_edge_to(&staging, "myclass.hpp"),
        "expected import edge to myclass.hpp"
    );
    assert!(
        has_import_edge_to(&staging, "utils/helper.hpp"),
        "expected import edge to utils/helper.hpp"
    );
    assert!(
        has_import_edge_to(&staging, "config.h"),
        "expected import edge to config.h"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 3,
        "Should have 3 import edges for local headers (myclass.hpp, utils/helper.hpp, config.h)"
    );
}

#[test]
fn test_mixed_includes_import() {
    let content = include_str!("fixtures/imports/mixed_includes.cpp");
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("tests/fixtures/imports/mixed_includes.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for mixed includes"
    );

    // Verify system headers
    assert!(
        has_import_edge_to(&staging, "iostream"),
        "expected import edge to iostream"
    );
    assert!(
        has_import_edge_to(&staging, "vector"),
        "expected import edge to vector"
    );
    assert!(
        has_import_edge_to(&staging, "memory"),
        "expected import edge to memory"
    );

    // Verify local headers
    assert!(
        has_import_edge_to(&staging, "database.hpp"),
        "expected import edge to database.hpp"
    );
    assert!(
        has_import_edge_to(&staging, "api/endpoints.hpp"),
        "expected import edge to api/endpoints.hpp"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 5,
        "Should have 5 import edges (iostream, vector, database.hpp, api/endpoints.hpp, memory)"
    );
}

#[test]
fn test_import_deduplication() {
    // Test that duplicate includes are deduplicated
    let content = r#"
#include <iostream>
#include <iostream>
#include <iostream>
#include "local.hpp"
#include "local.hpp"

void test() {}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test_dedup.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify specific imports are present
    assert!(
        has_import_edge_to(&staging, "iostream"),
        "expected import edge to iostream"
    );
    assert!(
        has_import_edge_to(&staging, "local.hpp"),
        "expected import edge to local.hpp"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 2,
        "Should have 2 import edges (iostream and local.hpp, duplicates removed)"
    );
}

#[test]
fn test_import_edge_structure() {
    let content = r#"
#include <iostream>
void test() {}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test_structure.cpp");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .unwrap();

    // Verify specific target
    assert!(
        has_import_edge_to(&staging, "iostream"),
        "expected import edge to iostream"
    );

    // Find the import edge
    let import_edge = staging.operations().iter().find(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::Imports { .. },
                ..
            }
        )
    });

    assert!(import_edge.is_some(), "Should have an import edge");

    // Verify import edge has correct structure (no alias, not wildcard)
    if let StagingOp::AddEdge {
        kind: EdgeKind::Imports { alias, is_wildcard },
        ..
    } = import_edge.unwrap()
    {
        assert!(
            alias.is_none(),
            "C++ includes should not have alias (alias is for import renaming)"
        );
        assert!(!*is_wildcard, "C++ includes are not wildcard imports");
    } else {
        panic!("Expected Imports edge");
    }
}

#[test]
fn test_nested_path_import() {
    let content = r#"
#include <sys/types.h>
#include <sys/socket.h>
#include "utils/helper.hpp"
#include "api/v1/endpoints.hpp"

void test() {}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test_nested.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for nested paths"
    );

    // Verify specific nested path imports
    assert!(
        has_import_edge_to(&staging, "sys/types.h"),
        "expected import edge to sys/types.h"
    );
    assert!(
        has_import_edge_to(&staging, "sys/socket.h"),
        "expected import edge to sys/socket.h"
    );
    assert!(
        has_import_edge_to(&staging, "utils/helper.hpp"),
        "expected import edge to utils/helper.hpp"
    );
    assert!(
        has_import_edge_to(&staging, "api/v1/endpoints.hpp"),
        "expected import edge to api/v1/endpoints.hpp"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 4,
        "Should have 4 import edges for nested path includes"
    );
}

#[test]
fn test_no_imports_code_only() {
    let content = r#"
class Foo {
public:
    void bar() {}
};

int main() {
    Foo f;
    f.bar();
    return 0;
}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("no_imports.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 0,
        "File without includes should have no import edges"
    );
}

#[test]
fn test_import_with_comments() {
    let content = r#"
// System includes
#include <iostream>  // Standard I/O
/* Multi-line comment
   before include */
#include <vector>
#include "local.hpp"  /* Local header */

void test() {}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test_comments.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed with comments");

    // Verify specific imports are present despite comments
    assert!(
        has_import_edge_to(&staging, "iostream"),
        "expected import edge to iostream"
    );
    assert!(
        has_import_edge_to(&staging, "vector"),
        "expected import edge to vector"
    );
    assert!(
        has_import_edge_to(&staging, "local.hpp"),
        "expected import edge to local.hpp"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 3,
        "Should have 3 import edges (comments don't affect parsing)"
    );
}

// ==================== Inheritance Edge Tests ====================

#[test]
fn test_single_class_inheritance() {
    let content = include_str!("fixtures/inheritance/single_inheritance.cpp");
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("tests/fixtures/inheritance/single_inheritance.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for single inheritance"
    );

    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 1,
        "Should have 1 inherits edge (Derived : Base)"
    );
}

#[test]
fn test_multiple_class_inheritance() {
    let content = include_str!("fixtures/inheritance/multiple_inheritance.cpp");
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("tests/fixtures/inheritance/multiple_inheritance.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for multiple inheritance"
    );

    // InterfaceA and InterfaceB are pure virtual interfaces (have = 0 methods)
    // So Implementation gets Implements edges, not Inherits edges
    let implements_count = count_implements_edges(&staging);
    assert_eq!(
        implements_count, 2,
        "Should have 2 implements edges (Implementation implements InterfaceA, InterfaceB)"
    );
}

#[test]
fn test_template_class_inheritance() {
    let content = include_str!("fixtures/inheritance/template_inheritance.cpp");
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("tests/fixtures/inheritance/template_inheritance.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for template inheritance"
    );

    // Container<T> is a pure virtual interface (has = 0 methods)
    // So ArrayContainer<T> and IntList get Implements edges
    let implements_count = count_implements_edges(&staging);
    assert_eq!(
        implements_count, 2,
        "Should have 2 implements edges (ArrayContainer implements Container, IntList implements Container)"
    );
}

#[test]
fn test_struct_inheritance() {
    let content = include_str!("fixtures/inheritance/struct_inheritance.cpp");
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("tests/fixtures/inheritance/struct_inheritance.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for struct inheritance"
    );

    let inherits_count = count_inherits_edges(&staging);
    // DerivedStruct : BaseStruct and StructFromClass : BaseClass
    assert_eq!(
        inherits_count, 2,
        "Should have 2 inherits edges for struct inheritance"
    );
}

#[test]
fn test_simple_class_inheritance() {
    let content = r#"
class Parent {
public:
    virtual void method() {}
};

class Child : public Parent {
public:
    void method() override {}
};
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("simple_inheritance.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 1,
        "Should have 1 inherits edge (Child : Parent)"
    );
}

#[test]
fn test_private_inheritance() {
    let content = r#"
class Base {
public:
    void publicMethod() {}
};

class DerivedPrivate : private Base {
    // All Base members become private
};

class DerivedProtected : protected Base {
    // All public Base members become protected
};
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("private_inheritance.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let inherits_count = count_inherits_edges(&staging);
    // Both private and protected inheritance should be detected
    assert_eq!(
        inherits_count, 2,
        "Should have 2 inherits edges (DerivedPrivate : Base, DerivedProtected : Base)"
    );
}

#[test]
fn test_diamond_inheritance() {
    let content = r#"
class Base {
public:
    virtual void method() {}
};

class Left : public Base {
public:
    void leftMethod() {}
};

class Right : public Base {
public:
    void rightMethod() {}
};

class Diamond : public Left, public Right {
public:
    void diamondMethod() {}
};
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("diamond_inheritance.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let inherits_count = count_inherits_edges(&staging);
    // Left : Base, Right : Base, Diamond : Left, Diamond : Right
    assert_eq!(
        inherits_count, 4,
        "Should have 4 inherits edges for diamond inheritance"
    );
}

#[test]
fn test_no_inheritance() {
    let content = r#"
class Standalone {
public:
    void method() {}
};

struct PlainStruct {
    int value;
};
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("no_inheritance.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 0,
        "Should have 0 inherits edges for standalone classes"
    );
}

// ==================== Combined Tests ====================

#[test]
fn test_imports_and_inheritance_together() {
    let content = r#"
#include <iostream>
#include <memory>

class Base {
public:
    virtual void process() = 0;
    virtual ~Base() = default;
};

class Derived : public Base {
public:
    void process() override {
        std::cout << "Processing" << std::endl;
    }
};
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("combined.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify specific import targets
    assert!(
        has_import_edge_to(&staging, "iostream"),
        "expected import edge to iostream"
    );
    assert!(
        has_import_edge_to(&staging, "memory"),
        "expected import edge to memory"
    );

    let import_count = count_import_edges(&staging);
    // Base is a pure virtual interface (has = 0 method)
    // So Derived gets an Implements edge, not Inherits
    let implements_count = count_implements_edges(&staging);

    assert_eq!(import_count, 2, "Should have 2 import edges");
    assert_eq!(
        implements_count, 1,
        "Should have 1 implements edge (Derived implements Base)"
    );
}

#[test]
fn test_namespace_with_inheritance() {
    let content = r#"
namespace shapes {
    class Shape {
    public:
        virtual double area() = 0;
    };

    class Circle : public Shape {
    public:
        double area() override { return 3.14; }
    };

    class Rectangle : public Shape {
    public:
        double area() override { return 0.0; }
    };
}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("namespace_inheritance.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Shape is a pure virtual interface (has = 0 method)
    // So Circle and Rectangle get Implements edges
    let implements_count = count_implements_edges(&staging);
    assert_eq!(
        implements_count, 2,
        "Should have 2 implements edges (Circle implements Shape, Rectangle implements Shape)"
    );
}

#[test]
fn test_virtual_inheritance() {
    let content = r#"
class Base {
public:
    int value;
};

class Left : virtual public Base {
public:
    void leftMethod() {}
};

class Right : virtual public Base {
public:
    void rightMethod() {}
};

class Diamond : public Left, public Right {
public:
    void use() {
        value = 42; // Unambiguous due to virtual inheritance
    }
};
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("virtual_inheritance.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let inherits_count = count_inherits_edges(&staging);
    // Left : Base, Right : Base, Diamond : Left, Diamond : Right
    assert_eq!(
        inherits_count, 4,
        "Should have 4 inherits edges for virtual inheritance diamond"
    );
}

#[test]
fn test_class_nodes_created() {
    let content = r#"
class Base {};
class Derived : public Base {};
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test_nodes.cpp");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    // Should have at least 2 class nodes and 1 inherits edge
    assert!(
        stats.nodes_staged >= 2,
        "Should have at least 2 class nodes. Got: {}",
        stats.nodes_staged
    );
    assert!(
        stats.edges_staged >= 1,
        "Should have at least 1 edge. Got: {}",
        stats.edges_staged
    );
}

// ==================== FFI Edge Tests ====================

/// Helper to count FfiCall edges in staged operations
fn count_ffi_edges(staging: &StagingGraph) -> usize {
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

/// Helper to check if any function node exists with a given name pattern
fn has_function_node_matching(staging: &StagingGraph, pattern: &str) -> bool {
    let string_map = build_string_map(staging);

    staging.operations().iter().any(|op| {
        if let StagingOp::AddNode { entry, .. } = op
            && let Some(name) = get_node_structural_name(entry, &string_map)
        {
            name.contains(pattern)
        } else {
            false
        }
    })
}

#[test]
fn test_extern_c_block_declarations() {
    let content = r#"
extern "C" {
    void printf(const char* format, ...);
    int strlen(const char* str);
}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("ffi_block.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for extern C block"
    );

    // Verify FFI function nodes are created
    assert!(
        has_function_node_matching(&staging, "extern::C::printf"),
        "expected FFI function node for printf"
    );
    assert!(
        has_function_node_matching(&staging, "extern::C::strlen"),
        "expected FFI function node for strlen"
    );
}

#[test]
fn test_extern_c_single_declaration() {
    let content = r#"
extern "C" void my_c_function(int x);
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("ffi_single.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for single extern C"
    );

    // Verify FFI function node is created
    assert!(
        has_function_node_matching(&staging, "extern::C::my_c_function"),
        "expected FFI function node for my_c_function"
    );
}

#[test]
fn test_ffi_call_edge_created() {
    let content = r#"
extern "C" {
    void c_function();
}

void caller() {
    c_function();
}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("ffi_call.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed for FFI call");

    // Verify FfiCall edge is created
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Should have 1 FfiCall edge for c_function() call"
    );
}

#[test]
fn test_multiple_ffi_calls() {
    let content = r#"
extern "C" {
    void func_a();
    void func_b();
}

void caller() {
    func_a();
    func_b();
}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("multi_ffi.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 2,
        "Should have 2 FfiCall edges for func_a() and func_b()"
    );
}

#[test]
fn test_extern_c_no_ffi_for_qualified_calls() {
    let content = r#"
extern "C" {
    void foo();
}

namespace other {
    void foo() {}
}

void caller() {
    other::foo();  // This should NOT be an FfiCall
}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("qualified_call.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Qualified calls should NOT create FfiCall edges
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Qualified calls (other::foo) should not create FfiCall edges"
    );
}

// ==================== Implements Edge Tests ====================

/// Helper to count implements edges in staged operations
fn count_implements_edges(staging: &StagingGraph) -> usize {
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
fn test_pure_virtual_interface_implements() {
    let content = r#"
class IShape {
public:
    virtual double area() = 0;
    virtual double perimeter() = 0;
};

class Circle : public IShape {
public:
    double area() override { return 3.14; }
    double perimeter() override { return 6.28; }
};
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("interface.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for interface pattern"
    );

    // Circle should have an Implements edge to IShape
    let implements_count = count_implements_edges(&staging);
    assert_eq!(
        implements_count, 1,
        "Should have 1 Implements edge (Circle implements IShape)"
    );
}

#[test]
fn test_multiple_pure_virtual_interfaces() {
    let content = r#"
class IReadable {
public:
    virtual void read() = 0;
};

class IWritable {
public:
    virtual void write() = 0;
};

class File : public IReadable, public IWritable {
public:
    void read() override {}
    void write() override {}
};
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("multi_interface.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // File should have Implements edges to both IReadable and IWritable
    let implements_count = count_implements_edges(&staging);
    assert_eq!(
        implements_count, 2,
        "Should have 2 Implements edges (File implements IReadable and IWritable)"
    );
}

#[test]
fn test_mixed_inheritance_and_implements() {
    let content = r#"
class IPlugin {
public:
    virtual void execute() = 0;
};

class BasePlugin {
public:
    void log() {}
};

class MyPlugin : public BasePlugin, public IPlugin {
public:
    void execute() override {}
};
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("mixed.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // MyPlugin should have:
    // - 1 Inherits edge to BasePlugin
    // - 1 Implements edge to IPlugin
    let implements_count = count_implements_edges(&staging);
    let inherits_count = count_inherits_edges(&staging);

    assert_eq!(
        implements_count, 1,
        "Should have 1 Implements edge (MyPlugin implements IPlugin)"
    );
    assert_eq!(
        inherits_count, 1,
        "Should have 1 Inherits edge (MyPlugin extends BasePlugin)"
    );
}

#[test]
fn test_non_pure_virtual_not_interface() {
    let content = r#"
class Base {
public:
    virtual void method() {}  // Virtual but NOT pure (has implementation)
};

class Derived : public Base {
public:
    void method() override {}
};
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("non_interface.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Base is NOT a pure virtual interface (has implementation)
    // So Derived should have Inherits edge, NOT Implements
    let implements_count = count_implements_edges(&staging);
    let inherits_count = count_inherits_edges(&staging);

    assert_eq!(
        implements_count, 0,
        "Should have 0 Implements edges (Base is not a pure interface)"
    );
    assert_eq!(
        inherits_count, 1,
        "Should have 1 Inherits edge (Derived extends Base)"
    );
}

#[test]
fn test_struct_implements_interface() {
    let content = r#"
class ISerializable {
public:
    virtual void serialize() = 0;
};

struct Data : public ISerializable {
    int value;
    void serialize() override {}
};
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("struct_implements.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Struct can also implement interfaces
    let implements_count = count_implements_edges(&staging);
    assert_eq!(
        implements_count, 1,
        "Should have 1 Implements edge (struct Data implements ISerializable)"
    );
}

// ==================== Combined FFI + OOP Tests ====================

#[test]
fn test_ffi_and_inheritance_together() {
    let content = r#"
extern "C" {
    void native_init();
}

class IProcessor {
public:
    virtual void process() = 0;
};

class NativeProcessor : public IProcessor {
public:
    void process() override {
        native_init();
    }
};
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("combined.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify all edge types are created
    let ffi_count = count_ffi_edges(&staging);
    let implements_count = count_implements_edges(&staging);

    assert_eq!(ffi_count, 1, "Should have 1 FfiCall edge for native_init()");
    assert_eq!(
        implements_count, 1,
        "Should have 1 Implements edge (NativeProcessor implements IProcessor)"
    );
}

// ==================== Additional Import Edge Tests ====================

#[test]
fn test_cpp_specific_headers() {
    // Test C++ standard library headers (no .h extension)
    let content = r#"
#include <iostream>
#include <algorithm>
#include <functional>
#include <optional>
#include <variant>

void test() {}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test_cpp_headers.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for C++ standard headers"
    );

    // Verify specific C++ headers
    assert!(
        has_import_edge_to(&staging, "iostream"),
        "expected import edge to iostream"
    );
    assert!(
        has_import_edge_to(&staging, "algorithm"),
        "expected import edge to algorithm"
    );
    assert!(
        has_import_edge_to(&staging, "functional"),
        "expected import edge to functional"
    );
    assert!(
        has_import_edge_to(&staging, "optional"),
        "expected import edge to optional"
    );
    assert!(
        has_import_edge_to(&staging, "variant"),
        "expected import edge to variant"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 5,
        "Should have 5 import edges for C++ standard headers"
    );
}

#[test]
fn test_c_compat_headers() {
    // Test C compatibility headers (c-prefix versions)
    let content = r#"
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>

void test() {}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test_c_compat.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for C compatibility headers"
    );

    // Verify C compatibility headers
    assert!(
        has_import_edge_to(&staging, "cstdio"),
        "expected import edge to cstdio"
    );
    assert!(
        has_import_edge_to(&staging, "cstdlib"),
        "expected import edge to cstdlib"
    );
    assert!(
        has_import_edge_to(&staging, "cstring"),
        "expected import edge to cstring"
    );
    assert!(
        has_import_edge_to(&staging, "cmath"),
        "expected import edge to cmath"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 4,
        "Should have 4 import edges for C compatibility headers"
    );
}

#[test]
fn test_hpp_and_h_extensions() {
    // Test both .hpp and .h extensions for local headers
    let content = r#"
#include "modern.hpp"
#include "legacy.h"
#include "interface.hxx"
#include "template.tpp"

void test() {}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test_extensions.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for various header extensions"
    );

    // Verify different extensions
    assert!(
        has_import_edge_to(&staging, "modern.hpp"),
        "expected import edge to modern.hpp"
    );
    assert!(
        has_import_edge_to(&staging, "legacy.h"),
        "expected import edge to legacy.h"
    );
    assert!(
        has_import_edge_to(&staging, "interface.hxx"),
        "expected import edge to interface.hxx"
    );
    assert!(
        has_import_edge_to(&staging, "template.tpp"),
        "expected import edge to template.tpp"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 4,
        "Should have 4 import edges for various header extensions"
    );
}

#[test]
fn test_deeply_nested_includes() {
    // Test deeply nested directory paths
    let content = r#"
#include "src/core/utils/string/helper.hpp"
#include "third_party/boost/optional/optional.hpp"
#include <boost/asio/io_context.hpp>

void test() {}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test_deep_paths.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for deeply nested paths"
    );

    // Verify deeply nested paths
    assert!(
        has_import_edge_to(&staging, "src/core/utils/string/helper.hpp"),
        "expected import edge to src/core/utils/string/helper.hpp"
    );
    assert!(
        has_import_edge_to(&staging, "third_party/boost/optional/optional.hpp"),
        "expected import edge to third_party/boost/optional/optional.hpp"
    );
    assert!(
        has_import_edge_to(&staging, "boost/asio/io_context.hpp"),
        "expected import edge to boost/asio/io_context.hpp"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 3,
        "Should have 3 import edges for deeply nested paths"
    );
}

#[test]
fn test_import_nodes_created() {
    // Verify that import nodes are properly created for the included headers
    let content = r#"
#include <vector>
#include "myclass.hpp"

void test() {}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test_import_nodes.cpp");

    builder
        .build_graph(&tree, content.as_bytes(), file, &mut staging)
        .unwrap();

    let stats = staging.stats();

    // Should have at least:
    // - 1 module node (for <file>) - may be shared/reused
    // - 2 import nodes (for vector and myclass.hpp)
    // - 1 function node (for test)
    // Note: Module node may be reused, so minimum is 3 nodes
    assert!(
        stats.nodes_staged >= 3,
        "Should have at least 3 nodes (imports + function). Got: {}",
        stats.nodes_staged
    );

    // Should have 2 import edges
    let import_count = count_import_edges(&staging);
    assert_eq!(import_count, 2, "Should have 2 import edges");
}

#[test]
fn test_conditional_includes_all_extracted() {
    // Test that all includes are extracted regardless of preprocessor conditions
    // (We extract statically, not evaluating preprocessor)
    let content = r#"
#ifdef _WIN32
#include <windows.h>
#else
#include <unistd.h>
#endif

#if __cplusplus >= 201703L
#include <optional>
#include <variant>
#endif

#include <iostream>

void test() {}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test_conditional.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify all includes are extracted regardless of ifdef
    assert!(
        has_import_edge_to(&staging, "windows.h"),
        "expected import edge to windows.h"
    );
    assert!(
        has_import_edge_to(&staging, "unistd.h"),
        "expected import edge to unistd.h"
    );
    assert!(
        has_import_edge_to(&staging, "optional"),
        "expected import edge to optional"
    );
    assert!(
        has_import_edge_to(&staging, "variant"),
        "expected import edge to variant"
    );
    assert!(
        has_import_edge_to(&staging, "iostream"),
        "expected import edge to iostream"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 5,
        "Should extract all 5 includes (windows.h, unistd.h, optional, variant, iostream)"
    );
}

#[test]
fn test_template_header_includes() {
    // Test includes typical for template-heavy code
    let content = r#"
#include <type_traits>
#include <utility>
#include <tuple>
#include <array>

template<typename T>
class Container {
    T value;
};

void test() {}
"#;
    let tree = parse_cpp(content);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test_template_headers.cpp");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for template headers"
    );

    // Verify template-related headers
    assert!(
        has_import_edge_to(&staging, "type_traits"),
        "expected import edge to type_traits"
    );
    assert!(
        has_import_edge_to(&staging, "utility"),
        "expected import edge to utility"
    );
    assert!(
        has_import_edge_to(&staging, "tuple"),
        "expected import edge to tuple"
    );
    assert!(
        has_import_edge_to(&staging, "array"),
        "expected import edge to array"
    );

    let import_count = count_import_edges(&staging);
    assert_eq!(
        import_count, 4,
        "Should have 4 import edges for template-related headers"
    );
}

// ============================================================================
// Export Edge Tests
// ============================================================================

#[test]
fn test_export_file_scope_function() {
    let source = r#"
        #include <string>

        std::string greet(const std::string& name) {
            return "Hello, " + name + "!";
        }
    "#;
    let tree = parse_cpp(source);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.cpp"),
        &mut staging,
    );

    assert!(
        result.is_ok(),
        "build_graph should succeed for file-scope function"
    );

    // Verify export edge exists for the greet function
    assert!(
        has_export_edge_for(&staging, "greet"),
        "expected export edge for greet function"
    );

    // Should have at least one export edge
    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 1,
        "Should have at least 1 export edge for greet function"
    );
}

#[test]
fn test_export_file_scope_class() {
    let source = r#"
        class User {
        public:
            std::string getName() const {
                return name;
            }
        private:
            std::string name;
        };
    "#;
    let tree = parse_cpp(source);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.cpp"),
        &mut staging,
    );

    assert!(
        result.is_ok(),
        "build_graph should succeed for file-scope class"
    );

    // Verify export edge exists for the User class
    assert!(
        has_export_edge_for(&staging, "User"),
        "expected export edge for User class"
    );

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 1,
        "Should have at least 1 export edge for User class"
    );
}

#[test]
fn test_export_namespace_function() {
    let source = r#"
        namespace utils {
            std::string format(const std::string& str) {
                return str;
            }
        }
    "#;
    let tree = parse_cpp(source);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.cpp"),
        &mut staging,
    );

    assert!(
        result.is_ok(),
        "build_graph should succeed for namespace function"
    );

    // Verify export edge exists for the format function
    assert!(
        has_export_edge_for(&staging, "format"),
        "expected export edge for utils::format function"
    );

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 1,
        "Should have at least 1 export edge for namespace function"
    );
}

#[test]
fn test_export_namespace_class() {
    let source = r#"
        namespace api {
            class Service {
            public:
                void execute() {}
            };
        }
    "#;
    let tree = parse_cpp(source);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.cpp"),
        &mut staging,
    );

    assert!(
        result.is_ok(),
        "build_graph should succeed for namespace class"
    );

    // Verify export edge exists for the Service class
    assert!(
        has_export_edge_for(&staging, "Service"),
        "expected export edge for api::Service class"
    );

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 1,
        "Should have at least 1 export edge for namespace class"
    );
}

#[test]
fn test_export_struct() {
    let source = r#"
        struct Point {
            int x;
            int y;
        };
    "#;
    let tree = parse_cpp(source);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.cpp"),
        &mut staging,
    );

    assert!(result.is_ok(), "build_graph should succeed for struct");

    // Verify export edge exists for the Point struct
    assert!(
        has_export_edge_for(&staging, "Point"),
        "expected export edge for Point struct"
    );

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 1,
        "Should have at least 1 export edge for Point struct"
    );
}

#[test]
fn test_no_export_for_static_function() {
    let source = r#"
        static void helper() {
            // static function has internal linkage
        }
    "#;
    let tree = parse_cpp(source);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.cpp"),
        &mut staging,
    );

    assert!(
        result.is_ok(),
        "build_graph should succeed for static function"
    );

    // Verify NO export edge exists for static function
    assert!(
        !has_export_edge_for(&staging, "helper"),
        "should NOT have export edge for static function with internal linkage"
    );
}

#[test]
fn test_no_export_for_nested_class() {
    let source = r#"
        class Outer {
        public:
            class Inner {
                void method() {}
            };
        };
    "#;
    let tree = parse_cpp(source);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.cpp"),
        &mut staging,
    );

    assert!(
        result.is_ok(),
        "build_graph should succeed for nested class"
    );

    // Verify export edge exists for Outer (file-scope class)
    assert!(
        has_export_edge_for(&staging, "Outer"),
        "expected export edge for Outer class"
    );

    // Verify NO export edge exists for Inner (nested class)
    assert!(
        !has_export_edge_for(&staging, "Inner"),
        "should NOT have export edge for nested class Inner"
    );
}

#[test]
fn test_export_multiple_symbols() {
    let source = r#"
        class User {
            std::string name;
        };

        class Product {
            std::string title;
        };

        void processUser() {}
        void processProduct() {}
    "#;
    let tree = parse_cpp(source);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.cpp"),
        &mut staging,
    );

    assert!(
        result.is_ok(),
        "build_graph should succeed for multiple symbols"
    );

    // Verify export edges exist for all top-level symbols
    assert!(
        has_export_edge_for(&staging, "User"),
        "expected export edge for User class"
    );
    assert!(
        has_export_edge_for(&staging, "Product"),
        "expected export edge for Product class"
    );
    assert!(
        has_export_edge_for(&staging, "processUser"),
        "expected export edge for processUser function"
    );
    assert!(
        has_export_edge_for(&staging, "processProduct"),
        "expected export edge for processProduct function"
    );

    // Should have 4 export edges (2 classes + 2 functions)
    let export_count = count_export_edges(&staging);
    assert_eq!(
        export_count, 4,
        "Should have 4 export edges for 2 classes and 2 functions"
    );
}

#[test]
fn test_export_template_class() {
    let source = r#"
        template<typename T>
        class Container {
        public:
            T data;
            T getData() { return data; }
        };
    "#;
    let tree = parse_cpp(source);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.cpp"),
        &mut staging,
    );

    assert!(
        result.is_ok(),
        "build_graph should succeed for template class"
    );

    // Verify export edge exists for the template Container class
    assert!(
        has_export_edge_for(&staging, "Container"),
        "expected export edge for template Container class"
    );
}

// ========================================
// VISIBILITY TESTS
// ========================================

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn find_function_visibility(staging: &StagingGraph, name: &str) -> Option<String> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Function
        {
            let node_name = strings.get(&entry.name.index());
            if node_name.is_some_and(|n| n == name) {
                return entry
                    .visibility
                    .and_then(|id| strings.get(&id.index()).cloned());
            }
        }
    }
    None
}

fn find_class_visibility(staging: &StagingGraph, name: &str) -> Option<String> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Class
        {
            let node_name = strings.get(&entry.name.index());
            if node_name.is_some_and(|n| n == name) {
                return entry
                    .visibility
                    .and_then(|id| strings.get(&id.index()).cloned());
            }
        }
    }
    None
}

#[test]
fn test_function_visibility_public() {
    let source = r#"
// Public function (non-static, external linkage)
int public_function() {
    return 42;
}
"#;
    let tree = parse_cpp(source);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.cpp"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "public_function");
    assert_eq!(
        visibility,
        Some("public".to_string()),
        "Non-static function should have public visibility"
    );
}

#[test]
fn test_function_visibility_private() {
    let source = r#"
// Private function (static, internal linkage)
static int private_function() {
    return 42;
}
"#;
    let tree = parse_cpp(source);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.cpp"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_function_visibility(&staging, "private_function");
    assert_eq!(
        visibility,
        Some("private".to_string()),
        "Static function should have private visibility"
    );
}

#[test]
fn test_function_visibility_mixed() {
    let source = r#"
// Mix of public and private functions
static int helper() {
    return 1;
}

int api_function() {
    return helper() + 1;
}

static void internal_log() {
    // Private logging
}
"#;
    let tree = parse_cpp(source);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.cpp"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify visibility of each function
    assert_eq!(
        find_function_visibility(&staging, "helper"),
        Some("private".to_string()),
        "helper should be private (static)"
    );
    assert_eq!(
        find_function_visibility(&staging, "api_function"),
        Some("public".to_string()),
        "api_function should be public (non-static)"
    );
    assert_eq!(
        find_function_visibility(&staging, "internal_log"),
        Some("private".to_string()),
        "internal_log should be private (static)"
    );
}

#[test]
fn test_class_visibility() {
    let source = r#"
class MyClass {
public:
    void method() {}
};
"#;
    let tree = parse_cpp(source);
    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder;

    let result = builder.build_graph(
        &tree,
        source.as_bytes(),
        Path::new("test.cpp"),
        &mut staging,
    );
    assert!(result.is_ok(), "build_graph should succeed");

    let visibility = find_class_visibility(&staging, "MyClass");
    assert_eq!(
        visibility,
        Some("public".to_string()),
        "Class should have public visibility (default)"
    );
}
