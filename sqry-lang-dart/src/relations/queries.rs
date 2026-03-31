//! Tree-sitter queries for Dart graph extraction.
//!
//! Defines queries for:
//! - Widget class detection (`StatelessWidget`, `StatefulWidget`, etc.)
//! - `MethodChannel` constructor detection
//! - Widget child relationships in build methods

use sqry_core::graph::{GraphBuilderError, GraphResult, Span};
use tree_sitter::{Language, Query};

/// Container for all Dart graph extraction queries.
#[derive(Debug)]
pub struct DartQueries {
    /// Query for widget class definitions (classes extending Widget types).
    pub widget_classes: Query,
    /// Query for ALL class definitions (not just widgets).
    pub generic_classes: Query,
    /// Query for ALL functions (top-level and methods).
    pub functions: Query,
    /// Query for `MethodChannel` constructor invocations.
    pub method_channels: Query,
    /// Query for `MethodChannel` field declarations.
    pub channel_fields: Query,
    /// Query for `MethodChannel.invokeMethod()` calls.
    pub channel_invocations: Query,
    /// Query for widget child instantiation in build methods.
    pub widget_children: Query,
}

impl DartQueries {
    /// Create new `DartQueries` by compiling all query patterns.
    ///
    /// # Errors
    /// Returns error if any query fails to compile against the language grammar.
    pub fn new(language: &Language) -> GraphResult<Self> {
        let widget_classes = Query::new(language, WIDGET_CLASS_QUERY).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: format!("Failed to compile widget_classes query: {e}"),
            }
        })?;

        let generic_classes = Query::new(language, GENERIC_CLASS_QUERY).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: format!("Failed to compile generic_classes query: {e}"),
            }
        })?;

        let functions =
            Query::new(language, FUNCTION_QUERY).map_err(|e| GraphBuilderError::ParseError {
                span: Span::default(),
                reason: format!("Failed to compile functions query: {e}"),
            })?;

        let method_channels = Query::new(language, METHOD_CHANNEL_QUERY).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: format!("Failed to compile method_channels query: {e}"),
            }
        })?;

        let channel_fields = Query::new(language, CHANNEL_FIELD_QUERY).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: format!("Failed to compile channel_fields query: {e}"),
            }
        })?;

        let channel_invocations = Query::new(language, CHANNEL_INVOCATION_QUERY).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: format!("Failed to compile channel_invocations query: {e}"),
            }
        })?;

        let widget_children = Query::new(language, WIDGET_CHILD_QUERY).map_err(|e| {
            GraphBuilderError::ParseError {
                span: Span::default(),
                reason: format!("Failed to compile widget_children query: {e}"),
            }
        })?;

        Ok(Self {
            widget_classes,
            generic_classes,
            functions,
            method_channels,
            channel_fields,
            channel_invocations,
            widget_children,
        })
    }
}

/// Query for widget class definitions.
///
/// Matches classes that extend `StatelessWidget`, `StatefulWidget`, or `InheritedWidget`.
/// Uses the superclass field to identify widget base classes.
///
/// Captures:
/// - @`class_name`: The name of the widget class
/// - @`base_class`: The superclass `type_identifier` (e.g., "`StatelessWidget`")
/// - @`widget_def`: The entire `class_definition` node
const WIDGET_CLASS_QUERY: &str = r#"
(class_definition
  name: (identifier) @class_name
  superclass: (superclass
    (type_identifier) @base_class
    (#match? @base_class "(Stateless|Stateful|Inherited)Widget"))) @widget_def
"#;

/// Query for ALL class definitions (not just widgets).
///
/// Matches all class definitions regardless of superclass.
/// This is used in a second pass to catch non-widget classes.
///
/// Captures:
/// - @`class_name`: The name of the class
/// - @`class_def`: The entire `class_definition` node
const GENERIC_CLASS_QUERY: &str = r"
(class_definition
  name: (identifier) @class_name
) @class_def
";

/// Query for ALL functions (top-level and methods).
///
/// Matches both lambda expressions (top-level functions) and method signatures (class methods).
/// In Dart's tree-sitter grammar:
/// - Top-level functions: `lambda_expression` with `function_signature` parameters
/// - Class methods: `method_signature` wrapping a `function_signature`
///
/// Captures:
/// - @`function_name`: The name of the function or method
/// - @`function_def`: The entire function/method node
const FUNCTION_QUERY: &str = r"
[
  ; Top-level functions (lambda_expression with function_signature)
  (lambda_expression
    parameters: (function_signature
      name: (identifier) @function_name
    )
  ) @function_def

  ; Class methods (class_member_definition containing method and body)
  (class_member_definition
    (method_signature
      (function_signature
        name: (identifier) @function_name
      )
    )
  ) @function_def
]
";

/// Query for `MethodChannel` constructor invocations.
///
/// Matches `MethodChannel` constructor calls in two patterns:
/// 1. Direct import: `MethodChannel('channel/name')`
///    AST: (`member_access` (identifier) (selector (`argument_part` ...)))
/// 2. Aliased import: `services.MethodChannel('channel/name')`
///    AST: (`member_access` (identifier) (selector (`unconditional_assignable_selector` (identifier))) (selector (`argument_part` ...)))
///
/// This query uses alternation (|) to match either:
/// - Direct: identifier "`MethodChannel`" followed by `argument_part` selector
/// - Aliased: any identifier followed by selector with "`MethodChannel`" identifier, then `argument_part` selector
///
/// Captures:
/// - @`constructor_name`: Should be "`MethodChannel`" (from either direct or aliased position)
/// - @`channel_args`: The arguments node (contains channel name)
/// - @`channel_construct`: The entire `member_access` node
const METHOD_CHANNEL_QUERY: &str = r#"
[
  ; Pattern 1: Direct import - MethodChannel('...')
  (member_access
    (identifier) @constructor_name
    (#eq? @constructor_name "MethodChannel")
    (selector
      (argument_part
        (arguments) @channel_args))) @channel_construct

  ; Pattern 2: Aliased import - services.MethodChannel('...')
  (member_access
    (identifier)
    (selector
      (unconditional_assignable_selector
        (identifier) @constructor_name
        (#eq? @constructor_name "MethodChannel")))
    (selector
      (argument_part
        (arguments) @channel_args))) @channel_construct
]
"#;

/// Query for `MethodChannel` field declarations.
///
/// Matches field declarations of type `MethodChannel` in classes:
/// - `final MethodChannel analyticsChannel = MethodChannel('analytics/events');`
/// - `static const MethodChannel analyticsChannel = MethodChannel('analytics/events');`
/// - `const MethodChannel analyticsChannel = MethodChannel('analytics/events');`
///
/// Captures:
/// - @`var_name`: The variable name (e.g., "analyticsChannel")
/// - @`channel_name`: The string literal channel name (e.g., "'analytics/events'")
const CHANNEL_FIELD_QUERY: &str = r#"
; Match MethodChannel field declarations
; Approach: Match class_member_definition containing a declaration with:
; 1. type_identifier "MethodChannel"
; This works for: final, const, static const, static final patterns
; Tree structure:
; - static const: (declaration (const_builtin) (type_identifier) (static_final_declaration_list ...))
; - final: (declaration (final_builtin) (type_identifier) (initialized_identifier_list ...))
(class_member_definition
  (declaration
    (type_identifier) @type_name
    (#eq? @type_name "MethodChannel")
  )
) @member_def
"#;

/// Query for `MethodChannel.invokeMethod()` calls.
///
/// Matches `member_access` patterns containing:
/// 1. A selector with identifier "invokeMethod"
/// 2. Arguments containing the method name string
///
/// This query matches both:
/// - Direct: channel.invokeMethod('method')
/// - Qualified: registry.channel.invokeMethod('method')
///
/// Tree-walking is used to extract the actual channel receiver, which may be:
/// - Simple identifier (channel)
/// - Qualified selector chain (registry.channel)
///
/// Captures:
/// - @`method_name`: The string literal method name being invoked
/// - @invocation: The entire `member_access` node
const CHANNEL_INVOCATION_QUERY: &str = r#"
(member_access
  (selector
    (unconditional_assignable_selector
      (identifier) @selector_name
      (#eq? @selector_name "invokeMethod")))
  (selector
    (argument_part
      (arguments
        (argument
          (string_literal) @method_name
        )
      )
    )
  )
) @invocation
"#;

/// Query for widget child instantiation in build methods.
///
/// Matches `build()` methods in widget classes. Tree-walking is used to extract
/// all widget instantiations from the return statement and named arguments.
///
/// This simplified query matches `build()` methods, then tree-walking extracts:
/// - Direct return widgets: `return Container()`
/// - Named argument children: `body: Container()`, `child: Text()`
/// - Nested widget trees recursively
///
/// Captures:
/// - @`parent_class`: The widget class containing the `build()` method
/// - @`build_method`: The entire `build()` method definition
const WIDGET_CHILD_QUERY: &str = r#"
(class_definition
  name: (identifier) @parent_class
  body: (class_body
    (class_member_definition
      (method_signature
        (function_signature
          name: (identifier) @method_name
          (#eq? @method_name "build")))
      (function_body) @build_body
    ) @build_method))
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    #[test]
    fn debug_widget_tree_structure() {
        let dart_code = r"
class MyWidget extends StatelessWidget {
  Widget build(BuildContext context) {
    return Container();
  }
}
";

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_dart::language()).unwrap();
        let tree = parser.parse(dart_code, None).unwrap();

        eprintln!("Widget class tree:");
        eprintln!("{}", tree.root_node().to_sexp());

        // Now test with a more verbose widget structure
        let dart_code2 = r"
class PaymentScreen extends StatefulWidget {
  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: Container(
        child: Text('Payment')
      )
    );
  }
}
";
        let tree2 = parser.parse(dart_code2, None).unwrap();
        eprintln!("\nStatefulWidget with children tree:");
        eprintln!("{}", tree2.root_node().to_sexp());

        // Test MethodChannel structure - direct import
        let dart_code3 = r"
void setupChannel() {
    final channel = MethodChannel('payments/native');
}
";
        let tree3 = parser.parse(dart_code3, None).unwrap();
        eprintln!("\nMethodChannel (direct) tree:");
        eprintln!("{}", tree3.root_node().to_sexp());

        // Test MethodChannel structure - aliased import (services.MethodChannel)
        let dart_code4 = r"
import 'package:flutter/services.dart' as services;

void setupChannel() {
    final channel = services.MethodChannel('samples.flutter.dev/battery');
}
";
        let tree4 = parser.parse(dart_code4, None).unwrap();
        eprintln!("\nMethodChannel (aliased services.MethodChannel) tree:");
        eprintln!("{}", tree4.root_node().to_sexp());
    }

    #[test]
    fn debug_function_tree_structure() {
        let dart_code = r"
void myFunction() {
  print('hello');
}

class MyClass {
  void myMethod() {
    print('world');
  }
}
";

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_dart::language()).unwrap();
        let tree = parser.parse(dart_code, None).unwrap();

        eprintln!("Function and method tree:");
        eprintln!("{}", tree.root_node().to_sexp());
    }

    #[test]
    fn debug_static_const_field_structure() {
        let dart_code = r"
class MyClass {
  static const MethodChannel analyticsChannel = MethodChannel('app/native/analytics');
  final MethodChannel paymentsChannel = MethodChannel('payments');
}
";

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_dart::language()).unwrap();
        let tree = parser.parse(dart_code, None).unwrap();

        eprintln!("\nstatic const vs final field structure:");
        eprintln!("{}", tree.root_node().to_sexp());
    }

    #[test]
    fn debug_method_channel_invocation_tree() {
        let dart_code = r"
import 'package:flutter/services.dart';

class PaymentService {
  final MethodChannel analyticsChannel = MethodChannel('analytics/events');
  final MethodChannel paymentsChannel = MethodChannel('payments/native');

  void trackLaunch() {
    analyticsChannel.invokeMethod('trackLaunch');
  }

  Future<void> processPayment() async {
    await paymentsChannel.invokeMethod('commitPurchase', {'amount': 100});
  }
}
";

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_dart::language()).unwrap();
        let tree = parser.parse(dart_code, None).unwrap();

        eprintln!("\nMethodChannel invocation tree:");
        eprintln!("{}", tree.root_node().to_sexp());
    }

    #[test]
    fn debug_qualified_receiver_invocation() {
        let dart_code = r"
class Registry {
  final MethodChannel analyticsChannel = MethodChannel('app/analytics');
  final MethodChannel paymentsChannel = MethodChannel('app/payments');
}

class MyService {
  final Registry registry;

  MyService(this.registry);

  void trackEvent() {
    // Qualified receiver: registry.analyticsChannel.invokeMethod(...)
    registry.analyticsChannel.invokeMethod('trackLaunch');
  }

  void processPayment() {
    // Another qualified receiver
    registry.paymentsChannel.invokeMethod('processPurchase');
  }
}
";

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_dart::language()).unwrap();
        let tree = parser.parse(dart_code, None).unwrap();

        eprintln!("\nQualified receiver invocation tree:");
        eprintln!("{}", tree.root_node().to_sexp());
    }

    #[test]
    fn test_queries_compile() {
        let language = tree_sitter_dart::language();
        let queries = DartQueries::new(&language);
        assert!(
            queries.is_ok(),
            "Dart queries should compile: {:?}",
            queries.err()
        );
    }

    #[test]
    fn test_widget_class_query_captures() {
        let language = tree_sitter_dart::language();
        let queries = DartQueries::new(&language).unwrap();

        let capture_names: Vec<_> = queries
            .widget_classes
            .capture_names()
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect();

        assert!(capture_names.contains(&"class_name"));
        assert!(capture_names.contains(&"base_class"));
        assert!(capture_names.contains(&"widget_def"));
    }

    #[test]
    fn test_method_channel_query_captures() {
        let language = tree_sitter_dart::language();
        let queries = DartQueries::new(&language).unwrap();

        let capture_names: Vec<_> = queries
            .method_channels
            .capture_names()
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect();

        assert!(capture_names.contains(&"constructor_name"));
        assert!(capture_names.contains(&"channel_args"));
        assert!(capture_names.contains(&"channel_construct"));
    }

    #[test]
    fn test_widget_child_query_captures() {
        let language = tree_sitter_dart::language();
        let queries = DartQueries::new(&language).unwrap();

        let capture_names: Vec<_> = queries
            .widget_children
            .capture_names()
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect();

        // Updated query captures parent_class and build_body for tree-walking approach
        assert!(capture_names.contains(&"parent_class"));
        assert!(capture_names.contains(&"build_body"));
        assert!(capture_names.contains(&"method_name"));
    }
}
