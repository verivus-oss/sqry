//! Test suite for Ruby signature metadata extraction
//!
//! Validates that the Ruby plugin correctly extracts parameter signatures
//! from method definitions. Note that Ruby is dynamically typed, so we
//! focus on parameter names rather than type annotations.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_core::graph::unified::node::NodeKind;
use sqry_lang_ruby::RubyGraphBuilder;
use sqry_test_support::graph_helpers::build_string_lookup;
use std::path::Path;
use tree_sitter::Parser;

// ========== Test Helper Functions ==========

/// Parse Ruby source code into a tree-sitter Tree
fn parse_ruby(content: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    let language = tree_sitter_ruby::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to load Ruby grammar");
    parser
        .parse(content, None)
        .expect("Failed to parse Ruby code")
}

/// Build staging graph from Ruby source and return it for assertions
fn build_test_graph(content: &str, filename: &str) -> StagingGraph {
    let tree = parse_ruby(content);
    let mut staging = StagingGraph::new();
    let builder = RubyGraphBuilder::default();
    let file_path = Path::new(filename);

    builder
        .build_graph(&tree, content.as_bytes(), file_path, &mut staging)
        .expect("Failed to build graph");

    staging
}

/// Helper to find method nodes and their signatures
fn find_methods_with_signatures(staging: &StagingGraph) -> Vec<(String, Option<String>)> {
    let strings = build_string_lookup(staging);
    let mut methods = Vec::new();

    for op in staging.operations() {
        if let StagingOp::AddNode { entry, .. } = op
            && matches!(entry.kind, NodeKind::Method)
        {
            let method_name = strings
                .get(&entry.name.index())
                .map_or("<unknown>".to_string(), |s| s.to_string());
            let signature = entry
                .signature
                .and_then(|sig_id| strings.get(&sig_id.index()).map(|s| s.to_string()));
            methods.push((method_name, signature));
        }
    }

    methods
}

// ========== Tests ==========

#[test]
fn test_simple_parameters() {
    let code = r#"
        def add(x, y)
          x + y
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "add");
    assert_eq!(signature.as_deref(), Some("x, y"));
}

#[test]
fn test_single_parameter() {
    let code = r#"
        def square(n)
          n * n
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "square");
    assert_eq!(signature.as_deref(), Some("n"));
}

#[test]
fn test_no_parameters() {
    let code = r#"
        def hello
          puts "Hello"
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "hello");
    assert!(
        signature.is_none(),
        "Methods with no parameters should have no signature"
    );
}

#[test]
fn test_optional_parameters() {
    let code = r#"
        def greet(name, greeting = "Hello")
          puts greeting + name
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "greet");

    // Verify optional parameter captured with default value
    let sig = signature.as_ref().expect("Should have signature");
    assert!(sig.contains("name"), "Should contain required param");
    assert!(
        sig.contains("greeting"),
        "Should contain optional param name"
    );
    assert!(sig.contains("Hello"), "Should contain default value");
}

#[test]
fn test_splat_parameters() {
    let code = r#"
        def sum(x, *args)
          x + args.sum
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "sum");

    let sig = signature.as_ref().expect("Should have signature");
    assert!(sig.contains("x"), "Should contain regular param");
    assert!(sig.contains("*args"), "Should contain splat parameter");
}

#[test]
fn test_keyword_parameters() {
    let code = r#"
        def configure(host:, port: 8080)
          # comment
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "configure");

    let sig = signature.as_ref().expect("Should have signature");
    assert!(
        sig.contains("host:"),
        "Should contain required keyword param"
    );
    assert!(
        sig.contains("port:"),
        "Should contain optional keyword param"
    );
    assert!(sig.contains("8080"), "Should contain keyword default value");
}

#[test]
fn test_hash_splat_parameters() {
    let code = r#"
        def options(x, **kwargs)
          x
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "options");

    let sig = signature.as_ref().expect("Should have signature");
    assert!(sig.contains("x"), "Should contain regular param");
    assert!(
        sig.contains("**kwargs"),
        "Should contain hash splat parameter"
    );
}

#[test]
fn test_block_parameter() {
    let code = r#"
        def with_callback(x, &block)
          block.call(x)
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "with_callback");

    let sig = signature.as_ref().expect("Should have signature");
    assert!(sig.contains("x"), "Should contain regular param");
    assert!(sig.contains("&block"), "Should contain block parameter");
}

#[test]
fn test_complex_parameter_combination() {
    let code = r#"
        def complex(a, b = 10, *args, x:, y: 20, **kwargs, &block)
          a
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "complex");

    let sig = signature.as_ref().expect("Should have signature");
    assert!(sig.contains("a"), "Should contain required param");
    assert!(
        sig.contains("b = 10") || sig.contains("b=10"),
        "Should contain optional param with default"
    );
    assert!(sig.contains("*args"), "Should contain splat");
    assert!(sig.contains("x:"), "Should contain required keyword");
    assert!(
        sig.contains("y:") && sig.contains("20"),
        "Should contain optional keyword with default"
    );
    assert!(sig.contains("**kwargs"), "Should contain hash splat");
    assert!(sig.contains("&block"), "Should contain block");
}

#[test]
fn test_singleton_method_signature() {
    let code = r#"
        class Foo
          def self.create(name)
            new(name)
          end
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    // Find the singleton method
    let singleton = methods
        .iter()
        .find(|(name, _)| name.contains("create"))
        .expect("Should find singleton method");

    let (_, signature) = singleton;
    assert_eq!(
        signature.as_deref(),
        Some("name"),
        "Singleton methods should have signatures"
    );
}

#[test]
fn test_multiple_methods_different_signatures() {
    let code = r#"
        def foo
          1
        end

        def bar(x)
          x
        end

        def baz(x, y, z = 10)
          x + y + z
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 3);

    // Verify each method has correct signature
    let foo = methods
        .iter()
        .find(|(n, _)| n == "foo")
        .expect("Should find foo");
    assert!(foo.1.is_none(), "foo should have no signature");

    let bar = methods
        .iter()
        .find(|(n, _)| n == "bar")
        .expect("Should find bar");
    assert_eq!(bar.1.as_deref(), Some("x"));

    let baz = methods
        .iter()
        .find(|(n, _)| n == "baz")
        .expect("Should find baz");
    let baz_sig = baz.1.as_ref().expect("Should have signature");
    assert!(baz_sig.contains("x") && baz_sig.contains("y") && baz_sig.contains("z"));
}

#[test]
fn test_signature_does_not_break_existing_functionality() {
    // Ensure that adding signatures doesn't break existing call edge detection
    let code = r#"
        def caller_method(x)
          callee_method(x)
        end

        def callee_method(y)
          y * 2
        end
    "#;

    let staging = build_test_graph(code, "test.rb");

    // Should have both methods with signatures
    let methods = find_methods_with_signatures(&staging);
    assert_eq!(methods.len(), 2);

    // Verify call edges still created (check operations)
    let has_call_edge = staging.operations().iter().any(|op| {
        matches!(op, StagingOp::AddEdge { kind, .. } if matches!(kind, sqry_core::graph::unified::edge::EdgeKind::Calls { .. }))
    });
    assert!(has_call_edge, "Should still create call edges");
}

// ========== New Parameter Types Tests ==========

#[test]
fn test_destructured_parameter() {
    let code = r#"
        def process((x, y))
          x + y
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "process");

    let sig = signature.as_ref().expect("Should have signature");
    assert!(
        sig.contains("(x, y)"),
        "Should contain destructured parameter, got: {}",
        sig
    );
}

#[test]
fn test_forward_parameter() {
    let code = r#"
        def wrapper(...)
          target(...)
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "wrapper");

    let sig = signature.as_ref().expect("Should have signature");
    assert!(
        sig.contains("..."),
        "Should contain forward parameter, got: {}",
        sig
    );
}

#[test]
fn test_hash_splat_nil() {
    let code = r#"
        def strict(x, **nil)
          x
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "strict");

    let sig = signature.as_ref().expect("Should have signature");
    assert!(sig.contains("x"), "Should contain regular param");
    assert!(
        sig.contains("**nil"),
        "Should contain hash splat nil, got: {}",
        sig
    );
}

// ========== Return Type Extraction Tests ==========

#[test]
fn test_sorbet_return_type() {
    let code = r#"
        sig { returns(String) }
        def get_name
          "John"
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "get_name");

    let sig = signature.as_ref().expect("Should have signature");
    assert!(
        sig.contains("-> String"),
        "Should have Sorbet return type, got: {}",
        sig
    );
}

#[test]
fn test_rbs_return_type() {
    let code = r#"
        def get_count #: () -> Integer
          42
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "get_count");

    let sig = signature.as_ref().expect("Should have signature");
    assert!(
        sig.contains("-> Integer"),
        "Should have RBS return type, got: {}",
        sig
    );
}

#[test]
fn test_yard_return_type() {
    let code = r#"
        # Get the user's email
        # @return [String] the email address
        def get_email
          "user@example.com"
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "get_email");

    let sig = signature.as_ref().expect("Should have signature");
    assert!(
        sig.contains("-> String"),
        "Should have YARD return type, got: {}",
        sig
    );
}

// ========== Combined Signature Tests ==========

#[test]
fn test_parameters_with_return_type() {
    let code = r#"
        sig { params(x: Integer, y: Integer).returns(Integer) }
        def add(x, y)
          x + y
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "add");

    let sig = signature.as_ref().expect("Should have signature");
    assert!(sig.contains("x"), "Should contain parameter x");
    assert!(sig.contains("y"), "Should contain parameter y");
    assert!(
        sig.contains("-> Integer"),
        "Should contain return type, got: {}",
        sig
    );
}

#[test]
fn test_return_type_only_no_params() {
    let code = r#"
        sig { returns(Boolean) }
        def valid?
          true
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "valid?");

    let sig = signature.as_ref().expect("Should have signature");
    assert_eq!(sig, "-> Boolean", "Should have return type only");
}

#[test]
fn test_complex_signature_with_all_features() {
    let code = r#"
        sig { params(a: Integer, b: String, args: T.untyped, kwargs: T.untyped, block: T.proc.void).returns(Hash) }
        def complex(a, b = "default", *args, x:, y: 20, **kwargs, &block)
          {}
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "complex");

    let sig = signature.as_ref().expect("Should have signature");
    // Check parameters
    assert!(sig.contains("a"), "Should contain param a");
    assert!(sig.contains("b"), "Should contain param b");
    assert!(sig.contains("*args"), "Should contain splat");
    assert!(sig.contains("x:"), "Should contain keyword x");
    assert!(sig.contains("y:"), "Should contain keyword y");
    assert!(sig.contains("**kwargs"), "Should contain hash splat");
    assert!(sig.contains("&block"), "Should contain block");
    // Check return type
    assert!(
        sig.contains("-> Hash"),
        "Should contain return type, got: {}",
        sig
    );
}

// ========== Negative Tests (Edge Cases) ==========

#[test]
fn test_rbs_requires_prefix() {
    // Regular comment with -> should NOT be treated as RBS
    let code = r#"
        def process # This arrow -> is just a comment
          42
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "process");

    // Should have NO return type (regular comment ignored)
    if let Some(sig) = signature {
        assert!(
            !sig.contains("->"),
            "Regular comment with -> should not be treated as RBS return type, got: {}",
            sig
        );
    }
}

#[test]
fn test_yard_requires_adjacency() {
    // YARD comment separated by blank line should NOT be used
    let code = r#"
        # @return [String] old comment from previous method

        def unrelated
          42
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "unrelated");

    // Should have NO return type (comment not adjacent)
    if let Some(sig) = signature {
        assert!(
            !sig.contains("->"),
            "Non-adjacent YARD comment should not be used, got: {}",
            sig
        );
    }
}

#[test]
fn test_annotation_precedence_sorbet_wins() {
    // When all three sources present, Sorbet should take precedence
    let code = r#"
        # @return [WrongType] this is YARD
        sig { returns(CorrectType) }
        def get_value #: () -> AlsoWrong
          42
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "get_value");

    let sig = signature.as_ref().expect("Should have signature");
    assert!(
        sig.contains("-> CorrectType"),
        "Sorbet should take precedence over RBS and YARD, got: {}",
        sig
    );
    assert!(
        !sig.contains("WrongType") && !sig.contains("AlsoWrong"),
        "Should not contain YARD or RBS types when Sorbet present, got: {}",
        sig
    );
}

#[test]
fn test_rbs_nested_proc_types() {
    // RBS with nested proc types (contains multiple ->)
    let code = r#"
        def transform #: (Proc[String, Integer]) -> Boolean
          true
        end
    "#;

    let staging = build_test_graph(code, "test.rb");
    let methods = find_methods_with_signatures(&staging);

    assert_eq!(methods.len(), 1);
    let (name, signature) = &methods[0];
    assert_eq!(name, "transform");

    let sig = signature.as_ref().expect("Should have signature");
    assert!(
        sig.contains("-> Boolean"),
        "Should extract top-level return type (Boolean), got: {}",
        sig
    );
    assert!(
        !sig.contains("-> Integer"),
        "Should not extract nested proc return type, got: {}",
        sig
    );
}
