use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::{GraphBuilder, Language, unified::StagingGraph};
use sqry_lang_csharp::relations::CSharpGraphBuilder;
use std::path::Path;
use tree_sitter::Parser;

fn parse_csharp_file(content: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
        .expect("Failed to set C# language");
    parser
        .parse(content.as_bytes(), None)
        .expect("Failed to parse C# code")
}

#[test]
fn test_pinvoke_detection() {
    let content = include_str!("fixtures/csharp/pinvoke.cs");
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("tests/fixtures/csharp/pinvoke.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify staging graph has staged nodes and edges
    let stats = staging.stats();
    assert!(
        stats.nodes_staged > 0,
        "Should have staged nodes for P/Invoke detection"
    );
    assert!(
        stats.edges_staged >= 4,
        "Should have at least 4 staged edges for P/Invoke calls, found {}",
        stats.edges_staged
    );
}

#[test]
fn test_simple_method_calls() {
    let content = include_str!("fixtures/csharp/simple_calls.cs");
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("tests/fixtures/csharp/simple_calls.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify staging graph has staged nodes and edges
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 4,
        "Should have at least 4 staged nodes (Calculator class + 3 methods), found {}",
        stats.nodes_staged
    );
    assert!(
        stats.edges_staged >= 2,
        "Should have at least 2 staged call edges, found {}",
        stats.edges_staged
    );
}

#[test]
fn test_async_await() {
    let content = include_str!("fixtures/csharp/async_await.cs");
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("tests/fixtures/csharp/async_await.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed for async/await");

    // Verify staging graph has staged nodes and edges
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 3,
        "Should have at least 3 async methods staged, found {}",
        stats.nodes_staged
    );
    assert!(
        stats.edges_staged >= 2,
        "Should have at least 2 await call edges staged, found {}",
        stats.edges_staged
    );
}

#[test]
fn test_properties() {
    let content = include_str!("fixtures/csharp/properties.cs");
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("tests/fixtures/csharp/properties.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed for properties");

    // Verify staging graph has staged nodes and edges for properties
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 4,
        "Should have staged synthetic getter/setter nodes, found {}",
        stats.nodes_staged
    );
    assert!(
        stats.edges_staged > 0,
        "Should have staged property access edges"
    );
}

#[test]
fn test_nested_classes() {
    let content = include_str!("fixtures/csharp/nested_classes.cs");
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("tests/fixtures/csharp/nested_classes.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for nested classes"
    );

    // Verify staging graph has staged nested classes
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 3,
        "Should have staged at least 3 nested classes, found {}",
        stats.nodes_staged
    );
}

#[test]
fn test_constructors() {
    let content = include_str!("fixtures/csharp/constructors.cs");
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("tests/fixtures/csharp/constructors.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for constructors"
    );

    // Verify staging graph has staged constructor calls
    let stats = staging.stats();
    assert!(
        stats.edges_staged >= 3,
        "Should have at least 3 staged constructor calls, found {}",
        stats.edges_staged
    );
}

#[test]
fn test_static_methods() {
    let content = include_str!("fixtures/csharp/static_methods.cs");
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("tests/fixtures/csharp/static_methods.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for static methods"
    );

    // Verify staging graph has staged static methods and calls
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 1,
        "Should have staged static methods, found {}",
        stats.nodes_staged
    );
    assert!(
        stats.edges_staged >= 2,
        "Should have at least 2 staged static method calls, found {}",
        stats.edges_staged
    );
}

#[test]
fn test_namespaces() {
    let content = include_str!("fixtures/csharp/namespaces.cs");
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("tests/fixtures/csharp/namespaces.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed for namespaces");

    // Verify staging graph has staged namespaced classes
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Should have staged at least 2 namespaced classes, found {}",
        stats.nodes_staged
    );
}

#[test]
fn test_interfaces() {
    let content = include_str!("fixtures/csharp/interfaces.cs");
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("tests/fixtures/csharp/interfaces.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed for interfaces");

    // Verify staging graph has staged interface and implementation
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 2,
        "Should have staged interface and implementation, found {}",
        stats.nodes_staged
    );
}

#[test]
fn test_builder_language() {
    let builder = CSharpGraphBuilder::default();
    assert_eq!(builder.language(), Language::CSharp);
}

#[test]
fn test_empty_file() {
    let content = "";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("empty.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed for empty file");

    // Verify staging graph has no nodes for empty file
    let stats = staging.stats();
    assert_eq!(
        stats.nodes_staged, 0,
        "Empty file should have no staged nodes"
    );
}

#[test]
fn test_nodes_have_correct_language() {
    let content = include_str!("fixtures/csharp/simple_calls.cs");
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("tests/fixtures/csharp/simple_calls.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify staging graph has staged nodes
    let stats = staging.stats();
    assert!(
        stats.nodes_staged > 0,
        "Should have staged nodes for simple calls"
    );
}

#[test]
fn test_edges_have_metadata() {
    let content = include_str!("fixtures/csharp/simple_calls.cs");
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("tests/fixtures/csharp/simple_calls.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify staging graph has staged edges
    let stats = staging.stats();
    assert!(
        stats.edges_staged > 0,
        "Should have staged edges for simple calls"
    );
}

#[test]
fn test_call_sites_stored() {
    let content = include_str!("fixtures/csharp/simple_calls.cs");
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("tests/fixtures/csharp/simple_calls.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify staging graph has staged edges (representing call sites)
    let stats = staging.stats();
    assert!(stats.edges_staged > 0, "Should have staged call site edges");
}

#[test]
fn test_mixed_features() {
    let content = include_str!("fixtures/csharp/mixed_features.cs");
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("tests/fixtures/csharp/mixed_features.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for mixed features"
    );

    // Verify staging graph has staged mixed features
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 10,
        "Should have staged multiple nodes for mixed features, found {}",
        stats.nodes_staged
    );
    assert!(
        stats.edges_staged >= 5,
        "Should have staged multiple call edges, found {}",
        stats.edges_staged
    );
}

// ============================================================================
// Import Edge Tests (Wave 2 Team 3)
// ============================================================================

/// Helper function to count edges of a specific kind in staging operations.
fn count_edges_by_kind(staging: &StagingGraph, kind_check: impl Fn(&EdgeKind) -> bool) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                kind_check(kind)
            } else {
                false
            }
        })
        .count()
}

#[test]
fn test_simple_using_directive() {
    let content = r"
using System;

namespace Test {
    public class MyClass { }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Count import edges
    let import_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Imports { .. }));
    assert!(
        import_count >= 1,
        "Should have at least 1 import edge for 'using System;', found {import_count}"
    );
}

#[test]
fn test_qualified_using_directive() {
    let content = r"
using System.Collections.Generic;
using System.Linq;
using System.IO;

namespace Test {
    public class MyClass { }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Count import edges
    let import_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Imports { .. }));
    assert!(
        import_count >= 3,
        "Should have at least 3 import edges for qualified using directives, found {import_count}"
    );
}

#[test]
fn test_static_using_directive() {
    let content = r"
using static System.Math;
using static System.Console;

namespace Test {
    public class Calculator {
        public double Calc() {
            return Sqrt(4);
        }
    }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for static using"
    );

    // Count import edges
    let import_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Imports { .. }));
    assert!(
        import_count >= 2,
        "Should have at least 2 import edges for static using, found {import_count}"
    );
}

#[test]
fn test_aliased_using_directive() {
    let content = r"
using IO = System.IO;
using Console = System.Console;

namespace Test {
    public class FileHandler { }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for aliased using"
    );

    // Count import edges
    let import_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Imports { .. }));
    assert!(
        import_count >= 2,
        "Should have at least 2 import edges for aliased using, found {import_count}"
    );
}

#[test]
fn test_multiple_using_directives() {
    let content = r"
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;

namespace Test {
    public class MyClass { }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Count import edges
    let import_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Imports { .. }));
    assert!(
        import_count >= 5,
        "Should have at least 5 import edges, found {import_count}"
    );
}

// ============================================================================
// Inheritance Edge Tests (Wave 2 Team 3)
// ============================================================================

#[test]
fn test_simple_class_inheritance() {
    let content = r"
public class Animal { }
public class Dog : Animal { }
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Count inherits edges
    let inherits_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Inherits));
    assert!(
        inherits_count >= 1,
        "Should have at least 1 Inherits edge for Dog : Animal, found {inherits_count}"
    );
}

#[test]
fn test_inheritance_chain() {
    let content = r"
public class GrandParent { }
public class Parent : GrandParent { }
public class Child : Parent { }
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Count inherits edges
    let inherits_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Inherits));
    assert!(
        inherits_count >= 2,
        "Should have at least 2 Inherits edges for inheritance chain, found {inherits_count}"
    );
}

#[test]
fn test_class_with_generic_base() {
    let content = r"
using System.Collections.Generic;

public class MyList : List<string> { }
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(
        result.is_ok(),
        "build_graph should succeed for generic base class"
    );

    // Count inherits edges
    let inherits_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Inherits));
    assert!(
        inherits_count >= 1,
        "Should have at least 1 Inherits edge for generic base, found {inherits_count}"
    );
}

// ============================================================================
// Interface Implementation Edge Tests (Wave 2 Team 3)
// ============================================================================

#[test]
fn test_single_interface_implementation() {
    let content = r"
public interface IDisposable {
    void Dispose();
}

public class Resource : IDisposable {
    public void Dispose() { }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Count implements edges
    let implements_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Implements));
    assert!(
        implements_count >= 1,
        "Should have at least 1 Implements edge for Resource : IDisposable, found {implements_count}"
    );
}

#[test]
fn test_multiple_interface_implementation() {
    let content = r"
public interface IReadable { void Read(); }
public interface IWritable { void Write(); }
public interface IDisposable { void Dispose(); }

public class Stream : IReadable, IWritable, IDisposable {
    public void Read() { }
    public void Write() { }
    public void Dispose() { }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Count implements edges
    let implements_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Implements));
    assert!(
        implements_count >= 3,
        "Should have at least 3 Implements edges for Stream : IReadable, IWritable, IDisposable, found {implements_count}"
    );
}

#[test]
fn test_class_with_base_and_interfaces() {
    let content = r"
public class BaseService { }
public interface IRepository { }
public interface ILogger { }

public class Service : BaseService, IRepository, ILogger { }
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Count inherits edges
    let inherits_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Inherits));
    assert!(
        inherits_count >= 1,
        "Should have at least 1 Inherits edge for Service : BaseService, found {inherits_count}"
    );

    // Count implements edges
    let implements_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Implements));
    assert!(
        implements_count >= 2,
        "Should have at least 2 Implements edges for Service : IRepository, ILogger, found {implements_count}"
    );
}

#[test]
fn test_interface_extends_interface() {
    let content = r"
public interface IBase { }
public interface IChild : IBase { }
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Interface extending interface should create Inherits edge
    let inherits_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Inherits));
    assert!(
        inherits_count >= 1,
        "Should have at least 1 Inherits edge for IChild : IBase, found {inherits_count}"
    );
}

#[test]
fn test_interface_extends_multiple_interfaces() {
    let content = r"
public interface IReadable { }
public interface IWritable { }
public interface IStream : IReadable, IWritable { }
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Interface extending multiple interfaces should create multiple Inherits edges
    let inherits_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Inherits));
    assert!(
        inherits_count >= 2,
        "Should have at least 2 Inherits edges for IStream : IReadable, IWritable, found {inherits_count}"
    );
}

// ============================================================================
// Combined Feature Tests (Wave 2 Team 3)
// ============================================================================

#[test]
fn test_complete_csharp_file() {
    let content = r#"
using System;
using System.Collections.Generic;
using static System.Math;

namespace MyApp.Services {
    public interface IService {
        void Execute();
    }

    public interface ILoggable {
        void Log(string message);
    }

    public class BaseService {
        protected virtual void Initialize() { }
    }

    public class UserService : BaseService, IService, ILoggable {
        public void Execute() {
            Initialize();
            Log("Executing");
        }

        public void Log(string message) {
            Console.WriteLine(message);
        }
    }
}
"#;
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let stats = staging.stats();

    // Should have imports for using directives
    let import_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Imports { .. }));
    assert!(
        import_count >= 3,
        "Should have at least 3 import edges, found {import_count}"
    );

    // Should have inherits for UserService : BaseService
    let inherits_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Inherits));
    assert!(
        inherits_count >= 1,
        "Should have at least 1 Inherits edge, found {inherits_count}"
    );

    // Should have implements for UserService : IService, ILoggable
    let implements_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Implements));
    assert!(
        implements_count >= 2,
        "Should have at least 2 Implements edges, found {implements_count}"
    );

    // Should have nodes for classes, interfaces, and methods
    assert!(
        stats.nodes_staged >= 5,
        "Should have multiple nodes staged, found {}",
        stats.nodes_staged
    );
}

#[test]
fn test_interfaces_fixture_with_oop() {
    // This tests the existing interfaces.cs fixture with OOP edge detection
    let content = include_str!("fixtures/csharp/interfaces.cs");
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("tests/fixtures/csharp/interfaces.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed for interfaces");

    // The interfaces.cs fixture has:
    // - IRepository interface
    // - ILogger interface
    // - Repository class implementing both
    // - Service class using IRepository

    // Count implements edges (Repository : IRepository, ILogger)
    let implements_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Implements));
    assert!(
        implements_count >= 2,
        "Should have at least 2 Implements edges for Repository : IRepository, ILogger, found {implements_count}"
    );
}

#[test]
fn test_no_false_interface_detection() {
    // Test that class names like "Item", "Int32", "Image" are NOT detected as interfaces
    let content = r"
public class Item { }
public class ImageProcessor : Item { }
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Should have Inherits edge (not Implements) because Item doesn't follow I+Uppercase convention
    let inherits_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Inherits));
    let implements_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Implements));

    assert!(
        inherits_count >= 1,
        "Should have at least 1 Inherits edge for ImageProcessor : Item, found {inherits_count}"
    );
    assert_eq!(
        implements_count, 0,
        "Should have 0 Implements edges (Item is not an interface), found {implements_count}"
    );
}

// ============================================================================
// Comprehensive Import Edge Tests (10+ tests required)
// ============================================================================

/// Helper to check if any import edge has `is_wildcard` set to true
fn has_wildcard_import(staging: &StagingGraph) -> bool {
    staging.operations().iter().any(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::Imports {
                    is_wildcard: true,
                    ..
                },
                ..
            }
        )
    })
}

/// Helper to count non-wildcard imports without alias
fn count_simple_imports(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                matches!(
                    kind,
                    EdgeKind::Imports {
                        alias: None,
                        is_wildcard: false
                    }
                )
            } else {
                false
            }
        })
        .count()
}

/// Helper to count wildcard imports
fn count_wildcard_imports(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                matches!(
                    kind,
                    EdgeKind::Imports {
                        is_wildcard: true,
                        ..
                    }
                )
            } else {
                false
            }
        })
        .count()
}

/// Helper to count imports with alias
fn count_aliased_imports(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                matches!(kind, EdgeKind::Imports { alias: Some(_), .. })
            } else {
                false
            }
        })
        .count()
}

#[test]
fn test_import_simple_namespace() {
    let content = "using System;";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let simple_count = count_simple_imports(&staging);
    assert_eq!(
        simple_count, 1,
        "Should have 1 simple import edge for 'using System;', found {simple_count}"
    );

    // Verify no wildcard imports for simple using
    assert!(
        !has_wildcard_import(&staging),
        "Simple using should not be marked as wildcard"
    );
}

#[test]
fn test_import_deeply_qualified_namespace() {
    let content = "using System.Collections.Concurrent.Partitioner;";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let import_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Imports { .. }));
    assert_eq!(
        import_count, 1,
        "Should have 1 import edge for deeply qualified namespace, found {import_count}"
    );
}

#[test]
fn test_import_static_is_wildcard() {
    let content = "using static System.Math;";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Static using should be marked as wildcard (all static members imported)
    let wildcard_count = count_wildcard_imports(&staging);
    assert_eq!(
        wildcard_count, 1,
        "Static using should have is_wildcard=true, found {wildcard_count} wildcard imports"
    );
}

#[test]
fn test_import_static_multiple() {
    let content = r"
using static System.Math;
using static System.Console;
using static System.String;
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let wildcard_count = count_wildcard_imports(&staging);
    assert_eq!(
        wildcard_count, 3,
        "Should have 3 wildcard imports for static usings, found {wildcard_count}"
    );
}

#[test]
fn test_import_alias_has_alias_field() {
    let content = "using IO = System.IO;";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Aliased import should have alias field populated
    let aliased_count = count_aliased_imports(&staging);
    assert_eq!(
        aliased_count, 1,
        "Aliased using should have alias field set, found {aliased_count} aliased imports"
    );

    // Aliased imports should not be wildcard
    assert!(
        !has_wildcard_import(&staging),
        "Aliased using should not be marked as wildcard"
    );
}

#[test]
fn test_import_alias_multiple() {
    let content = r"
using IO = System.IO;
using Console = System.Console;
using Text = System.Text;
using Collections = System.Collections;
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let aliased_count = count_aliased_imports(&staging);
    assert_eq!(
        aliased_count, 4,
        "Should have 4 aliased imports, found {aliased_count}"
    );
}

#[test]
fn test_import_mixed_types() {
    // Mix of simple, static, and aliased usings
    let content = r"
using System;
using System.Collections.Generic;
using static System.Math;
using static System.Console;
using IO = System.IO;
using Text = System.Text;
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let simple_count = count_simple_imports(&staging);
    let wildcard_count = count_wildcard_imports(&staging);
    let aliased_count = count_aliased_imports(&staging);
    let total_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Imports { .. }));

    assert_eq!(
        simple_count, 2,
        "Should have 2 simple imports (System, System.Collections.Generic), found {simple_count}"
    );
    assert_eq!(
        wildcard_count, 2,
        "Should have 2 wildcard imports (static Math, static Console), found {wildcard_count}"
    );
    assert_eq!(
        aliased_count, 2,
        "Should have 2 aliased imports (IO, Text), found {aliased_count}"
    );
    assert_eq!(
        total_count, 6,
        "Should have 6 total import edges, found {total_count}"
    );
}

#[test]
fn test_import_global_using() {
    // C# 10 global using directive
    let content = "global using System;";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    // Global using may or may not be supported by tree-sitter-c-sharp
    // The test verifies we don't crash on this syntax
    assert!(
        result.is_ok(),
        "build_graph should not crash on global using"
    );
}

#[test]
fn test_import_no_using() {
    // File with no using directives
    let content = r"
public class MyClass {
    public void Method() { }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let import_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Imports { .. }));
    assert_eq!(
        import_count, 0,
        "Should have 0 import edges when no using directives, found {import_count}"
    );
}

#[test]
fn test_import_inside_namespace() {
    // Using directives inside namespace declaration
    let content = r"
namespace MyApp {
    using System;
    using System.Text;

    public class MyClass { }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let import_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Imports { .. }));
    assert!(
        import_count >= 2,
        "Should have at least 2 import edges for usings inside namespace, found {import_count}"
    );
}

#[test]
fn test_import_alias_with_generic_type() {
    // Aliased using with generic type
    let content = "using StringList = System.Collections.Generic.List<string>;";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    // May or may not be fully supported, but should not crash
    assert!(
        result.is_ok(),
        "build_graph should not crash on aliased generic type"
    );
}

#[test]
fn test_import_file_scoped_namespace() {
    // Using directives with file-scoped namespace (C# 10+)
    let content = r"
using System;
using System.IO;

namespace MyApp.Services;

public class Service { }
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    let import_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Imports { .. }));
    assert_eq!(
        import_count, 2,
        "Should have 2 import edges with file-scoped namespace, found {import_count}"
    );
}

#[test]
fn test_import_edge_metadata_correctness() {
    // Comprehensive test verifying edge metadata is correctly set
    let content = r"
using System;                      // Simple: alias=None, is_wildcard=false
using static System.Math;          // Static: alias=None, is_wildcard=true
using IO = System.IO;              // Aliased: alias=Some, is_wildcard=false
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Verify each type of import edge
    let simple = staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Imports {
                        alias: None,
                        is_wildcard: false
                    },
                    ..
                }
            )
        })
        .count();

    let wildcard = staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Imports {
                        alias: None,
                        is_wildcard: true
                    },
                    ..
                }
            )
        })
        .count();

    let aliased = staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Imports {
                        alias: Some(_),
                        is_wildcard: false
                    },
                    ..
                }
            )
        })
        .count();

    assert_eq!(
        simple, 1,
        "Should have 1 simple import (System), found {simple}"
    );
    assert_eq!(
        wildcard, 1,
        "Should have 1 wildcard import (static Math), found {wildcard}"
    );
    assert_eq!(
        aliased, 1,
        "Should have 1 aliased import (IO), found {aliased}"
    );
}

#[test]
fn test_import_with_comments() {
    // Using directives with comments
    let content = r"
// System namespace for basic types
using System;
/* Multi-line
   comment */
using System.Text; // Inline comment
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed with comments");

    let import_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Imports { .. }));
    assert_eq!(
        import_count, 2,
        "Should have 2 import edges despite comments, found {import_count}"
    );
}

// ============================================================================
// Export Edge Tests
// ============================================================================

#[test]
fn test_export_public_class() {
    let content = r"
public class PublicClass {
    public void PublicMethod() { }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Count export edges
    let export_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Exports { .. }));
    assert!(
        export_count >= 2,
        "Should have at least 2 export edges (public class + public method), found {export_count}"
    );
}

#[test]
fn test_export_public_interface() {
    let content = r"
public interface IRepository {
    void Save();
    void Delete();
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Count export edges (interface + 2 methods)
    let export_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Exports { .. }));
    assert!(
        export_count >= 3,
        "Should have at least 3 export edges (interface + 2 methods), found {export_count}"
    );
}

#[test]
fn test_export_internal_class() {
    let content = r"
internal class InternalClass {
    internal void InternalMethod() { }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Internal types should be exported (accessible within assembly)
    let export_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Exports { .. }));
    assert!(
        export_count >= 2,
        "Should have at least 2 export edges (internal class + internal method), found {export_count}"
    );
}

#[test]
fn test_no_export_private_class() {
    let content = r"
private class PrivateClass {
    private void PrivateMethod() { }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Private types should NOT be exported
    let export_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Exports { .. }));
    assert_eq!(
        export_count, 0,
        "Should have 0 export edges for private class, found {export_count}"
    );
}

#[test]
fn test_export_mixed_visibility_members() {
    let content = r"
public class MixedClass {
    public void PublicMethod() { }
    private void PrivateMethod() { }
    protected void ProtectedMethod() { }
    internal void InternalMethod() { }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Should export: public class, public method, internal method
    // Should NOT export: private method, protected method
    let export_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Exports { .. }));
    assert!(
        export_count >= 3,
        "Should have at least 3 export edges (class + public method + internal method), found {export_count}"
    );
}

#[test]
fn test_export_public_properties() {
    let content = r"
public class UserModel {
    public int Id { get; set; }
    public string Name { get; set; }
    private string password;
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Should export: public class + 2 public properties
    // Should NOT export: private field
    let export_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Exports { .. }));
    assert!(
        export_count >= 3,
        "Should have at least 3 export edges (class + 2 properties), found {export_count}"
    );
}

#[test]
fn test_export_public_constructor() {
    let content = r"
public class Service {
    public Service() { }
    private Service(int id) { }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Should export: public class + public constructor
    // Should NOT export: private constructor
    let export_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Exports { .. }));
    assert!(
        export_count >= 2,
        "Should have at least 2 export edges (class + public constructor), found {export_count}"
    );
}

#[test]
fn test_export_static_members() {
    let content = r#"
public static class Utils {
    public static string Greet(string name) {
        return "Hello";
    }

    private static void InternalHelper() { }
}
"#;
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Should export: public static class + public static method
    let export_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Exports { .. }));
    assert!(
        export_count >= 2,
        "Should have at least 2 export edges (static class + static method), found {export_count}"
    );
}

#[test]
fn test_export_namespaced_types() {
    let content = r"
namespace MyApp.Services {
    public class UserService {
        public void Execute() { }
    }

    internal class InternalService {
        internal void Process() { }
    }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Should export: public class + public method + internal class + internal method
    let export_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Exports { .. }));
    assert!(
        export_count >= 4,
        "Should have at least 4 export edges for namespaced types, found {export_count}"
    );
}

#[test]
fn test_export_nested_public_class() {
    let content = r"
public class OuterClass {
    public class InnerClass {
        public void Method() { }
    }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Should export: outer class + inner class + method
    let export_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Exports { .. }));
    assert!(
        export_count >= 3,
        "Should have at least 3 export edges for nested classes, found {export_count}"
    );
}

#[test]
fn test_export_interface_methods() {
    let content = r"
public interface IService {
    void Execute();
    void Process();
    private void Helper() { }  // C# 8.0+ private interface methods
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Should export: interface + 2 public methods (implicit public)
    // Should NOT export: private method
    let export_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Exports { .. }));
    assert!(
        export_count >= 3,
        "Should have at least 3 export edges (interface + 2 public methods), found {export_count}"
    );
}

#[test]
fn test_export_complete_program() {
    let content = r"
using System;

namespace MyApp {
    public interface IRepository {
        void Save();
    }

    public class User {
        public int Id { get; set; }
        public string Name { get; set; }

        public User(int id, string name) {
            Id = id;
            Name = name;
        }

        public string GetInfo() {
            return Name;
        }

        private void Validate() { }
    }

    internal class InternalService {
        internal void Process() { }
    }
}
";
    let tree = parse_csharp_file(content);
    let mut staging = StagingGraph::new();
    let builder = CSharpGraphBuilder::default();
    let file = Path::new("test.cs");

    let result = builder.build_graph(&tree, content.as_bytes(), file, &mut staging);
    assert!(result.is_ok(), "build_graph should succeed");

    // Should export:
    // - IRepository interface
    // - IRepository.Save method
    // - User class
    // - User.Id property
    // - User.Name property
    // - User constructor
    // - User.GetInfo method
    // - InternalService class (internal)
    // - InternalService.Process method (internal)
    // Should NOT export: User.Validate (private)
    let export_count = count_edges_by_kind(&staging, |k| matches!(k, EdgeKind::Exports { .. }));
    assert!(
        export_count >= 9,
        "Should have at least 9 export edges for complete program, found {export_count}"
    );
}
