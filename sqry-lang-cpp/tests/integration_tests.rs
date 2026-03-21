//! Integration tests for C++ language plugin (graph-native).
//!
//! Validates:
//! - Class/struct/enum/function/method node extraction
//! - Qualified names across namespaces
//! - Static method flags
//! - Inheritance or interface implementation edges

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::NodeEntry;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_cpp::CppPlugin;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

fn resolved_node_name(entry: &NodeEntry, strings: &HashMap<u32, String>) -> String {
    entry
        .qualified_name
        .and_then(|qualified_name_id| strings.get(&qualified_name_id.index()).cloned())
        .or_else(|| strings.get(&entry.name.index()).cloned())
        .unwrap_or_default()
}

fn build_node_lookup(staging: &StagingGraph) -> HashMap<NodeId, (String, NodeKind)> {
    let strings = build_string_lookup(staging);
    let mut nodes = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode {
            entry,
            expected_id: Some(node_id),
        } = op
        {
            let name = resolved_node_name(entry, &strings);
            nodes.insert(*node_id, (name, entry.kind));
        }
    }
    nodes
}

fn find_node_entry<'a>(
    staging: &'a StagingGraph,
    name: &str,
    kind: NodeKind,
) -> Option<&'a NodeEntry> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == kind
            && resolved_node_name(entry, &strings) == name
        {
            return Some(entry);
        }
    }
    None
}

fn find_node_id(staging: &StagingGraph, name: &str, kind: NodeKind) -> Option<NodeId> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode {
            entry,
            expected_id: Some(node_id),
        } = op
            && entry.kind == kind
            && resolved_node_name(entry, &strings) == name
        {
            return Some(*node_id);
        }
    }
    None
}

fn build_graph(file: &PathBuf) -> StagingGraph {
    let plugin = CppPlugin::default();
    let content = fs::read(file).expect("read test source");
    let tree = plugin.parse_ast(&content).expect("parse C++ source");
    let mut staging = StagingGraph::new();
    let builder = plugin.graph_builder().expect("graph builder");

    builder
        .build_graph(&tree, &content, file, &mut staging)
        .expect("build graph");

    staging
}

fn build_graph_from_source(source: &str) -> StagingGraph {
    let dir = TempDir::new().expect("temp dir");
    let file = dir.path().join("test.cpp");
    fs::write(&file, source).expect("write test source");
    build_graph(&file)
}

#[test]
fn test_simple_class() {
    let staging = build_graph_from_source(
        r#"
class MyClass {
public:
    void publicMethod() {}
private:
    void privateMethod() {}
protected:
    void protectedMethod() {}
};
"#,
    );

    assert!(
        find_node_entry(&staging, "MyClass", NodeKind::Class).is_some(),
        "MyClass class node not found"
    );
}

#[test]
fn test_virtual_methods_and_inheritance() {
    let staging = build_graph_from_source(
        r#"
class Base {
public:
    virtual void process() {}
    virtual int compute() const { return 0; }
};

class Derived : public Base {
public:
    void process() override {}
    int compute() const override final { return 1; }
};
"#,
    );

    assert!(
        find_node_entry(&staging, "Base", NodeKind::Class).is_some(),
        "Base class not found"
    );
    assert!(
        find_node_entry(&staging, "Derived", NodeKind::Class).is_some(),
        "Derived class not found"
    );
    assert!(
        find_node_entry(&staging, "Base::process", NodeKind::Method).is_some(),
        "Base::process method not found"
    );
    assert!(
        find_node_entry(&staging, "Derived::process", NodeKind::Method).is_some(),
        "Derived::process method not found"
    );

    let nodes = build_node_lookup(&staging);
    let mut inherits = false;
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
            && matches!(kind, EdgeKind::Inherits)
        {
            let source_name = nodes.get(source).map(|(name, _)| name.as_str());
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            if source_name == Some("Derived") && target_name == Some("Base") {
                inherits = true;
            }
        }
    }
    assert!(inherits, "Expected Derived to inherit Base");
}

#[test]
fn test_template_class() {
    let staging = build_graph_from_source(
        r#"
template <typename T>
class Container {
public:
    void add(T item) {}
    T get(int index) { return T{}; }
};

template <typename K, typename V>
class Map {
public:
    void insert(K key, V value) {}
};
"#,
    );

    assert!(
        find_node_entry(&staging, "Container", NodeKind::Class).is_some(),
        "Container class node not found"
    );
    assert!(
        find_node_entry(&staging, "Map", NodeKind::Class).is_some(),
        "Map class node not found"
    );
}

#[test]
fn test_template_function() {
    let staging = build_graph_from_source(
        r#"
template <typename T>
T max(T a, T b) {
    return a > b ? a : b;
}

template <typename T>
void swap(T& a, T& b) {
    T temp = a;
    a = b;
    b = temp;
}
"#,
    );

    assert!(
        find_node_entry(&staging, "max", NodeKind::Function).is_some(),
        "max function node not found"
    );
    assert!(
        find_node_entry(&staging, "swap", NodeKind::Function).is_some(),
        "swap function node not found"
    );
}

#[test]
fn test_namespace_qualified_names() {
    let staging = build_graph_from_source(
        r#"
namespace utils {
    void helper() {}

    namespace math {
        int add(int a, int b) { return a + b; }
    }
}

namespace net {
    class Socket {
    public:
        void open() {}
    };
}
"#,
    );

    assert!(
        find_node_entry(&staging, "utils::helper", NodeKind::Function).is_some(),
        "utils::helper function node not found"
    );
    assert!(
        find_node_entry(&staging, "utils::math::add", NodeKind::Function).is_some(),
        "utils::math::add function node not found"
    );
    assert!(
        find_node_entry(&staging, "net::Socket", NodeKind::Class).is_some(),
        "net::Socket class node not found"
    );
}

#[test]
fn test_static_methods() {
    let staging = build_graph_from_source(
        r#"
class Utility {
public:
    static int getValue() { return 42; }
    static void setValue(int val) { (void)val; }
};
"#,
    );

    let get_value = find_node_entry(&staging, "Utility::getValue", NodeKind::Method)
        .expect("Utility::getValue method not found");
    assert!(get_value.is_static, "getValue should be static");

    let set_value = find_node_entry(&staging, "Utility::setValue", NodeKind::Method)
        .expect("Utility::setValue method not found");
    assert!(set_value.is_static, "setValue should be static");
}

#[test]
fn test_inline_functions() {
    let staging = build_graph_from_source(
        r#"
inline int square(int x) {
    return x * x;
}

inline double cube(double x) {
    return x * x * x;
}

class Math {
public:
    inline int add(int a, int b) {
        return a + b;
    }
};
"#,
    );

    assert!(
        find_node_entry(&staging, "square", NodeKind::Function).is_some(),
        "square function node not found"
    );
    assert!(
        find_node_entry(&staging, "cube", NodeKind::Function).is_some(),
        "cube function node not found"
    );
    assert!(
        find_node_entry(&staging, "Math::add", NodeKind::Method).is_some(),
        "Math::add method node not found"
    );
}

#[test]
fn test_enum_types() {
    let staging = build_graph_from_source(
        r#"
enum Color {
    RED,
    GREEN,
    BLUE
};

enum class Status {
    SUCCESS,
    FAILURE,
    PENDING
};
"#,
    );

    assert!(
        find_node_entry(&staging, "Color", NodeKind::Enum).is_some(),
        "Color enum node not found"
    );
    assert!(
        find_node_entry(&staging, "Status", NodeKind::Enum).is_some(),
        "Status enum node not found"
    );
}

#[test]
fn test_enum_export_edge() {
    let staging = build_graph_from_source(
        r#"
enum Color {
    RED,
    GREEN,
    BLUE
};
"#,
    );

    let enum_id =
        find_node_id(&staging, "Color", NodeKind::Enum).expect("Color enum node id not found");
    let mut has_export = false;
    for op in staging.operations() {
        if let StagingOp::AddEdge { target, kind, .. } = op
            && matches!(kind, EdgeKind::Exports { .. })
            && *target == enum_id
        {
            has_export = true;
            break;
        }
    }

    assert!(has_export, "File-level enum should have export edge");
}

#[test]
fn test_struct_definition() {
    let staging = build_graph_from_source(
        r#"
struct Point {
    int x;
    int y;
};

struct Node {
    int data;
    Node* next;
};
"#,
    );

    assert!(
        find_node_entry(&staging, "Point", NodeKind::Struct).is_some(),
        "Point struct node not found"
    );
    assert!(
        find_node_entry(&staging, "Node", NodeKind::Struct).is_some(),
        "Node struct node not found"
    );
}

#[test]
fn test_constructor_destructor() {
    let staging = build_graph_from_source(
        r#"
class Resource {
public:
    Resource();
    explicit Resource(int id);
    ~Resource();
private:
    int id_;
};

Resource::Resource() : id_(0) {}
Resource::Resource(int id) : id_(id) {}
Resource::~Resource() {}
"#,
    );

    assert!(
        find_node_entry(&staging, "Resource", NodeKind::Class).is_some(),
        "Resource class node not found"
    );
    assert!(
        find_node_entry(&staging, "Resource::Resource", NodeKind::Method).is_some(),
        "Resource constructor method not found"
    );
    assert!(
        find_node_entry(&staging, "Resource::~Resource", NodeKind::Method).is_some(),
        "Resource destructor method not found"
    );
}

#[test]
fn test_complex_cpp_features() {
    let staging = build_graph_from_source(
        r#"
namespace shapes {
    class Shape {
    public:
        virtual double area() const = 0;
        virtual ~Shape() {}
    };

    class Circle : public Shape {
    public:
        Circle(double r) : radius(r) {}
        double area() const override { return 3.14 * radius * radius; }
    private:
        double radius;
    };

    template <typename T>
    class Container {
    public:
        void add(T item) {}
        T get(int index) const { return T{}; }
    private:
        T* data;
    };
}
"#,
    );

    assert!(
        find_node_entry(&staging, "shapes::Shape", NodeKind::Class).is_some(),
        "shapes::Shape class node not found"
    );
    assert!(
        find_node_entry(&staging, "shapes::Circle", NodeKind::Class).is_some(),
        "shapes::Circle class node not found"
    );
    assert!(
        find_node_entry(&staging, "shapes::Container", NodeKind::Class).is_some(),
        "shapes::Container class node not found"
    );

    let nodes = build_node_lookup(&staging);
    let mut relation_found = false;
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
            && matches!(kind, EdgeKind::Implements | EdgeKind::Inherits)
        {
            let source_name = nodes.get(source).map(|(name, _)| name.as_str());
            let target_name = nodes.get(target).map(|(name, _)| name.as_str());
            if source_name == Some("shapes::Circle") && target_name == Some("shapes::Shape") {
                relation_found = true;
            }
        }
    }

    assert!(
        relation_found,
        "Expected Circle to relate to Shape via Implements or Inherits"
    );
}
#[test]
fn debug_constructor() {
    use sqry_core::graph::unified::build::staging::StagingOp;
    use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
    use sqry_lang_cpp::relations::CppGraphBuilder;
    use std::collections::HashMap;
    use std::path::Path;
    use tree_sitter::Parser;

    let source = r#"
class Resource {
public:
    Resource();
    ~Resource();
};

Resource::Resource() {}
Resource::~Resource() {}
"#;

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source.as_bytes(), None).unwrap();

    let mut staging = StagingGraph::new();
    let builder = CppGraphBuilder::new();
    let file = Path::new("test.cpp");

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .unwrap();

    // Build string map
    let mut string_map = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            string_map.insert(*local_id, value.clone());
        }
    }

    // Print all nodes
    println!("\nAll nodes:");
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && let Some(name) = string_map.get(&entry.name)
        {
            println!("  {:?}: {}", entry.kind, name);
        }
    }
    println!();
}
