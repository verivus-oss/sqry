// Groovy graph builder tests - migrated to StagingGraph API (FR-2025-007)
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::{GraphBuilder, Language, NodeId};
use sqry_lang_groovy::relations::GroovyGraphBuilder;
use std::path::{Path, PathBuf};

fn parse_groovy(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_groovy_sqry::language())
        .expect("failed to set language");
    parser.parse(source, None).expect("failed to parse")
}

#[test]
fn test_extracts_classes_and_methods() {
    let source = r#"
class MyClass {
    def myMethod() {
        return 42
    }

    def anotherMethod() {
        return "hello"
    }
}
"#;

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let file_str = file.to_string_lossy();
    let class_id = NodeId::new(Language::Groovy, file_str.as_ref(), "MyClass");
    let method1_id = NodeId::new(Language::Groovy, file_str.as_ref(), "MyClass::myMethod");
    let method2_id = NodeId::new(
        Language::Groovy,
        file_str.as_ref(),
        "MyClass::anotherMethod",
    );

    // Verify class and method nodes exist
    let nodes: Vec<_> = staging.nodes().collect();
    assert!(!nodes.is_empty(), "Expected nodes, got {}", nodes.len());

    // Verify MyClass exists
    assert!(
        nodes.iter().any(|n| {
            matches!(n.entry.kind, NodeKind::Class)
                && staging
                    .resolve_node_name(n.entry)
                    .is_some_and(|name| name.contains("MyClass"))
        }),
        "Expected MyClass node"
    );

    // Verify methods exist (Groovy uses Function, not Method)
    assert!(
        nodes.iter().any(|n| {
            matches!(n.entry.kind, NodeKind::Function)
                && staging
                    .resolve_node_name(n.entry)
                    .is_some_and(|name| name.contains("myMethod"))
        }),
        "Expected myMethod"
    );

    assert!(
        nodes.iter().any(|n| {
            matches!(n.entry.kind, NodeKind::Function)
                && staging
                    .resolve_node_name(n.entry)
                    .is_some_and(|name| name.contains("anotherMethod"))
        }),
        "Expected anotherMethod"
    );

    let _ = (class_id, method1_id, method2_id); // Keep old IDs for compatibility
}

#[test]
fn test_creates_call_edges() {
    let source = r"
class BuildTasks {
    def validate() {
        checkDependencies()
    }

    def checkDependencies() {
        // empty
    }

    def build() {
        validate()
        compile()
    }

    def compile() {
        // empty
    }
}
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let file_str = file.to_string_lossy();
    let validate_id = NodeId::new(Language::Groovy, file_str.as_ref(), "BuildTasks::validate");
    let check_deps_id = NodeId::new(
        Language::Groovy,
        file_str.as_ref(),
        "BuildTasks::checkDependencies",
    );
    let build_id = NodeId::new(Language::Groovy, file_str.as_ref(), "BuildTasks::build");
    let compile_id = NodeId::new(Language::Groovy, file_str.as_ref(), "BuildTasks::compile");

    // Verify method nodes exist (Groovy uses Function, not Method)
    let nodes: Vec<_> = staging.nodes().collect();
    assert!(
        nodes
            .iter()
            .any(|n| matches!(n.entry.kind, NodeKind::Function)),
        "Expected function nodes"
    );

    // Verify call edges exist
    let edges: Vec<_> = staging.edges().collect();
    assert!(
        edges
            .iter()
            .any(|e| matches!(e.kind, EdgeKind::Calls { .. })),
        "Expected Calls edges"
    );

    let _ = (validate_id, check_deps_id, build_id, compile_id); // Keep old IDs
}

#[test]
fn test_handles_closures() {
    let source = r#"
def process = {
    println "processing"
}

def run() {
    process()
}
"#;

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let file_str = file.to_string_lossy();
    let process_id = NodeId::new(Language::Groovy, file_str.as_ref(), "process");
    let run_id = NodeId::new(Language::Groovy, file_str.as_ref(), "run");

    // Verify closure and function nodes exist
    let nodes: Vec<_> = staging.nodes().collect();
    assert!(!nodes.is_empty(), "Expected nodes for closure and function");

    // Verify 'process' exists (variable or function)
    assert!(
        nodes.iter().any(|n| {
            staging
                .resolve_node_name(n.entry)
                .is_some_and(|name| name.contains("process"))
        }),
        "Expected 'process' node"
    );

    // Verify 'run' function exists
    assert!(
        nodes.iter().any(|n| {
            matches!(n.entry.kind, NodeKind::Function)
                && staging
                    .resolve_node_name(n.entry)
                    .is_some_and(|name| name.contains("run"))
        }),
        "Expected 'run' function"
    );

    let _ = (process_id, run_id); // Keep old IDs
}

// ================================
// Import Edge Integration Tests
// ================================

#[test]
fn test_import_edges_simple() {
    let source = r"
import groovy.transform.ToString
import java.util.List
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    // Verify stats: should have import nodes and edges
    let stats = staging.stats();
    assert!(
        stats.nodes_staged >= 3,
        "Expected at least 3 nodes (module + 2 imports), got {}",
        stats.nodes_staged
    );
    assert!(
        stats.edges_staged >= 2,
        "Expected at least 2 import edges, got {}",
        stats.edges_staged
    );
}

#[test]
fn test_import_edges_wildcard() {
    let source = r"
import groovy.transform.*
import java.util.*
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.edges_staged >= 2,
        "Expected at least 2 wildcard import edges, got {}",
        stats.edges_staged
    );
}

#[test]
fn test_import_edges_aliased() {
    let source = r"
import groovy.transform.ToString as TS
import java.util.HashMap as HM
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.edges_staged >= 2,
        "Expected at least 2 aliased import edges, got {}",
        stats.edges_staged
    );
}

#[test]
fn test_import_edges_static() {
    let source = r"
import static java.lang.Math.PI
import static java.lang.System.out
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    assert!(
        stats.edges_staged >= 2,
        "Expected at least 2 static import edges, got {}",
        stats.edges_staged
    );
}

// ================================
// OOP Edge Integration Tests
// ================================

#[test]
fn test_oop_edges_inheritance() {
    let source = r"
class Animal {
}

class Dog extends Animal {
}

class Cat extends Animal {
}
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    // 3 classes, 2 inheritance edges
    assert!(
        stats.nodes_staged >= 3,
        "Expected at least 3 class nodes, got {}",
        stats.nodes_staged
    );
    assert!(
        stats.edges_staged >= 2,
        "Expected at least 2 inheritance edges, got {}",
        stats.edges_staged
    );
}

#[test]
fn test_oop_edges_external_parent() {
    let source = r"
class MyServlet extends GroovyServlet {
    def doGet() {
        // handle request
    }
}
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    // MyServlet class + GroovyServlet (external) + method
    assert!(
        stats.nodes_staged >= 3,
        "Expected at least 3 nodes, got {}",
        stats.nodes_staged
    );
    assert!(
        stats.edges_staged >= 1,
        "Expected at least 1 inheritance edge, got {}",
        stats.edges_staged
    );
}

#[test]
fn test_combined_imports_and_inheritance() {
    let source = r"
import java.util.ArrayList
import java.util.List

class MyList extends ArrayList {
    def add(item) {
        super.add(item)
    }
}
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    // 2 imports + module + MyList + ArrayList (for inheritance) + method
    assert!(
        stats.nodes_staged >= 5,
        "Expected at least 5 nodes, got {}",
        stats.nodes_staged
    );
    // 2 import edges + 1 inheritance edge
    assert!(
        stats.edges_staged >= 3,
        "Expected at least 3 edges (imports + inheritance), got {}",
        stats.edges_staged
    );
}

// ================================
// Interface Implementation Tests (Wave 3)
// ================================

#[test]
fn test_interface_declaration_creates_interface_node() {
    // Verify that interface declarations are properly recognized
    let source = r"
interface Runnable {
    void run()
}
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    // Should have at least the interface node
    assert!(
        stats.nodes_staged >= 1,
        "Expected at least 1 interface node, got {}",
        stats.nodes_staged
    );
}

#[test]
fn test_class_implements_interface() {
    // In Groovy grammar, class implementing interface uses "extends"
    // Our implementation should detect this and create Implements edge (not Inherits)
    let source = r"
interface Runnable {
    void run()
}

class Task extends Runnable {
    void run() {
        println 'running'
    }
}
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    // Runnable (interface) + Task (class) + run method = 3+ nodes
    assert!(
        stats.nodes_staged >= 3,
        "Expected at least 3 nodes (interface + class + method), got {}",
        stats.nodes_staged
    );

    // Verify we have an Implements edge (Task -> Runnable), NOT an Inherits edge
    let ops = staging.operations();
    let implements_edge = ops.iter().find(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::Implements,
                ..
            }
        )
    });
    assert!(
        implements_edge.is_some(),
        "Expected EdgeKind::Implements edge for class extending interface. \
         Operations: {:?}",
        ops.iter()
            .filter(|op| matches!(op, StagingOp::AddEdge { .. }))
            .collect::<Vec<_>>()
    );

    // Verify there is NO Inherits edge (class-to-interface should be Implements)
    let inherits_edge = ops.iter().find(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::Inherits,
                ..
            }
        )
    });
    assert!(
        inherits_edge.is_none(),
        "Expected NO EdgeKind::Inherits edge for class extending interface"
    );
}

#[test]
fn test_interface_extends_interface() {
    // Interface extending another interface should use Inherits edge (not Implements)
    let source = r"
interface Closeable {
    void close()
}

interface AutoCloseable extends Closeable {
}
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    // 2 interfaces
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 interface nodes, got {}",
        stats.nodes_staged
    );

    // Verify we have an Inherits edge (AutoCloseable -> Closeable)
    let ops = staging.operations();
    let inherits_edge = ops.iter().find(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::Inherits,
                ..
            }
        )
    });
    assert!(
        inherits_edge.is_some(),
        "Expected EdgeKind::Inherits edge for interface extending interface. \
         Operations: {:?}",
        ops.iter()
            .filter(|op| matches!(op, StagingOp::AddEdge { .. }))
            .collect::<Vec<_>>()
    );

    // Verify there is NO Implements edge (interface-to-interface should be Inherits)
    let implements_edge = ops.iter().find(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::Implements,
                ..
            }
        )
    });
    assert!(
        implements_edge.is_none(),
        "Expected NO EdgeKind::Implements edge for interface extending interface"
    );
}

#[test]
fn test_class_extends_class_with_interfaces_present() {
    // When both classes and interfaces exist, class-to-class should use Inherits (not Implements)
    let source = r"
interface Runnable {
    void run()
}

class BaseWorker {
    def work() {}
}

class AdvancedWorker extends BaseWorker {
    def work() {
        println 'working harder'
    }
}
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    // Runnable + BaseWorker + AdvancedWorker + methods = 5+ nodes
    assert!(
        stats.nodes_staged >= 5,
        "Expected at least 5 nodes, got {}",
        stats.nodes_staged
    );

    // Verify we have an Inherits edge (AdvancedWorker -> BaseWorker)
    let ops = staging.operations();
    let inherits_edge = ops.iter().find(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::Inherits,
                ..
            }
        )
    });
    assert!(
        inherits_edge.is_some(),
        "Expected EdgeKind::Inherits edge for class extending class. \
         Operations: {:?}",
        ops.iter()
            .filter(|op| matches!(op, StagingOp::AddEdge { .. }))
            .collect::<Vec<_>>()
    );

    // Verify there is NO Implements edge (class-to-class should be Inherits)
    let implements_edge = ops.iter().find(|op| {
        matches!(
            op,
            StagingOp::AddEdge {
                kind: EdgeKind::Implements,
                ..
            }
        )
    });
    assert!(
        implements_edge.is_none(),
        "Expected NO EdgeKind::Implements edge for class extending class"
    );
}

#[test]
fn test_annotation_interface() {
    // @interface declarations should also be recognized as interfaces
    let source = r"
@interface MyAnnotation {
    String value() default ''
}

class MyClass {
}
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    // @interface + class = 2+ nodes
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 nodes (@interface + class), got {}",
        stats.nodes_staged
    );
}

// ================================
// Additional Import Tests (Wave 3)
// ================================

#[test]
fn test_import_edges_static_wildcard() {
    let source = r"
import static java.lang.Math.*
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    // module + import node = 2 nodes
    assert!(
        stats.nodes_staged >= 2,
        "Expected at least 2 nodes for static wildcard import, got {}",
        stats.nodes_staged
    );
    assert!(
        stats.edges_staged >= 1,
        "Expected at least 1 import edge, got {}",
        stats.edges_staged
    );
}

#[test]
fn test_import_edges_deeply_nested_package() {
    let source = r"
import org.springframework.web.servlet.mvc.Controller
import com.example.service.impl.UserServiceImpl
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    // module + 2 import nodes = 3 nodes
    assert!(
        stats.nodes_staged >= 3,
        "Expected at least 3 nodes for deeply nested imports, got {}",
        stats.nodes_staged
    );
    assert!(
        stats.edges_staged >= 2,
        "Expected at least 2 import edges, got {}",
        stats.edges_staged
    );
}

#[test]
fn test_mixed_imports_and_interfaces() {
    // Complex scenario: imports, interfaces, and classes
    let source = r"
import java.util.List
import java.util.Map

interface DataProcessor {
    void process(List data)
}

class MyProcessor extends DataProcessor {
    void process(List data) {
        data.each { println it }
    }
}
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    // module + 2 imports + interface + class + method = 6+ nodes
    assert!(
        stats.nodes_staged >= 5,
        "Expected at least 5 nodes, got {}",
        stats.nodes_staged
    );
    // 2 import edges + 1 implements edge = 3+ edges
    assert!(
        stats.edges_staged >= 3,
        "Expected at least 3 edges (imports + implements), got {}",
        stats.edges_staged
    );
}

#[test]
fn test_multiple_interface_hierarchy() {
    // Test multiple levels of interface inheritance
    let source = r"
interface Readable {
    def read()
}

interface Streamable extends Readable {
    def stream()
}

interface BufferedStreamable extends Streamable {
    def buffer()
}
";

    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;
    let file = PathBuf::from("test.groovy");

    builder
        .build_graph(&tree, source.as_bytes(), &file, &mut staging)
        .unwrap();

    let stats = staging.stats();
    // 3 interfaces = 3+ nodes
    assert!(
        stats.nodes_staged >= 3,
        "Expected at least 3 interface nodes, got {}",
        stats.nodes_staged
    );
    // 2 inherits edges (Streamable->Readable, BufferedStreamable->Streamable)
    assert!(
        stats.edges_staged >= 2,
        "Expected at least 2 inherits edges, got {}",
        stats.edges_staged
    );
}

#[test]
#[ignore = "AST exploration test - run manually with --ignored"]
fn explore_groovy_type_annotations() {
    use tree_sitter::Node;

    fn print_ast(node: Node, content: &[u8], depth: usize) {
        let indent = "  ".repeat(depth);
        let kind = node.kind();
        let text = if node.named_child_count() == 0 {
            node.utf8_text(content).unwrap_or("")
        } else {
            ""
        };
        println!("{}{} '{}'", indent, kind, text);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            print_ast(child, content, depth + 1);
        }
    }

    let source = r#"
class User {
    String name
    int age
    List<String> tags
}

String getName() {
    return "test"
}

void process(String input, int count) {
    println input
}
"#;

    let tree = parse_groovy(source);
    println!("\n=== GROOVY AST STRUCTURE ===\n");
    print_ast(tree.root_node(), source.as_bytes(), 0);
}

#[test]
#[ignore = "AST exploration for def keyword"]
fn explore_def_keyword() {
    use tree_sitter::Node;

    fn print_ast(node: Node, content: &[u8], depth: usize) {
        let indent = "  ".repeat(depth);
        let kind = node.kind();
        let text = if node.named_child_count() == 0 {
            node.utf8_text(content).unwrap_or("")
        } else {
            ""
        };
        println!("{}{} '{}'", indent, kind, text);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            print_ast(child, content, depth + 1);
        }
    }

    let source = "def x = null";
    let tree = parse_groovy(source);
    println!("\n=== DEF KEYWORD AST ===\n");
    print_ast(tree.root_node(), source.as_bytes(), 0);
}

#[test]
#[ignore = "Parameter AST exploration"]
fn explore_parameter_ast() {
    use tree_sitter::Node;

    fn print_ast(node: Node, content: &[u8], depth: usize) {
        let indent = "  ".repeat(depth);
        let kind = node.kind();
        let text = if node.named_child_count() == 0 {
            node.utf8_text(content).unwrap_or("")
        } else {
            ""
        };
        println!("{}{} '{}'", indent, kind, text);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            print_ast(child, content, depth + 1);
        }
    }

    let source = r#"
void greet(String name) {
    println name
}
"#;

    let tree = parse_groovy(source);
    println!("\n=== PARAMETER AST ===\n");
    print_ast(tree.root_node(), source.as_bytes(), 0);
}

#[test]
#[ignore = "Explore def parameter AST"]
fn explore_def_parameter() {
    use tree_sitter::Node;

    fn print_ast(node: Node, content: &[u8], depth: usize) {
        let indent = "  ".repeat(depth);
        let kind = node.kind();
        let text = if node.named_child_count() == 0 {
            node.utf8_text(content).unwrap_or("")
        } else {
            ""
        };
        println!("{}{} '{}'", indent, kind, text);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            print_ast(child, content, depth + 1);
        }
    }

    let source = r#"
void process(def input) {
    println input
}
"#;

    let tree = parse_groovy(source);
    println!("\n=== DEF PARAMETER AST ===\n");
    print_ast(tree.root_node(), source.as_bytes(), 0);
}

#[test]
#[ignore = "Explore constructor AST"]
fn explore_constructor() {
    use tree_sitter::Node;

    fn print_ast(node: Node, content: &[u8], depth: usize) {
        let indent = "  ".repeat(depth);
        let kind = node.kind();
        let text = if node.named_child_count() == 0 {
            node.utf8_text(content).unwrap_or("")
        } else {
            ""
        };
        println!("{}{} '{}'", indent, kind, text);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            print_ast(child, content, depth + 1);
        }
    }

    let source = r#"
class Person {
    String name

    Person(String initialName) {
        this.name = initialName
    }
}
"#;

    let tree = parse_groovy(source);
    println!("\n=== CONSTRUCTOR AST ===\n");
    print_ast(tree.root_node(), source.as_bytes(), 0);
}

#[test]
#[ignore = "Explore native method AST"]
fn explore_native_method() {
    use tree_sitter::Node;

    fn print_ast(node: Node, content: &[u8], depth: usize) {
        let indent = "  ".repeat(depth);
        let kind = node.kind();
        let text = if node.named_child_count() == 0 {
            node.utf8_text(content).unwrap_or("")
        } else {
            ""
        };
        println!("{}{} '{}'", indent, kind, text);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            print_ast(child, content, depth + 1);
        }
    }

    let source = "native String getNativeLibraryPath()";

    let tree = parse_groovy(source);
    println!("\n=== NATIVE METHOD AST ===\n");
    print_ast(tree.root_node(), source.as_bytes(), 0);
}

#[test]
#[ignore = "Explore Native.load() AST"]
fn explore_native_load_ast() {
    use tree_sitter::Node;

    fn print_ast(node: Node, content: &[u8], depth: usize) {
        let indent = "  ".repeat(depth);
        let kind = node.kind();
        let text = if node.named_child_count() == 0 {
            node.utf8_text(content).unwrap_or("")
        } else {
            ""
        };
        println!("{}{} '{}'", indent, kind, text);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            print_ast(child, content, depth + 1);
        }
    }

    let source = r#"
MyLib lib = Native.load("mylib", MyLib.class)
"#;

    let tree = parse_groovy(source);
    println!("\n=== NATIVE.LOAD() AST ===\n");
    print_ast(tree.root_node(), source.as_bytes(), 0);
}

#[test]
#[ignore = "Explore static native method AST"]
fn explore_static_native_method() {
    use tree_sitter::Node;

    fn print_ast(node: Node, content: &[u8], depth: usize) {
        let indent = "  ".repeat(depth);
        let kind = node.kind();
        let text = if node.named_child_count() == 0 {
            node.utf8_text(content).unwrap_or("")
        } else {
            ""
        };
        println!("{}{} '{}'", indent, kind, text);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            print_ast(child, content, depth + 1);
        }
    }

    let source = "static native void staticNative()";

    let tree = parse_groovy(source);
    println!("\n=== STATIC NATIVE METHOD AST ===\n");
    print_ast(tree.root_node(), source.as_bytes(), 0);
}

#[test]
#[ignore = "Explore private native method AST"]
fn explore_private_native_method() {
    use tree_sitter::Node;

    fn print_ast(node: Node, content: &[u8], depth: usize) {
        let indent = "  ".repeat(depth);
        let kind = node.kind();
        let text = if node.named_child_count() == 0 {
            node.utf8_text(content).unwrap_or("")
        } else {
            ""
        };
        println!("{}{} '{}'", indent, kind, text);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            print_ast(child, content, depth + 1);
        }
    }

    let source = "private native void nativeHelper()";

    let tree = parse_groovy(source);
    println!("\n=== PRIVATE NATIVE METHOD AST ===\n");
    print_ast(tree.root_node(), source.as_bytes(), 0);
}

#[test]
#[ignore = "Explore JNA interface with qualified annotation AST"]
fn explore_jna_qualified_annotation() {
    use tree_sitter::Node;

    fn print_ast(node: Node, content: &[u8], depth: usize) {
        let indent = "  ".repeat(depth);
        let kind = node.kind();
        let text = if node.named_child_count() == 0 {
            node.utf8_text(content).unwrap_or("")
        } else {
            ""
        };
        println!("{}{} '{}'", indent, kind, text);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            print_ast(child, content, depth + 1);
        }
    }

    let source = r#"
@com.sun.jna.NativeLibrary("mylib")
interface MyLib extends Library {
    void doWork()
}
"#;

    let tree = parse_groovy(source);
    println!("\n=== JNA QUALIFIED ANNOTATION AST ===\n");
    print_ast(tree.root_node(), source.as_bytes(), 0);
}

// ================================
// FFI Detection Tests (JNI / JNA)
// ================================

#[test]
fn test_native_method_creates_ffi_edge() {
    // native method should create an FfiCall edge (JNI)
    let source = "native String getNativeLibraryPath()";
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify FFI edge exists
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for native method, found {ffi_count}"
    );

    // Verify FFI target
    let nodes: Vec<_> = staging.nodes().collect();
    let node_names: Vec<String> = nodes
        .iter()
        .filter_map(|n| staging.resolve_node_name(n.entry))
        .map(ToString::to_string)
        .collect();

    assert!(
        node_names
            .iter()
            .any(|name| name == "<ffi:getNativeLibraryPath>"),
        "Expected FFI target <ffi:getNativeLibraryPath>, found: {node_names:?}"
    );
}

#[test]
fn test_native_method_in_class_creates_ffi_edge() {
    // native method inside a class should create an FfiCall edge from a Method node
    let source = r#"
class NativeLib {
    native String nativeMethod()
}
"#;
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify FFI edge exists
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for native method in class, found {ffi_count}"
    );

    // Verify FFI target uses qualified name
    let nodes: Vec<_> = staging.nodes().collect();
    let node_names: Vec<String> = nodes
        .iter()
        .filter_map(|n| staging.resolve_node_name(n.entry))
        .map(ToString::to_string)
        .collect();

    assert!(
        node_names
            .iter()
            .any(|name| name == "<ffi:NativeLib.nativeMethod>"),
        "Expected FFI target <ffi:NativeLib.nativeMethod>, found: {node_names:?}"
    );
}

#[test]
fn test_multiple_native_methods() {
    // Multiple native methods should each create an FfiCall edge
    let source = r#"
native boolean loadLibrary(String path)
native void unloadLibrary()
native int getVersion()
"#;
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify all 3 FFI edges exist
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 3,
        "Expected 3 FfiCall edges for 3 native methods, found {ffi_count}"
    );
}

#[test]
fn test_non_native_method_no_ffi_edge() {
    // Regular (non-native) methods should NOT create FfiCall edges
    let source = "String regularMethod() { return 'Hello' }";
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify no FFI edges
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges for regular method, found {ffi_count}"
    );
}

#[test]
fn test_jna_interface_creates_ffi_edges() {
    // @NativeLibrary annotation on interface should create FFI edges for all methods
    let source = r#"
@NativeLibrary("mylib")
interface MyLib extends Library {
    String getString(int index)
    void setString(int index, String value)
    int getCount()
}
"#;
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify all 3 methods have FFI edges
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 3,
        "Expected 3 FfiCall edges for JNA interface methods, found {ffi_count}"
    );

    // Verify FFI targets
    let nodes: Vec<_> = staging.nodes().collect();
    let node_names: Vec<String> = nodes
        .iter()
        .filter_map(|n| staging.resolve_node_name(n.entry))
        .map(ToString::to_string)
        .collect();

    assert!(
        node_names
            .iter()
            .any(|name| name == "<ffi:MyLib.getString>"),
        "Expected <ffi:MyLib.getString>"
    );
    assert!(
        node_names
            .iter()
            .any(|name| name == "<ffi:MyLib.setString>"),
        "Expected <ffi:MyLib.setString>"
    );
    assert!(
        node_names.iter().any(|name| name == "<ffi:MyLib.getCount>"),
        "Expected <ffi:MyLib.getCount>"
    );
}

#[test]
fn test_jna_interface_with_qualified_annotation() {
    // @com.sun.jna.NativeLibrary should also be detected
    let source = r#"
@com.sun.jna.NativeLibrary("mylib")
interface MyLib extends Library {
    void doWork()
}
"#;
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify FFI edge exists
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for qualified @NativeLibrary, found {ffi_count}"
    );
}

#[test]
fn test_interface_without_native_library_annotation_no_ffi() {
    // Regular interface (without @NativeLibrary) should NOT create FFI edges
    let source = r#"
interface MyInterface {
    void doWork()
}
"#;
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify no FFI edges
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges for regular interface, found {ffi_count}"
    );
}

#[test]
fn test_mixed_native_and_jna() {
    // Mix of native methods and JNA interface
    let source = r#"
class NativeLib {
    native String jniMethod()
}

@NativeLibrary("mylib")
interface JnaLib extends Library {
    void jnaMethod()
}
"#;
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify both FFI edges exist
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 2,
        "Expected 2 FfiCall edges (1 JNI + 1 JNA), found {ffi_count}"
    );
}

#[test]
fn test_native_method_with_parameters() {
    // native method with typed parameters
    let source = "native int process(String input, int count)";
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify FFI edge exists
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for native method with params, found {ffi_count}"
    );
}

#[test]
fn test_overloaded_native_methods_collision() {
    // Overloaded native methods with same name but different signatures
    // Currently collapse into a single FFI node (name-only, no JVM signature mangling)
    // This test documents the limitation - future work could add signature disambiguation
    let source = r#"
class NativeLib {
    native void process(int x)
    native void process(String s)
}
"#;
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Both methods create FFI edges, but they target the same FFI node (collision)
    // We should have 2 FfiCall edges (one per native method)
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 2,
        "Expected 2 FfiCall edges (one per overload), found {ffi_count}"
    );

    // Verify both target the same FFI node name (documenting the collision)
    // Both should create edges to "<ffi:NativeLib.process>"
    let nodes: Vec<_> = staging.nodes().collect();
    let node_names: Vec<String> = nodes
        .iter()
        .filter_map(|n| staging.resolve_node_name(n.entry))
        .map(ToString::to_string)
        .collect();

    let ffi_node_names: Vec<_> = node_names
        .iter()
        .filter(|name| name.starts_with("<ffi:"))
        .collect();

    // Should have only 1 distinct FFI target node (collision)
    assert_eq!(
        ffi_node_names.len(),
        1,
        "Expected 1 FFI target node (collision), found {}. FFI nodes: {:?}",
        ffi_node_names.len(),
        ffi_node_names
    );
}

#[test]
fn test_private_native_method() {
    // private native method should still create FFI edge
    let source = "private native void nativeHelper()";
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify FFI edge exists
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for private native method, found {ffi_count}"
    );
}

#[test]
fn test_native_load_call() {
    // Direct Native.load() call should create FFI edges
    let source = r#"
interface MyLib extends Library {
    void doWork()
}

class Test {
    void setup() {
        MyLib lib = Native.load("mylib", MyLib.class)
    }
}
"#;
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify FFI edge exists for Native.load()
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected exactly 1 FfiCall edge for Native.load(), found {ffi_count}"
    );

    // Verify the specific FFI target node exists
    let nodes: Vec<_> = staging.nodes().collect();
    let node_names: Vec<String> = nodes
        .iter()
        .filter_map(|n| staging.resolve_node_name(n.entry))
        .map(ToString::to_string)
        .collect();

    assert!(
        node_names.iter().any(|name| name == "<ffi:MyLib.doWork>"),
        "Expected FFI target '<ffi:MyLib.doWork>', got nodes: {:?}",
        node_names
            .iter()
            .filter(|n| n.starts_with("<ffi:"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_native_loadlibrary_call() {
    // Native.loadLibrary() should also create FFI edges
    let source = r#"
interface MyLib extends Library {
    int getValue()
}

class Test {
    void init() {
        MyLib lib = Native.loadLibrary("mylib", MyLib.class)
    }
}
"#;
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify FFI edge exists
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected exactly 1 FfiCall edge for Native.loadLibrary(), found {ffi_count}"
    );

    // Verify the specific FFI target node exists
    let nodes: Vec<_> = staging.nodes().collect();
    let node_names: Vec<String> = nodes
        .iter()
        .filter_map(|n| staging.resolve_node_name(n.entry))
        .map(ToString::to_string)
        .collect();

    assert!(
        node_names.iter().any(|name| name == "<ffi:MyLib.getValue>"),
        "Expected FFI target '<ffi:MyLib.getValue>', got nodes: {:?}",
        node_names
            .iter()
            .filter(|n| n.starts_with("<ffi:"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_native_load_prefix_matching_regression() {
    // Regression test: Native.load("MyLib", MyLib.class) should NOT create
    // FFI edges for unrelated "MyLibHelper" class methods
    // Guards against the prefix matching bug where "MyLibHelper::method" would
    // incorrectly match when searching for "MyLib"
    let source = r#"
interface MyLib extends Library {
    void doWork()
}

interface MyLibHelper extends Library {
    void helpWork()
}

class Test {
    void setup() {
        MyLib lib = Native.load("mylib", MyLib.class)
    }
}
"#;
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Should create exactly 1 FFI edge for MyLib.doWork (NOT MyLibHelper.helpWork)
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected exactly 1 FfiCall edge (MyLib.doWork only), found {ffi_count}"
    );

    // Verify ONLY MyLib.doWork target exists, NOT MyLibHelper.helpWork
    let nodes: Vec<_> = staging.nodes().collect();
    let node_names: Vec<String> = nodes
        .iter()
        .filter_map(|n| staging.resolve_node_name(n.entry))
        .map(ToString::to_string)
        .collect();

    assert!(
        node_names.iter().any(|name| name == "<ffi:MyLib.doWork>"),
        "Expected FFI target '<ffi:MyLib.doWork>'"
    );

    // Verify NO FFI edges for MyLibHelper (guards against prefix matching regression)
    let ffi_nodes: Vec<_> = node_names
        .iter()
        .filter(|n| n.starts_with("<ffi:"))
        .collect();
    assert!(
        !ffi_nodes.iter().any(|name| name.contains("MyLibHelper")),
        "Should NOT create FFI edges for MyLibHelper (prefix collision), got FFI nodes: {:?}",
        ffi_nodes
    );
}

#[test]
fn test_native_load_fully_qualified_class_literal() {
    // Regression test: Fully qualified class literals (com.example.MyLib.class)
    // should extract "MyLib" (last segment) not "com" (first segment)
    // Even without a package statement, the class literal can be fully qualified
    let source = r#"
interface MyLib extends Library {
    void doWork()
    int getValue()
}

class Test {
    void setup() {
        // Fully qualified class literal - should extract "MyLib" not "com"
        MyLib lib = Native.load("mylib", com.example.MyLib.class)
    }
}
"#;
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Should create exactly 2 FFI edges (doWork + getValue)
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 2,
        "Expected exactly 2 FfiCall edges for fully qualified class literal, found {ffi_count}"
    );

    // Verify FFI targets exist for MyLib methods
    let nodes: Vec<_> = staging.nodes().collect();
    let node_names: Vec<String> = nodes
        .iter()
        .filter_map(|n| staging.resolve_node_name(n.entry))
        .map(ToString::to_string)
        .collect();

    assert!(
        node_names.iter().any(|name| name == "<ffi:MyLib.doWork>"),
        "Expected FFI target '<ffi:MyLib.doWork>', got nodes: {:?}",
        node_names
            .iter()
            .filter(|n| n.starts_with("<ffi:"))
            .collect::<Vec<_>>()
    );

    assert!(
        node_names.iter().any(|name| name == "<ffi:MyLib.getValue>"),
        "Expected FFI target '<ffi:MyLib.getValue>', got nodes: {:?}",
        node_names
            .iter()
            .filter(|n| n.starts_with("<ffi:"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_native_library_without_extends_library() {
    // @NativeLibrary without extending Library should NOT create FFI edges
    let source = r#"
@NativeLibrary("mylib")
class NotAnInterface {
    void method()
}
"#;
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify NO FFI edges (must extend Library)
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges for @NativeLibrary without Library inheritance, found {ffi_count}"
    );
}

#[test]
fn test_multi_annotation_ordering() {
    // Multiple annotations including @NativeLibrary
    let source = r#"
@Deprecated
@NativeLibrary("mylib")
interface MyLib extends Library {
    void doWork()
}
"#;
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify FFI edge exists despite multiple annotations
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge with multiple annotations, found {ffi_count}"
    );
}

#[test]
fn test_static_native_method() {
    // static native method should create FFI edge
    let source = "static native void staticNative()";
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify FFI edge exists
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for static native method, found {ffi_count}"
    );
}

#[test]
fn test_public_static_native_method() {
    // public static native method should create FFI edge
    let source = "public static native int calculate()";
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify FFI edge exists
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge for public static native method, found {ffi_count}"
    );
}

#[test]
fn test_jna_interface_filters_default_methods() {
    // JNA interface with default method - only abstract methods should get FFI edges
    let source = r#"
@NativeLibrary("mylib")
interface MyLib extends Library {
    void abstractMethod()

    default void defaultMethod() {
        println("default")
    }
}
"#;
    let tree = parse_groovy(source);
    let mut staging = StagingGraph::new();
    let builder = GroovyGraphBuilder;

    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("test.groovy"),
            &mut staging,
        )
        .unwrap();

    // Verify only 1 FFI edge (for abstract method, not default)
    let ffi_count = count_ffi_call_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected 1 FfiCall edge (default method filtered), found {ffi_count}"
    );
}

/// Helper to count FfiCall edges in staging graph
fn count_ffi_call_edges(staging: &StagingGraph) -> usize {
    use sqry_core::graph::unified::edge::kind::EdgeKind;

    staging
        .edges()
        .filter(|edge| matches!(edge.kind, EdgeKind::FfiCall { .. }))
        .count()
}

#[test]
#[ignore = "Debug AST structure"]
fn explore_fully_qualified_class_literal() {
    let source1 = "x = MyLib.class";
    let tree1 = parse_groovy(source1);
    println!("\n=== AST for: {} ===", source1);
    println!("{}", tree1.root_node().to_sexp());
    find_dotted(tree1.root_node(), source1.as_bytes(), 0);

    let source = "x = com.example.MyLib.class";
    let tree = parse_groovy(source);
    println!("\n=== AST for: {} ===", source);
    println!("{}", tree.root_node().to_sexp());

    // Find the dotted_identifier node
    fn find_dotted(node: tree_sitter::Node, content: &[u8], depth: usize) {
        let indent = "  ".repeat(depth);
        if node.kind() == "dotted_identifier" {
            println!(
                "{}dotted_identifier: {:?}",
                indent,
                node.utf8_text(content).unwrap()
            );
            let mut cursor = node.walk();
            for (i, child) in node.named_children(&mut cursor).enumerate() {
                println!(
                    "{}  child[{}]: kind={} text={:?}",
                    indent,
                    i,
                    child.kind(),
                    child.utf8_text(content).ok()
                );
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            find_dotted(child, content, depth + 1);
        }
    }

    find_dotted(tree.root_node(), source.as_bytes(), 0);
}
