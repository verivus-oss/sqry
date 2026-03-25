//! Test suite for TypeOf edges and Signature metadata in Python plugin
//!
//! Validates that the Python plugin correctly creates:
//! - TypeOf edges for type-hinted parameters and variables
//! - Signature metadata for functions/methods with return type annotations

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_lang_python::PythonGraphBuilder;
use std::path::PathBuf;
use tree_sitter::Tree;

// ========== Test Helper Functions ==========

/// Parse Python source code into a tree-sitter Tree
fn parse_python(content: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_python::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to load Python grammar");
    parser
        .parse(content, None)
        .expect("Failed to parse Python code")
}

/// Build staging graph from Python source and return it for assertions
fn build_staging_graph(content: &str, filename: &str) -> StagingGraph {
    let tree = parse_python(content);
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();
    let file_path = PathBuf::from(filename);

    builder
        .build_graph(&tree, content.as_bytes(), &file_path, &mut staging)
        .expect("Failed to build graph");

    staging
}

/// Count edges of a specific kind in staging operations
fn count_edge_kind(staging: &StagingGraph, kind_tag: &str) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                kind.tag() == kind_tag
            } else {
                false
            }
        })
        .count()
}

/// Check if staging has an edge of a specific kind
fn has_edge_kind(staging: &StagingGraph, kind_tag: &str) -> bool {
    count_edge_kind(staging, kind_tag) > 0
}

/// Check if staging has a node with a signature
fn has_node_with_signature(staging: &StagingGraph) -> bool {
    staging.operations().iter().any(|op| {
        if let StagingOp::AddNode { entry, .. } = op {
            entry.signature.is_some()
        } else {
            false
        }
    })
}

/// Count variable nodes (for scoping verification)
fn count_variable_nodes(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                matches!(entry.kind, sqry_core::schema::NodeKind::Variable)
            } else {
                false
            }
        })
        .count()
}

/// Count type nodes
fn count_type_nodes(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                matches!(entry.kind, sqry_core::schema::NodeKind::Type)
            } else {
                false
            }
        })
        .count()
}

/// Verify that type-annotation Reference edges match TypeOf edges in count.
///
/// Local variable tracking also creates References edges (from usage to declaration),
/// so we count only References edges where the target is a Type node.
fn typeof_and_reference_counts_match(staging: &StagingGraph) -> bool {
    let typeof_count = count_edge_kind(staging, "type_of");
    let type_ref_count = count_type_reference_edges(staging);
    typeof_count == type_ref_count && typeof_count > 0
}

/// Count References edges that target a Type node (type-annotation references).
///
/// This distinguishes type-annotation references from local variable references
/// created by the scope tree.
fn count_type_reference_edges(staging: &StagingGraph) -> usize {
    use std::collections::HashSet;

    // Collect node IDs of Type nodes
    let type_node_ids: HashSet<sqry_core::graph::unified::node::NodeId> = staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode {
                entry, expected_id, ..
            } = op
            {
                if matches!(entry.kind, sqry_core::schema::NodeKind::Type) {
                    *expected_id
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    // Count References edges where the target is a Type node
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { target, kind, .. } = op {
                kind.tag() == "references" && type_node_ids.contains(target)
            } else {
                false
            }
        })
        .count()
}

// ============================================================================
// TypeOf Edge Tests - Parameters
// ============================================================================

#[test]
fn test_typed_parameter_simple() {
    let source = r#"
def process(x: int, y: str):
    pass
"#;

    let staging = build_staging_graph(source, "test.py");

    // Should have 2 TypeOf edges (one for each typed parameter)
    assert_eq!(count_edge_kind(&staging, "type_of"), 2);
}

#[test]
fn test_typed_parameter_generic() {
    let source = r#"
from typing import List, Optional

def process(items: List[str], user: Optional[User]):
    pass
"#;

    let staging = build_staging_graph(source, "test.py");

    // Should have 2 TypeOf edges (generic parameters, base types extracted)
    assert_eq!(count_edge_kind(&staging, "type_of"), 2);
}

#[test]
fn test_typed_parameter_skips_self_cls() {
    let source = r#"
class MyClass:
    def method(self, x: int):
        pass

    @classmethod
    def class_method(cls, y: str):
        pass
"#;

    let staging = build_staging_graph(source, "test.py");

    // Should have 2 TypeOf edges (self and cls are skipped)
    assert_eq!(count_edge_kind(&staging, "type_of"), 2);
}

#[test]
fn test_untyped_parameter_no_typeof() {
    let source = r#"
def process(x, y, z):
    pass
"#;

    let staging = build_staging_graph(source, "test.py");

    // No TypeOf edges for untyped parameters
    assert_eq!(count_edge_kind(&staging, "type_of"), 0);
}

// ============================================================================
// TypeOf Edge Tests - Annotated Assignments (Variables)
// ============================================================================

#[test]
fn test_annotated_assignment_simple() {
    let source = r#"
user: User = get_user()
count: int = 42
"#;

    let staging = build_staging_graph(source, "test.py");

    // Should have 2 TypeOf edges
    assert_eq!(count_edge_kind(&staging, "type_of"), 2);
}

#[test]
fn test_annotated_assignment_generic() {
    let source = r#"
from typing import List, Dict, Optional

items: List[str] = []
mapping: Dict[str, int] = {}
result: Optional[User] = None
"#;

    let staging = build_staging_graph(source, "test.py");

    // Should have 3 TypeOf edges (base types extracted)
    assert_eq!(count_edge_kind(&staging, "type_of"), 3);
}

#[test]
fn test_annotated_assignment_class_attribute() {
    let source = r#"
class Service:
    repository: UserRepository
    cache: Cache[str] = None
"#;

    let staging = build_staging_graph(source, "test.py");

    // Should have 2 TypeOf edges for class attributes
    assert_eq!(count_edge_kind(&staging, "type_of"), 2);
}

#[test]
fn test_unannotated_assignment_no_typeof() {
    let source = r#"
user = get_user()
count = 42
items = []
"#;

    let staging = build_staging_graph(source, "test.py");

    // No TypeOf edges for unannotated assignments
    assert_eq!(count_edge_kind(&staging, "type_of"), 0);
}

// ============================================================================
// Signature Tests - Return Types
// ============================================================================

#[test]
fn test_function_with_return_type() {
    let source = r#"
def find_user(id: int) -> User:
    pass

async def fetch_data() -> List[str]:
    pass
"#;

    let staging = build_staging_graph(source, "test.py");

    // Should have signature metadata for functions with return types
    assert!(has_node_with_signature(&staging));
}

#[test]
fn test_method_with_return_type() {
    let source = r#"
class UserService:
    def find(self, id: int) -> Optional[User]:
        pass

    async def fetch_all(self) -> List[User]:
        pass
"#;

    let staging = build_staging_graph(source, "test.py");

    // Should have signature metadata for methods with return types
    assert!(has_node_with_signature(&staging));

    // Should also have TypeOf edges for the typed parameter
    assert!(has_edge_kind(&staging, "type_of"));
}

#[test]
fn test_function_without_return_type() {
    let source = r#"
def process(x):
    pass
"#;

    let staging = build_staging_graph(source, "test.py");

    // No signature metadata for untyped function
    // Note: This tests that functions without return types still work
    // We just check that the graph builds successfully
    assert!(!staging.operations().is_empty());
}

// ============================================================================
// Integration Tests - Combined TypeOf and Signature
// ============================================================================

#[test]
fn test_comprehensive_type_hints() {
    let source = r#"
from typing import List, Optional

class UserService:
    repository: UserRepository

    def find(self, id: int) -> Optional[User]:
        user: Optional[User] = self.repository.find_by_id(id)
        return user

    async def find_all(self, limit: int) -> List[User]:
        results: List[User] = await self.repository.query(limit)
        return results
"#;

    let staging = build_staging_graph(source, "test.py");

    // TypeOf edges:
    // - repository attribute: UserRepository
    // - find param id: int
    // - find var user: Optional
    // - find_all param limit: int
    // - find_all var results: List
    assert_eq!(count_edge_kind(&staging, "type_of"), 5);

    // Should have signature metadata for methods with return types
    assert!(has_node_with_signature(&staging));
}

// ============================================================================
// Limitation Tests - Document Dynamic Typing
// ============================================================================

#[test]
fn test_limitation_untyped_code() {
    let source = r#"
def process(data):
    result = transform(data)
    return result
"#;

    let staging = build_staging_graph(source, "test.py");

    // No TypeOf edges for untyped code (dynamic typing limitation)
    assert_eq!(count_edge_kind(&staging, "type_of"), 0);

    // No signature metadata
    assert!(!has_node_with_signature(&staging));
}

// ============================================================================
// Enhanced Validation Tests - Edge Targets and Reference Edges
// ============================================================================

#[test]
fn test_reference_edges_created_with_typeof() {
    let source = r#"
def process(x: int):
    pass
"#;

    let staging = build_staging_graph(source, "test.py");

    // Should have TypeOf edges and at least as many Reference edges
    // (local variable tracking adds additional References edges)
    assert_eq!(count_edge_kind(&staging, "type_of"), 1);
    assert!(count_edge_kind(&staging, "references") >= 1);
    assert!(typeof_and_reference_counts_match(&staging));
}

#[test]
fn test_scope_qualified_parameter_names() {
    let source = r#"
def func1(x: int):
    pass

def func2(x: str):
    pass
"#;

    let staging = build_staging_graph(source, "test.py");

    // Parameters should create separate variable nodes (scope-qualified to prevent collisions)
    // At least 2 type-annotated variable nodes; local var tracking may add more
    assert!(count_variable_nodes(&staging) >= 2);
    assert_eq!(count_type_nodes(&staging), 2);

    // Each parameter should have its own TypeOf edge
    assert_eq!(count_edge_kind(&staging, "type_of"), 2);
}

#[test]
fn test_scope_qualified_local_variables() {
    let source = r#"
def func1():
    x: int = 1

def func2():
    x: str = "hello"
"#;

    let staging = build_staging_graph(source, "test.py");

    // Local variables should create separate nodes (scope-qualified)
    assert_eq!(count_variable_nodes(&staging), 2);
    assert_eq!(count_type_nodes(&staging), 2);

    // Each variable should have its own TypeOf edge
    assert_eq!(count_edge_kind(&staging, "type_of"), 2);
}

#[test]
fn test_class_attribute_naming() {
    let source = r#"
class MyClass:
    repo: Repository
    cache: Cache
"#;

    let staging = build_staging_graph(source, "test.py");

    // Class attributes should create separate variable nodes
    assert_eq!(count_variable_nodes(&staging), 2);
    assert_eq!(count_type_nodes(&staging), 2);

    // Each attribute should have its own TypeOf edge
    assert_eq!(count_edge_kind(&staging, "type_of"), 2);
}

#[test]
fn test_forward_reference_normalization() {
    let source = r#"
def find(id: int) -> "User":
    user: "User" = get_user(id)
    return user
"#;

    let staging = build_staging_graph(source, "test.py");

    // Forward references should be normalized (quotes stripped)
    // Both the parameter and variable should reference the same "User" type node
    // At least 2 type-annotated variable nodes; local var tracking adds more
    assert!(count_variable_nodes(&staging) >= 2);
    assert_eq!(count_type_nodes(&staging), 2);

    // Should have TypeOf edges for both
    assert_eq!(count_edge_kind(&staging, "type_of"), 2);

    // Should have signature with normalized return type (verified by checking node with signature exists)
    assert!(has_node_with_signature(&staging));
}

#[test]
fn test_pep604_union_normalization() {
    let source = r#"
def process(x: int | None, y: str | int):
    result: User | None = get_user()
"#;

    let staging = build_staging_graph(source, "test.py");

    // PEP 604 unions should extract left-most base type
    // At least 3 type-annotated variable nodes; local var tracking adds more
    assert!(count_variable_nodes(&staging) >= 3);
    assert_eq!(count_type_nodes(&staging), 3);

    // Each should have TypeOf edges
    assert_eq!(count_edge_kind(&staging, "type_of"), 3);
}

#[test]
fn test_generic_type_base_extraction() {
    let source = r#"
from typing import List, Dict, Optional

def process(items: List[str], mapping: Dict[str, int], user: Optional[User]):
    pass
"#;

    let staging = build_staging_graph(source, "test.py");

    // Generic types should extract base type
    // At least 3 type-annotated variable nodes; local var tracking adds more
    assert!(count_variable_nodes(&staging) >= 3);
    assert_eq!(count_type_nodes(&staging), 3);

    // Each should have TypeOf edges
    assert_eq!(count_edge_kind(&staging, "type_of"), 3);
}

#[test]
fn test_return_type_signature_validation() {
    let source = r#"
def simple() -> int:
    pass

def generic() -> List[User]:
    pass

def union() -> User | None:
    pass

def forward() -> "User":
    pass
"#;

    let staging = build_staging_graph(source, "test.py");

    // Verify all functions have signature metadata for their return types
    // Count function nodes with signatures (should be 4)
    let functions_with_signatures = staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                matches!(entry.kind, sqry_core::schema::NodeKind::Function)
                    && entry.signature.is_some()
            } else {
                false
            }
        })
        .count();

    assert_eq!(functions_with_signatures, 4);
}

#[test]
fn test_method_parameter_scope_qualification() {
    let source = r#"
class Service:
    def method1(self, x: int):
        pass

    def method2(self, x: str):
        pass
"#;

    let staging = build_staging_graph(source, "test.py");

    // Method parameters should be scope-qualified with full method name
    // At least 2 type-annotated variable nodes; local var tracking adds more
    assert!(count_variable_nodes(&staging) >= 2);
    assert_eq!(count_type_nodes(&staging), 2);

    // Each should have TypeOf edges
    assert_eq!(count_edge_kind(&staging, "type_of"), 2);
}

#[test]
fn test_no_cross_scope_contamination() {
    let source = r#"
def func1(x: int):
    y: str = "hello"

def func2(x: float):
    y: bool = True
"#;

    let staging = build_staging_graph(source, "test.py");

    // Verify each parameter/variable creates separate nodes (no contamination)
    // At least 4 type-annotated variable nodes; local var tracking adds more
    assert!(count_variable_nodes(&staging) >= 4);
    assert_eq!(count_type_nodes(&staging), 4);

    // Verify we have exactly 4 TypeOf edges (not shared/contaminated)
    assert_eq!(count_edge_kind(&staging, "type_of"), 4);

    // Verify type-annotation reference edges match typeof edges
    assert!(typeof_and_reference_counts_match(&staging));
}

#[test]
fn test_comprehensive_reference_edge_coverage() {
    let source = r#"
def func(x: int, y: str) -> bool:
    result: bool = x > 0
    return result

class MyClass:
    attr: str

    def method(self, param: int):
        local: int = param
"#;

    let staging = build_staging_graph(source, "test.py");

    // All TypeOf edges should have corresponding type-annotation Reference edges
    assert!(typeof_and_reference_counts_match(&staging));

    // At least 6 type-annotated variable nodes; local var tracking adds more
    assert!(count_variable_nodes(&staging) >= 6);
    assert_eq!(count_edge_kind(&staging, "type_of"), 6);
    // References include both type annotations and local var references
    assert!(count_edge_kind(&staging, "references") >= 6);
}
