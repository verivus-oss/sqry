/// Integration tests for R `GraphBuilder`
use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::node::NodeKind;
use sqry_lang_r::relations::RGraphBuilder;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

fn parse_r(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_r::LANGUAGE.into())
        .expect("Error loading R grammar");
    parser.parse(source, None).expect("Error parsing")
}

// ============================================================================
// Visibility Metadata Tests
// ============================================================================

/// Build a string lookup table from staging operations.
fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let mut lookup = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            lookup.insert(local_id.index(), value.clone());
        }
    }
    lookup
}

/// Find the visibility of a function by name.
fn find_function_visibility(staging: &StagingGraph, name: &str) -> Option<String> {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Function
        {
            let node_name = strings.get(&entry.name.index());
            if node_name.is_some_and(|n| n.contains(name)) {
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
    let source = r"
public_func <- function() {
    42
}
";
    let tree = parse_r(source);
    let file = Path::new("test_visibility_public.r");
    let mut staging = StagingGraph::new();
    let builder = RGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    let visibility = find_function_visibility(&staging, "public_func");
    assert_eq!(visibility, Some("public".to_string()));
}

#[test]
fn test_function_visibility_private() {
    let source = r"
.private_func <- function() {
    42
}
";
    let tree = parse_r(source);
    let file = Path::new("test_visibility_private.r");
    let mut staging = StagingGraph::new();
    let builder = RGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    let visibility = find_function_visibility(&staging, ".private_func");
    assert_eq!(visibility, Some("private".to_string()));
}

#[test]
fn test_mixed_visibility() {
    let source = r"
public_func <- function() {
    .helper()
}

.helper <- function() {
    42
}
";
    let tree = parse_r(source);
    let file = Path::new("test_mixed_visibility.r");
    let mut staging = StagingGraph::new();
    let builder = RGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    assert_eq!(
        find_function_visibility(&staging, "public_func"),
        Some("public".to_string())
    );
    assert_eq!(
        find_function_visibility(&staging, ".helper"),
        Some("private".to_string())
    );
}

// ============================================================================
// P2 Advanced Features: Class and Variable Nodes
// ============================================================================

/// Find a class node by name.
fn find_class_node(staging: &StagingGraph, name: &str) -> bool {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Class
            && let Some(node_name) = strings.get(&entry.name.index())
            && node_name.contains(name)
        {
            return true;
        }
    }
    false
}

/// Find a variable node by name.
fn find_variable_node(staging: &StagingGraph, name: &str) -> bool {
    let strings = build_string_lookup(staging);
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && entry.kind == NodeKind::Variable
            && let Some(node_name) = strings.get(&entry.name.index())
            && node_name.contains(name)
        {
            return true;
        }
    }
    false
}

#[test]
fn test_class_definition_s4() {
    let source = r#"
setClass("Person",
  slots = c(name = "character", age = "numeric")
)
"#;
    let tree = parse_r(source);
    let file = Path::new("test_class_s4.r");
    let mut staging = StagingGraph::new();
    let builder = RGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    assert!(find_class_node(&staging, "Person"), "S4 class not found");
}

#[test]
fn test_class_definition_r6() {
    let source = r#"
Person <- R6Class("Person",
  public = list(
    name = NULL,
    initialize = function(name) {
      self$name <- name
    }
  )
)
"#;
    let tree = parse_r(source);
    let file = Path::new("test_class_r6.r");
    let mut staging = StagingGraph::new();
    let builder = RGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    assert!(find_class_node(&staging, "Person"), "R6 class not found");
}

#[test]
fn test_variable_assignment_left_arrow() {
    let source = r"
x <- 42
y <- 'hello'
data_frame <- data.frame(a = 1:3, b = 4:6)
";
    let tree = parse_r(source);
    let file = Path::new("test_var_left_arrow.r");
    let mut staging = StagingGraph::new();
    let builder = RGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    assert!(find_variable_node(&staging, "x"), "Variable x not found");
    assert!(find_variable_node(&staging, "y"), "Variable y not found");
    assert!(
        find_variable_node(&staging, "data_frame"),
        "Variable data_frame not found"
    );
}

#[test]
fn test_variable_assignment_equals() {
    let source = r"
count = 10
message = 'status'
";
    let tree = parse_r(source);
    let file = Path::new("test_var_equals.r");
    let mut staging = StagingGraph::new();
    let builder = RGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    assert!(
        find_variable_node(&staging, "count"),
        "Variable count not found"
    );
    assert!(
        find_variable_node(&staging, "message"),
        "Variable message not found"
    );
}

#[test]
fn test_variable_assignment_right_arrow() {
    let source = r"
42 -> result
'test' -> label
";
    let tree = parse_r(source);
    let file = Path::new("test_var_right_arrow.r");
    let mut staging = StagingGraph::new();
    let builder = RGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    assert!(
        find_variable_node(&staging, "result"),
        "Variable result not found"
    );
    assert!(
        find_variable_node(&staging, "label"),
        "Variable label not found"
    );
}

#[test]
fn test_skip_function_assignments() {
    let source = r"
my_func <- function(x) { x + 1 }
my_var <- 42
";
    let tree = parse_r(source);
    let file = Path::new("test_skip_func_assign.r");
    let mut staging = StagingGraph::new();
    let builder = RGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    // Should NOT create a variable node for my_func (it's a function assignment)
    // Should create a variable node for my_var
    assert!(
        !find_variable_node(&staging, "my_func"),
        "Function assignment should not create variable node"
    );
    assert!(
        find_variable_node(&staging, "my_var"),
        "Variable my_var not found"
    );
}

#[test]
fn test_multiple_classes_and_variables() {
    let source = r#"
# S4 class
setClass("Animal", slots = c(name = "character"))

# R6 class
Dog <- R6Class("Dog", public = list(bark = function() {}))

# Variables
pet_count <- 3
owner_name <- "Alice"
is_active <- TRUE
"#;
    let tree = parse_r(source);
    let file = Path::new("test_multiple.r");
    let mut staging = StagingGraph::new();
    let builder = RGraphBuilder::default();

    let result = builder.build_graph(&tree, source.as_bytes(), file, &mut staging);
    assert!(result.is_ok());

    // Classes
    assert!(
        find_class_node(&staging, "Animal"),
        "Animal class not found"
    );
    assert!(find_class_node(&staging, "Dog"), "Dog class not found");

    // Variables
    assert!(
        find_variable_node(&staging, "pet_count"),
        "Variable pet_count not found"
    );
    assert!(
        find_variable_node(&staging, "owner_name"),
        "Variable owner_name not found"
    );
    assert!(
        find_variable_node(&staging, "is_active"),
        "Variable is_active not found"
    );
}
