//! AST structure inspection for grammar validation
//!
//! This test validates tree-sitter-ruby grammar assumptions for:
//! - BLOCKER 1: `command/command_call` nodes for DSL calls
//! - BLOCKER 2: method node structure and visibility
//! - BLOCKER 3: visibility modifier representation in AST

use tree_sitter::{Node, Parser, Tree};

fn parse_ruby(source: &str) -> Tree {
    let mut parser = Parser::new();
    let language = tree_sitter_ruby::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to set language");
    parser
        .parse(source.as_bytes(), None)
        .expect("Failed to parse")
}

fn print_tree(node: Node, source: &[u8], depth: usize) {
    let indent = "  ".repeat(depth);
    let kind = node.kind();

    print!("{indent}{kind}");

    // Print field name if available
    let field_name = if depth > 0 {
        // This is a simplification - in real code we'd track the field name
        ""
    } else {
        ""
    };

    if !field_name.is_empty() {
        print!(" [{field_name}]");
    }

    // Print text for leaf nodes and important nodes
    if (node.child_count() == 0
        || matches!(kind, "identifier" | "constant" | "simple_symbol" | "string"))
        && let Ok(text) = node.utf8_text(source)
    {
        let display = if text.len() > 50 {
            format!("{}...", &text[..50])
        } else {
            text.to_string()
        };
        print!(" → {display:?}");
    }

    println!(
        " @{}:{}",
        node.start_position().row,
        node.start_position().column
    );

    // Recurse
    if depth < 10 {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            print_tree(child, source, depth + 1);
        }
    }
}

fn collect_nodes_by_kind<'a>(node: Node<'a>, kind: &str, results: &mut Vec<Node<'a>>) {
    if node.kind() == kind {
        results.push(node);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_nodes_by_kind(child, kind, results);
    }
}

fn find_visibility_markers<'a>(
    node: Node<'a>,
    source: &[u8],
    private: &mut Vec<Node<'a>>,
    public: &mut Vec<Node<'a>>,
    protected: &mut Vec<Node<'a>>,
) {
    if let Ok(text) = node.utf8_text(source) {
        let text = text.trim();
        if text == "private" {
            private.push(node);
        } else if text == "public" {
            public.push(node);
        } else if text == "protected" {
            protected.push(node);
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_visibility_markers(child, source, private, public, protected);
    }
}

#[test]
fn test_blocker1_dsl_calls_command_nodes() {
    println!("\n=== BLOCKER 1: DSL-style calls (command/command_call nodes) ===\n");

    let source = r"
class UserController
  before_action :authenticate
  validates :email, presence: true
  has_many :posts
  delegate :name, to: :user
end
";

    let tree = parse_ruby(source);
    let root = tree.root_node();

    println!("Full AST:");
    print_tree(root, source.as_bytes(), 0);

    println!("\n--- Searching for 'command' nodes ---");
    let mut commands = Vec::new();
    collect_nodes_by_kind(root, "command", &mut commands);
    println!("Found {} 'command' nodes", commands.len());
    for (i, node) in commands.iter().enumerate() {
        if let Ok(text) = node.utf8_text(source.as_bytes()) {
            println!("  {}. {}", i + 1, text.trim());
        }
    }

    println!("\n--- Searching for 'command_call' nodes ---");
    let mut command_calls = Vec::new();
    collect_nodes_by_kind(root, "command_call", &mut command_calls);
    println!("Found {} 'command_call' nodes", command_calls.len());
    for (i, node) in command_calls.iter().enumerate() {
        if let Ok(text) = node.utf8_text(source.as_bytes()) {
            println!("  {}. {}", i + 1, text.trim());
        }
    }

    println!("\n--- Searching for 'call' nodes ---");
    let mut calls = Vec::new();
    collect_nodes_by_kind(root, "call", &mut calls);
    println!("Found {} 'call' nodes", calls.len());
    for (i, node) in calls.iter().enumerate() {
        if let Ok(text) = node.utf8_text(source.as_bytes()) {
            println!("  {}. {}", i + 1, text.trim());
        }
    }

    // Verify that DSL calls are captured as `call` nodes, not `command` nodes
    // (This validates that BLOCKER 1 was a false positive)
    assert_eq!(commands.len(), 0, "Unexpectedly found 'command' nodes");
    assert_eq!(
        command_calls.len(),
        0,
        "Unexpectedly found 'command_call' nodes"
    );
    assert!(
        !calls.is_empty(),
        "DSL calls should be captured as 'call' nodes"
    );
}

#[test]
fn test_blocker2_instance_method_structure() {
    println!("\n=== BLOCKER 2: Instance method structure ===\n");

    let source = r"
class User
  def authenticate
    verify_password
  end

  def self.find_by_email(email)
    query(email)
  end
end
";

    let tree = parse_ruby(source);
    let root = tree.root_node();

    println!("Full AST:");
    print_tree(root, source.as_bytes(), 0);

    println!("\n--- Searching for 'method' nodes ---");
    let mut methods = Vec::new();
    collect_nodes_by_kind(root, "method", &mut methods);
    println!("Found {} 'method' nodes", methods.len());
    for node in &methods {
        if let Ok(text) = node.utf8_text(source.as_bytes()) {
            println!("  Method: {}", text.lines().next().unwrap_or(""));
        }
    }

    println!("\n--- Searching for 'singleton_method' nodes ---");
    let mut singleton_methods = Vec::new();
    collect_nodes_by_kind(root, "singleton_method", &mut singleton_methods);
    println!("Found {} 'singleton_method' nodes", singleton_methods.len());
    for node in &singleton_methods {
        if let Ok(text) = node.utf8_text(source.as_bytes()) {
            println!("  Singleton method: {}", text.lines().next().unwrap_or(""));
        }
    }

    assert!(!methods.is_empty(), "Should find instance methods");
    assert!(
        !singleton_methods.is_empty(),
        "Should find singleton methods"
    );
}

#[test]
fn test_blocker3_visibility_modifiers() {
    println!("\n=== BLOCKER 3: Visibility modifier representation ===\n");

    let source = r"
class PaymentService
  def public_method
  end

  private

  def private_method
  end

  public

  def back_to_public
  end

  private def inline_private
  end

  def will_be_private
  end
  private :will_be_private

  protected

  def protected_method
  end
end
";

    let tree = parse_ruby(source);
    let root = tree.root_node();

    println!("Full AST:");
    print_tree(root, source.as_bytes(), 0);

    println!("\n--- Searching for visibility markers ---");

    // Look for 'private', 'protected', 'public' as identifiers or calls
    let mut private_nodes = Vec::new();
    let mut public_nodes = Vec::new();
    let mut protected_nodes = Vec::new();

    find_visibility_markers(
        root,
        source.as_bytes(),
        &mut private_nodes,
        &mut public_nodes,
        &mut protected_nodes,
    );

    println!("Found {} 'private' occurrences", private_nodes.len());
    println!("Found {} 'public' occurrences", public_nodes.len());
    println!("Found {} 'protected' occurrences", protected_nodes.len());

    for node in &private_nodes {
        println!(
            "  private at kind={} parent={}",
            node.kind(),
            node.parent().map_or("none", |p| p.kind())
        );
    }
}

#[test]
fn test_method_with_inline_visibility() {
    println!("\n=== Testing inline visibility: private def method ===\n");

    let source = r"
class Test
  private def secret
    42
  end
end
";

    let tree = parse_ruby(source);
    print_tree(tree.root_node(), source.as_bytes(), 0);
}

#[test]
fn test_post_declaration_visibility() {
    println!("\n=== Testing post-declaration: private :method ===\n");

    let source = r"
class Test
  def helper
  end
  private :helper
end
";

    let tree = parse_ruby(source);
    print_tree(tree.root_node(), source.as_bytes(), 0);
}
