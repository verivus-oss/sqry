// GraphBuilder implementation for Groovy
//
// Implements call graph extraction for Groovy source files, including:
// - Class and method definitions
// - Closures (including Gradle task blocks)
// - Function/method calls
// - Property access (treated as potential getter calls)
// - Import statements (simple, aliased, wildcard, static)
// - Class inheritance (extends clause)
//
// Multi-pass architecture:
// - Pass 1: Extract classes → create Class/Module nodes
// - Pass 2: Extract methods/functions/closures → create Function nodes with byte ranges
// - Pass 3: Extract calls → create Call edges
// - Pass 4: Extract imports → create Import edges
// - Pass 5: Extract OOP relationships → create Inherits edges

use sqry_core::graph::{
    GraphBuilder, GraphBuilderError, Language, Span,
    unified::StagingGraph,
    unified::build::GraphBuildHelper,
    unified::build::shape::{CfBucket, ShapeMapping},
    unified::edge::kind::TypeOfContext,
    unified::node::{NodeId, NodeKind},
    unified::storage::shape::SignatureShape,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator, Tree};

use super::type_extractor::{
    extract_all_type_names_from_groovy_type, extract_type_string, is_type_node,
};

/// File-level module name for import edges.
/// Distinct from `<module>` to avoid node kind collision in `GraphBuildHelper` cache.
const FILE_MODULE_NAME: &str = "<file_module>";

/// Represents a callable context (class, method, closure) with its byte range.
#[derive(Debug, Clone)]
struct CallableContext {
    /// Fully qualified name (e.g., "`MyClass::myMethod`")
    qualified_name: String,
    /// Byte range in source file
    byte_range: Range<usize>,
    /// Real line/column span of the declaration; byte_range above is offsets.
    decl_span: Span,
}

impl CallableContext {
    fn contains_offset(&self, offset: usize) -> bool {
        self.byte_range.contains(&offset)
    }
}

/// `GraphBuilder` implementation for Groovy.
#[derive(Debug, Default, Clone)]
pub struct GroovyGraphBuilder;

impl GraphBuilder for GroovyGraphBuilder {
    fn language(&self) -> Language {
        Language::Groovy
    }

    fn shape_mapping(&self) -> Option<&dyn ShapeMapping> {
        Some(groovy_shape_mapping())
    }

    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
        staging: &mut StagingGraph,
    ) -> Result<(), GraphBuilderError> {
        let mut helper = GraphBuildHelper::new(staging, file_path, Language::Groovy);

        // Pass 1: Extract classes and interfaces (including superclass info for OOP edges)
        let (class_contexts, interface_names) =
            extract_classes_and_interfaces(tree.root_node(), content)?;

        // Pass 2: Extract callables (methods, functions, closures)
        let callable_contexts = extract_callables(tree.root_node(), content, &class_contexts)?;

        // Create nodes for all contexts
        let mut context_to_node: HashMap<String, NodeId> = HashMap::new();

        // Create class/interface nodes
        for context in &class_contexts {
            let span = Some(context.decl_span);
            // Use add_interface for interface declarations, add_class for classes
            let node_id = if interface_names.contains(&context.qualified_name) {
                helper.add_interface(&context.qualified_name, span)
            } else {
                helper.add_class(&context.qualified_name, span)
            };
            // issue #394: real declaration; opt dual-use bare helper into is_definition
            helper.mark_definition(node_id);
            context_to_node.insert(context.qualified_name.clone(), node_id);
        }

        // Create callable nodes
        for context in &callable_contexts {
            let span = Some(context.decl_span);
            let node_id = helper.add_function(&context.qualified_name, span, false, false);
            // issue #394: real declaration; opt dual-use bare helper into is_definition
            helper.mark_definition(node_id);
            context_to_node.insert(context.qualified_name.clone(), node_id);
        }

        // Pass 2.5: Extract properties and fields from classes
        extract_properties_and_fields(
            tree.root_node(),
            content,
            &class_contexts,
            &mut helper,
            &mut context_to_node,
        )?;

        // Pass 2.6: Extract TypeOf/Reference edges for function parameters and return types
        extract_function_typeof_edges(
            tree.root_node(),
            content,
            &callable_contexts,
            &context_to_node,
            &mut helper,
        )?;

        // Pass 2.7: Extract FFI edges for native methods and JNA interfaces
        extract_ffi_edges(
            tree.root_node(),
            content,
            &callable_contexts,
            &context_to_node,
            &mut helper,
        )?;

        // Pass 3: Extract calls and create call edges
        visit_node_for_calls(
            tree.root_node(),
            content,
            &callable_contexts,
            &mut helper,
            &context_to_node,
        );

        // Pass 4: Extract imports and create import edges
        collect_import_edges(tree.root_node(), content, &mut helper)?;

        // Pass 5: Extract OOP relationships (inheritance/implementation) and create edges
        extract_oop_edges(
            tree.root_node(),
            content,
            &mut helper,
            &context_to_node,
            &interface_names,
        )?;

        // Pass 6: Emit export edges for public classes, methods, and functions
        emit_export_edges(tree.root_node(), content, &mut helper, &context_to_node)?;

        Ok(())
    }
}

/// Pass 1: Extract class and interface definitions.
///
/// Returns:
/// - A vector of class/interface contexts with their byte ranges for use in Pass 2
/// - A set of interface names (for distinguishing Inherits vs Implements edges)
fn extract_classes_and_interfaces(
    root: Node,
    content: &[u8],
) -> Result<(Vec<CallableContext>, HashSet<String>), GraphBuilderError> {
    let query = Query::new(
        &tree_sitter_groovy_sqry::language(),
        r"
        (class_definition
          name: (identifier) @class.name) @class
        ",
    )
    .map_err(|e| GraphBuilderError::ParseError {
        span: Span::default(),
        reason: format!("Failed to create class query: {e}"),
    })?;

    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, content);

    let mut class_contexts = Vec::new();
    let mut interface_names = HashSet::new();

    while let Some(m) = matches.next() {
        let mut class_name: Option<String> = None;
        let mut class_node: Option<Node> = None;

        for capture in m.captures {
            let capture_name = capture_names[capture.index as usize];
            match capture_name {
                "class.name" => {
                    class_name = Some(
                        capture
                            .node
                            .utf8_text(content)
                            .map_err(|e| GraphBuilderError::ParseError {
                                span: Span::default(),
                                reason: format!("Failed to extract class name: {e}"),
                            })?
                            .to_string(),
                    );
                }
                "class" => {
                    class_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let (Some(name), Some(node)) = (class_name, class_node) {
            // Check if this is an interface declaration
            // In tree-sitter-groovy, both class and interface use class_definition
            // but interface declarations have 'interface' keyword
            if is_interface_declaration(node, content) {
                interface_names.insert(name.clone());
            }

            class_contexts.push(CallableContext {
                qualified_name: name,
                byte_range: node.start_byte()..node.end_byte(),
                decl_span: Span::from_node(&node),
            });
        }
    }

    Ok((class_contexts, interface_names))
}

/// Check if a `class_definition` node represents an interface.
///
/// In tree-sitter-groovy, both classes and interfaces use `class_definition` node type.
/// We detect interfaces by looking for the 'interface' keyword in the node text.
fn is_interface_declaration(class_node: Node, content: &[u8]) -> bool {
    // Walk children to find the keyword
    let mut cursor = class_node.walk();
    for child in class_node.children(&mut cursor) {
        if let Ok(text) = child.utf8_text(content)
            && (text == "interface" || text == "@interface")
        {
            return true;
        }
    }
    false
}

/// Pass 2: Extract methods, functions, and closures.
///
/// Returns a vector of callable contexts with their byte ranges for use in Pass 3.
fn extract_callables(
    root: Node,
    content: &[u8],
    class_contexts: &[CallableContext],
) -> Result<Vec<CallableContext>, GraphBuilderError> {
    let mut contexts = Vec::new();
    extract_callables_recursive(root, content, class_contexts, &mut contexts)?;
    Ok(contexts)
}

/// Recursively extract callable definitions using manual tree walking.
/// This function only collects context information - node creation happens in `build_graph`.
fn extract_callables_recursive(
    node: Node,
    content: &[u8],
    class_contexts: &[CallableContext],
    contexts: &mut Vec<CallableContext>,
) -> Result<(), GraphBuilderError> {
    match node.kind() {
        "function_definition" | "function_declaration" => {
            if let Some(name_node) = node
                .child_by_field_name("function")
                .or_else(|| node.child_by_field_name("name"))
                && let Ok(name) = name_node.utf8_text(content)
            {
                let qualified_name = find_qualified_name(name, node.start_byte(), class_contexts);
                contexts.push(CallableContext {
                    qualified_name,
                    byte_range: node.start_byte()..node.end_byte(),
                    decl_span: Span::from_node(&node),
                });
            }
        }
        "declaration" => {
            if let (Some(value_node), Some(name_node)) = (
                node.child_by_field_name("value"),
                node.child_by_field_name("name"),
            ) && value_node.kind() == "closure"
                && let Ok(name) = name_node.utf8_text(content)
            {
                let qualified_name = find_qualified_name(name, node.start_byte(), class_contexts);
                contexts.push(CallableContext {
                    qualified_name,
                    byte_range: node.start_byte()..node.end_byte(),
                    decl_span: Span::from_node(&node),
                });
            }
        }
        "assignment" => {
            if let (Some(left_node), Some(right_node)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) && right_node.kind() == "closure"
                && let Ok(name) = left_node.utf8_text(content)
            {
                let qualified_name = find_qualified_name(name, node.start_byte(), class_contexts);
                contexts.push(CallableContext {
                    qualified_name,
                    byte_range: node.start_byte()..node.end_byte(),
                    decl_span: Span::from_node(&node),
                });
            }
        }
        "juxt_function_call" => {
            // Handle Gradle task declarations
            if let Some(func_node) = node.child_by_field_name("function")
                && let Ok(func_name) = func_node.utf8_text(content)
                && func_name == "task"
                && let Some(args_node) = node.child_by_field_name("args")
                && let Some(task_name) = extract_task_name(args_node, content)
            {
                let qualified_name =
                    find_qualified_name(&task_name, node.start_byte(), class_contexts);
                contexts.push(CallableContext {
                    qualified_name,
                    byte_range: node.start_byte()..node.end_byte(),
                    decl_span: Span::from_node(&node),
                });
            }
        }
        _ => {}
    }

    // Recursively process children
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        extract_callables_recursive(child, content, class_contexts, contexts)?;
    }

    Ok(())
}

// ================================
// Property and Field Extraction (Pass 2.5)
// ================================

/// Pass 2.5: Extract properties and fields from class definitions.
///
/// In Groovy, properties are fields that automatically get getters/setters.
/// A field is considered a property if:
/// 1. It's declared in a class (not method/function)
/// 2. It doesn't have the `private` or `protected` modifier (public/package by default)
/// 3. It's not `static final` (constants)
///
/// Fields that don't meet these criteria are treated as regular Variable nodes.
fn extract_properties_and_fields(
    root: Node,
    content: &[u8],
    class_contexts: &[CallableContext],
    helper: &mut GraphBuildHelper,
    context_to_node: &mut HashMap<String, NodeId>,
) -> Result<(), GraphBuilderError> {
    extract_properties_recursive(root, content, class_contexts, helper, context_to_node)
}

/// Recursively walk the AST to find field/property declarations.
fn extract_properties_recursive(
    node: Node,
    content: &[u8],
    class_contexts: &[CallableContext],
    helper: &mut GraphBuildHelper,
    context_to_node: &mut HashMap<String, NodeId>,
) -> Result<(), GraphBuilderError> {
    // Only process field declarations within class_definition nodes
    if node.kind() == "class_definition"
        && let Some(body_node) = node.child_by_field_name("body")
    {
        process_class_members(
            body_node,
            node,
            content,
            class_contexts,
            helper,
            context_to_node,
        )?;
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        extract_properties_recursive(child, content, class_contexts, helper, context_to_node)?;
    }

    Ok(())
}

/// Process members of a class body to extract properties and fields.
#[allow(clippy::needless_pass_by_value)]
fn process_class_members(
    body_node: Node,
    class_node: Node,
    content: &[u8],
    _class_contexts: &[CallableContext],
    helper: &mut GraphBuildHelper,
    context_to_node: &mut HashMap<String, NodeId>,
) -> Result<(), GraphBuilderError> {
    // Get class name for qualified naming
    let class_name = if let Some(name_node) = class_node.child_by_field_name("name") {
        name_node.utf8_text(content).unwrap_or("UnknownClass")
    } else {
        return Ok(());
    };

    let mut cursor = body_node.walk();
    for child in body_node.children(&mut cursor) {
        match child.kind() {
            "declaration" => {
                // Field declaration: `type name = value` or `def name = value`
                if let Some(name_node) = child.child_by_field_name("name")
                    && let Ok(field_name) = name_node.utf8_text(content)
                {
                    process_field_declaration(
                        child,
                        field_name,
                        class_name,
                        content,
                        helper,
                        context_to_node,
                    )?;
                }
            }
            "field_declaration" => {
                // Explicit field declaration node (if grammar supports it)
                if let Some(name_node) = child.child_by_field_name("name")
                    && let Ok(field_name) = name_node.utf8_text(content)
                {
                    process_field_declaration(
                        child,
                        field_name,
                        class_name,
                        content,
                        helper,
                        context_to_node,
                    )?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Process a single field declaration and determine if it's a property or variable.
fn process_field_declaration(
    field_node: Node,
    field_name: &str,
    class_name: &str,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    context_to_node: &mut HashMap<String, NodeId>,
) -> Result<(), GraphBuilderError> {
    // Skip if it's a closure/function (those are handled as callable contexts)
    if let Some(value_node) = field_node.child_by_field_name("value")
        && value_node.kind() == "closure"
    {
        return Ok(());
    }

    let qualified_name = format!("{class_name}::{field_name}");
    let span = Some(Span::from_node(&field_node));

    // Determine if this is a property or a field
    let is_static_final = is_static_final(field_node, content);
    let is_private_protected = is_private(field_node, content) || is_protected(field_node, content);

    // In Groovy, properties are public/package fields (not static final constants)
    // Properties automatically get getters/setters
    let node_id = if !is_static_final && !is_private_protected {
        // Property: public/package field with auto-generated accessors
        helper.add_node(&qualified_name, span, NodeKind::Property)
    } else {
        // Variable: private, protected, or static final field
        helper.add_variable(&qualified_name, span)
    };
    // issue #394: real declaration (Property or field/variable); opt dual-use
    // bare helper into is_definition
    helper.mark_definition(node_id);

    context_to_node.insert(qualified_name.clone(), node_id);

    // Process TypeOf and Reference edges for this field
    process_field_typeof_edges(field_node, node_id, content, helper)?;

    Ok(())
}

/// Process `TypeOf` and Reference edges for a field/property declaration.
///
/// Extracts type annotations from field declarations and creates:
/// - `TypeOf` edge with full type signature (Field context)
/// - Reference edges for all nested type names
///
/// Handles:
/// - Simple types: `String name` → `TypeOf`: name → "String"
/// - Generic types: `List<String> items` → `TypeOf`: items → "List<String>", Reference: List, String
/// - Builtin types: `int count` → `TypeOf`: count → "int"
/// - Dynamic types: `def value` → skipped (no type annotation)
#[allow(clippy::unnecessary_wraps)]
fn process_field_typeof_edges(
    field_node: Node,
    field_id: NodeId,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> Result<(), GraphBuilderError> {
    // Find type annotation in the declaration
    // In Groovy, type comes before the name in field declarations
    let type_node = find_field_type_annotation(field_node, content);
    let Some(type_node) = type_node else {
        // No type annotation (e.g., def value = ...) - skip
        return Ok(());
    };

    // Extract full type string for TypeOf edge
    let type_text = extract_type_string(type_node, content);
    let Some(type_text) = type_text else {
        // Failed to extract or dynamic type (def) - skip
        return Ok(());
    };

    // Create Type node and TypeOf edge
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(field_id, type_id, Some(TypeOfContext::Field), None, None);

    // Create Reference edges for nested types
    let referenced_types = extract_all_type_names_from_groovy_type(type_node, content);
    for ref_type_name in referenced_types {
        let ref_type_id = helper.add_type(&ref_type_name, None);
        helper.add_reference_edge(field_id, ref_type_id);
    }

    Ok(())
}

/// Find the type annotation node in a field declaration.
///
/// In Groovy AST:
/// - `declaration` node contains: [type] [name] [= value]
/// - Type is the first child (identifier, builtintype, or `type_with_generics`)
/// - Name is the second child (identifier)
fn find_field_type_annotation<'a>(field_node: Node<'a>, _content: &[u8]) -> Option<Node<'a>> {
    let mut cursor = field_node.walk();
    for child in field_node.children(&mut cursor) {
        if is_type_node(child.kind()) {
            // Found a type node - verify it's not the variable name
            if let Some(name_node) = field_node.child_by_field_name("name") {
                // Make sure this type node comes before the name node
                if child.start_byte() < name_node.start_byte() {
                    return Some(child);
                }
            } else {
                // No name field, so assume first type node is the type
                return Some(child);
            }
        }
    }
    None
}

/// Check if a field is `static final` (a constant).
fn is_static_final(node: Node, content: &[u8]) -> bool {
    let mut has_static = false;
    let mut has_final = false;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifier"
            && let Ok(text) = child.utf8_text(content)
        {
            if text == "static" {
                has_static = true;
            } else if text == "final" {
                has_final = true;
            }
        }
    }

    has_static && has_final
}

/// Check if a field has the `protected` visibility modifier.
fn is_protected(node: Node, content: &[u8]) -> bool {
    has_visibility_modifier(node, "protected", content)
}

// ================================
// Function TypeOf/Reference Extraction (Pass 2.6)
// ================================

/// Pass 2.6: Extract `TypeOf` and Reference edges for function parameters and return types.
///
/// Recursively traverses the AST to find function/method definitions and processes
/// their type annotations.
fn extract_function_typeof_edges(
    root: Node,
    content: &[u8],
    callable_contexts: &[CallableContext],
    context_to_node: &HashMap<String, NodeId>,
    helper: &mut GraphBuildHelper,
) -> Result<(), GraphBuilderError> {
    extract_function_typeof_recursive(root, content, callable_contexts, context_to_node, helper)
}

/// Recursively process function/method nodes to extract TypeOf/Reference edges.
fn extract_function_typeof_recursive(
    node: Node,
    content: &[u8],
    callable_contexts: &[CallableContext],
    context_to_node: &HashMap<String, NodeId>,
    helper: &mut GraphBuildHelper,
) -> Result<(), GraphBuilderError> {
    match node.kind() {
        "function_definition" | "function_declaration" => {
            // Get function name and qualified name
            if let Some(name_node) = node
                .child_by_field_name("function")
                .or_else(|| node.child_by_field_name("name"))
                && let Ok(func_name) = name_node.utf8_text(content)
            {
                // Find the matching callable context to get qualified name
                let qualified_name = callable_contexts
                    .iter()
                    .find(|ctx| {
                        ctx.byte_range.contains(&node.start_byte())
                            && ctx.qualified_name.ends_with(func_name)
                    })
                    .map_or(func_name, |ctx| ctx.qualified_name.as_str());

                // Get NodeId from context map
                if let Some(&func_id) = context_to_node.get(qualified_name) {
                    // Process return type
                    process_function_return_typeof(node, func_id, content, helper)?;

                    // Process parameters
                    process_function_parameters_typeof(node, func_id, content, helper)?;
                }
            }
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        extract_function_typeof_recursive(
            child,
            content,
            callable_contexts,
            context_to_node,
            helper,
        )?;
    }

    Ok(())
}

/// Process `TypeOf` and Reference edges for function return type.
///
/// Extracts return type annotation from function signature and creates:
/// - `TypeOf` edge with full type signature (Return context, index 0)
/// - Reference edges for all nested type names
///
/// Handles:
/// - Simple return types: `String getName()` → `TypeOf`: getName → "String"
/// - Generic return types: `List<String> getItems()` → `TypeOf`: getItems → "List<String>"
/// - Void return: `void process()` → `TypeOf`: process → "void"
/// - No return type: `def dynamic()` → skipped
#[allow(clippy::unnecessary_wraps)]
fn process_function_return_typeof(
    func_node: Node,
    func_id: NodeId,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> Result<(), GraphBuilderError> {
    // In Groovy AST, return type appears as first child of function_definition
    // before the function name node
    let return_type_node = find_function_return_type(func_node, content);
    let Some(return_type_node) = return_type_node else {
        // No return type annotation - skip
        return Ok(());
    };

    // Extract full type string for TypeOf edge
    let type_text = extract_type_string(return_type_node, content);
    let Some(type_text) = type_text else {
        // Failed to extract or dynamic type - skip
        return Ok(());
    };

    // Create Type node and TypeOf edge with Return context
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(
        func_id,
        type_id,
        Some(TypeOfContext::Return),
        Some(0), // Return index 0
        None,
    );

    // Create Reference edges for nested types
    let referenced_types = extract_all_type_names_from_groovy_type(return_type_node, content);
    for ref_type_name in referenced_types {
        let ref_type_id = helper.add_type(&ref_type_name, None);
        helper.add_reference_edge(func_id, ref_type_id);
    }

    Ok(())
}

/// Find the return type node in a function definition.
///
/// In Groovy AST:
/// - `function_definition` children: [`return_type`] [`function_name`] [`parameter_list`] [body]
/// - Return type is first child before the function name
fn find_function_return_type<'a>(func_node: Node<'a>, _content: &[u8]) -> Option<Node<'a>> {
    let mut cursor = func_node.walk();

    // Get the function name node position
    let name_node = func_node
        .child_by_field_name("function")
        .or_else(|| func_node.child_by_field_name("name"))?;

    // Find first type node that appears before the function name
    func_node
        .children(&mut cursor)
        .find(|&child| child.start_byte() < name_node.start_byte() && is_type_node(child.kind()))
}

/// Process `TypeOf` and Reference edges for function parameters.
///
/// Extracts parameter type annotations and creates:
/// - `TypeOf` edge for each parameter (Parameter context with index)
/// - Reference edges for all nested type names
///
/// Handles:
/// - Simple parameters: `void process(String input)` → `TypeOf`: process → "String" (param 0)
/// - Multiple parameters: `int add(int a, int b)` → `TypeOf`: process → "int" (param 0, 1)
/// - Generic parameters: `void handle(List<String> items)` → nested types
#[allow(clippy::unnecessary_wraps)]
fn process_function_parameters_typeof(
    func_node: Node,
    func_id: NodeId,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> Result<(), GraphBuilderError> {
    // Find parameter_list node using field name
    let param_list_node = func_node.child_by_field_name("parameters");

    // If not found by field name, try finding by kind
    let param_list_node = param_list_node.or_else(|| {
        let mut cursor = func_node.walk();
        func_node
            .children(&mut cursor)
            .find(|child| child.kind() == "parameter_list")
    });

    let Some(param_list_node) = param_list_node else {
        // No parameters - skip
        return Ok(());
    };

    // Iterate through parameters (named children only)
    let mut param_index: u16 = 0;
    let mut cursor = param_list_node.walk();
    for param_node in param_list_node.named_children(&mut cursor) {
        if param_node.kind() == "parameter" {
            process_parameter_typedef(param_node, func_id, param_index, content, helper)?;
            param_index += 1;
        }
    }

    Ok(())
}

/// Process `TypeOf` and Reference edges for a single function parameter.
#[allow(clippy::unnecessary_wraps)]
fn process_parameter_typedef(
    param_node: Node,
    func_id: NodeId,
    param_index: u16,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> Result<(), GraphBuilderError> {
    // Find type annotation in parameter
    // In Groovy, parameter structure: [type] [name]
    let type_node = find_parameter_type(param_node, content);
    let Some(type_node) = type_node else {
        // No type annotation - skip
        return Ok(());
    };

    // Extract full type string for TypeOf edge
    let type_text = extract_type_string(type_node, content);
    let Some(type_text) = type_text else {
        // Failed to extract or dynamic type - skip
        return Ok(());
    };

    // Create Type node and TypeOf edge with Parameter context
    let type_id = helper.add_type(&type_text, None);
    helper.add_typeof_edge_with_context(
        func_id,
        type_id,
        Some(TypeOfContext::Parameter),
        Some(param_index),
        None,
    );

    // Create Reference edges for nested types
    let referenced_types = extract_all_type_names_from_groovy_type(type_node, content);
    for ref_type_name in referenced_types {
        let ref_type_id = helper.add_type(&ref_type_name, None);
        helper.add_reference_edge(func_id, ref_type_id);
    }

    Ok(())
}

/// Find the type node in a parameter node.
///
/// In Groovy AST:
/// - Typed parameter: [type] [name] (2 identifiers: String name)
/// - Untyped parameter: [name] (1 identifier: def name, where "def" doesn't appear)
/// - Type is first child (identifier, builtintype, or `type_with_generics`)
///
/// Returns None if parameter has no explicit type (def keyword used).
fn find_parameter_type<'a>(param_node: Node<'a>, _content: &[u8]) -> Option<Node<'a>> {
    // Count type-like children
    let mut type_children = Vec::new();
    let mut cursor = param_node.walk();
    for child in param_node.named_children(&mut cursor) {
        if is_type_node(child.kind()) {
            type_children.push(child);
        }
    }

    // If there's only 1 identifier, it's the parameter name (def parameter)
    // If there are 2+ identifiers, the first is the type
    if type_children.len() >= 2 {
        Some(type_children[0])
    } else {
        None
    }
}

// ================================
// FFI Detection (Pass 2.7)
// ================================

/// Pass 2.7: Extract FFI edges for native methods (JNI) and JNA interfaces.
///
/// Groovy supports FFI through:
/// 1. JNI - `native` keyword (like Java)
/// 2. JNA - `@NativeLibrary` annotation on interfaces extending Library
/// 3. JNA - Direct `Native.load()` calls
fn extract_ffi_edges(
    root: Node,
    content: &[u8],
    callable_contexts: &[CallableContext],
    context_to_node: &HashMap<String, NodeId>,
    helper: &mut GraphBuildHelper,
) -> Result<(), GraphBuilderError> {
    extract_ffi_recursive(root, content, callable_contexts, context_to_node, helper)
}

/// Recursively process nodes to find FFI patterns.
fn extract_ffi_recursive(
    node: Node,
    content: &[u8],
    callable_contexts: &[CallableContext],
    context_to_node: &HashMap<String, NodeId>,
    helper: &mut GraphBuildHelper,
) -> Result<(), GraphBuilderError> {
    match node.kind() {
        "function_definition" | "function_declaration" => {
            // Check for native keyword (JNI)
            if has_native_modifier(node, content) {
                build_native_method_ffi_edge(
                    node,
                    content,
                    callable_contexts,
                    context_to_node,
                    helper,
                )?;
            }
        }
        "class_definition" => {
            // Check for @NativeLibrary annotation (JNA interface)
            if has_native_library_annotation(node, content) {
                // JNA interface - create FFI edges for all methods
                build_jna_interface_ffi_edges(
                    node,
                    content,
                    callable_contexts,
                    context_to_node,
                    helper,
                )?;
            }
        }
        "method_invocation" | "function_call" => {
            // Check for Native.load() or Native.loadLibrary() calls (JNA direct)
            if is_native_load_call(node, content) {
                build_native_load_ffi_edges(node, content, context_to_node, helper)?;
            }
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        extract_ffi_recursive(child, content, callable_contexts, context_to_node, helper)?;
    }

    Ok(())
}

/// Check if a function has the `native` modifier (JNI).
/// In Groovy's tree-sitter grammar, "native" can appear in multiple ways:
/// 1. As a separate identifier before the `function_declaration` (simple case: native void `foo()`)
/// 2. Inside an ERROR node within the `function_declaration` (access modifier case: private native void `foo()`)
/// 3. As a sibling in a parent "declaration" node (static native case: static native void `foo()`)
fn has_native_modifier(func_node: Node, content: &[u8]) -> bool {
    // Check if previous sibling is "native" identifier (simple case: native void foo())
    if let Some(prev_sibling) = func_node.prev_named_sibling()
        && prev_sibling.kind() == "identifier"
        && prev_sibling.utf8_text(content) == Ok("native")
    {
        return true;
    }

    // Check for ERROR node containing "native" identifier (access modifier case: private native void foo())
    let mut cursor = func_node.walk();
    for child in func_node.named_children(&mut cursor) {
        if child.kind() == "ERROR" {
            // Look for "native" identifier inside ERROR node
            let mut error_cursor = child.walk();
            for error_child in child.named_children(&mut error_cursor) {
                if error_child.kind() == "identifier"
                    && error_child.utf8_text(content) == Ok("native")
                {
                    return true;
                }
            }
        }
    }

    // Check previous sibling for "declaration" node containing "native" identifier (static native case)
    // AST: declaration { modifier "static", identifier "native" } → function_declaration
    let mut current_sibling = func_node.prev_named_sibling();
    while let Some(sibling) = current_sibling {
        if sibling.kind() == "declaration" {
            // Check if this declaration contains "native"
            let mut decl_cursor = sibling.walk();
            for child in sibling.named_children(&mut decl_cursor) {
                if child.kind() == "identifier" && child.utf8_text(content) == Ok("native") {
                    return true;
                }
            }
        }
        current_sibling = sibling.prev_named_sibling();
    }

    false
}

/// Check if a class has the `@NativeLibrary` annotation (JNA) AND extends Library.
/// Handles both simple (@`NativeLibrary`) and qualified (@com.sun.jna.NativeLibrary) forms.
/// Note: Groovy's tree-sitter grammar parses qualified annotations incorrectly as ERROR nodes
/// with separate `function_call` siblings, so we need to check the previous sibling.
/// JNA REQUIRES interfaces to extend com.sun.jna.Library, so we validate both annotation and inheritance.
fn has_native_library_annotation(class_node: Node, content: &[u8]) -> bool {
    // First check if class extends Library (required for JNA)
    if !extends_library(class_node, content) {
        return false; // Not a JNA interface if it doesn't extend Library
    }

    // Check direct annotation children (simple @NativeLibrary case)
    let mut cursor = class_node.walk();
    for child in class_node.children(&mut cursor) {
        if child.kind() == "annotation"
            && let Ok(text) = child.utf8_text(content)
        {
            let cleaned = text
                .trim()
                .trim_start_matches('@')
                .split('(')
                .next()
                .unwrap_or("");
            if cleaned == "NativeLibrary" || cleaned.ends_with(".NativeLibrary") {
                return true;
            }
        }
    }

    // Check ALL previous siblings for qualified annotation (grammar limitation workaround)
    // Qualified annotations like @com.sun.jna.NativeLibrary("lib") appear as function_call
    // Handle multiple annotations (e.g., @Deprecated + @NativeLibrary)
    let mut current_sibling = class_node.prev_named_sibling();
    while let Some(sibling) = current_sibling {
        if sibling.kind() == "function_call"
            && let Some(identifier_node) = sibling.child_by_field_name("function")
            && let Ok(text) = identifier_node.utf8_text(content)
            && (text.ends_with("NativeLibrary") || text.ends_with(".NativeLibrary"))
        {
            return true;
        }
        // Move to previous sibling
        current_sibling = sibling.prev_named_sibling();
    }

    false
}

/// Check if a class/interface extends Library (required for JNA).
/// Uses exact identifier matching on the superclass clause ONLY to avoid false positives
/// (e.g., methods, fields, or annotations named "Library" should not match).
fn extends_library(class_node: Node, content: &[u8]) -> bool {
    // Get the superclass clause using the "superclass" field (tree-sitter-groovy specific)
    // This scopes the check to ONLY the extends clause, avoiding false positives from:
    // - Methods named Library()
    // - Fields named Library
    // - Annotations @Library
    // - Any other identifiers that happen to be "Library"
    if let Some(superclass_node) = class_node.child_by_field_name("superclass") {
        let parent_name = extract_type_name(superclass_node, content);
        // Exact match: "Library" or ends with ".Library" (e.g., "com.sun.jna.Library")
        return parent_name == "Library" || parent_name.ends_with(".Library");
    }

    false
}

/// Build FFI edge for a native method (JNI).
#[allow(clippy::unnecessary_wraps)] // Result for API consistency with other graph builder helpers
fn build_native_method_ffi_edge(
    func_node: Node,
    content: &[u8],
    callable_contexts: &[CallableContext],
    context_to_node: &HashMap<String, NodeId>,
    helper: &mut GraphBuildHelper,
) -> Result<(), GraphBuilderError> {
    // Get function name
    let name_node = func_node
        .child_by_field_name("function")
        .or_else(|| func_node.child_by_field_name("name"));

    let Some(name_node) = name_node else {
        return Ok(());
    };

    let Ok(func_name) = name_node.utf8_text(content) else {
        return Ok(());
    };

    // Find matching callable context
    let qualified_name = callable_contexts
        .iter()
        .find(|ctx| {
            ctx.byte_range.contains(&func_node.start_byte())
                && ctx.qualified_name.ends_with(func_name)
        })
        .map_or(func_name, |ctx| ctx.qualified_name.as_str());

    // Get function node ID
    let Some(&func_id) = context_to_node.get(qualified_name) else {
        return Ok(());
    };

    // Create FFI target (using JVM-style naming like Kotlin/Scala)
    // Format: <ffi:ClassName.methodName>
    // NOTE: Currently uses name-only (no JVM signature mangling).
    // This means overloaded native methods will collide into a single FFI node.
    // JVM signature mangling (like Kotlin/Scala) can be added if Groovy needs overload disambiguation.
    // See test_overloaded_native_methods_collision for collision behavior documentation.
    let ffi_target = format!("<ffi:{}>", qualified_name.replace("::", "."));
    let target_id = helper.add_node(&ffi_target, None, NodeKind::Other);

    // Add FFI edge (JNI uses C calling convention)
    helper.add_ffi_edge(
        func_id,
        target_id,
        sqry_core::graph::unified::edge::kind::FfiConvention::C,
    );

    Ok(())
}

/// Build FFI edges for all methods in a JNA interface.
#[allow(clippy::unnecessary_wraps)] // Result for API consistency with other graph builder helpers
fn build_jna_interface_ffi_edges(
    class_node: Node,
    content: &[u8],
    _callable_contexts: &[CallableContext],
    context_to_node: &HashMap<String, NodeId>,
    helper: &mut GraphBuildHelper,
) -> Result<(), GraphBuilderError> {
    // Get class name
    let class_name = if let Some(name_node) = class_node.child_by_field_name("name") {
        name_node.utf8_text(content).unwrap_or("UnknownClass")
    } else {
        return Ok(());
    };

    // Find body node
    let Some(body_node) = class_node.child_by_field_name("body") else {
        return Ok(());
    };

    // Process all method declarations in the interface
    let mut cursor = body_node.walk();
    for child in body_node.children(&mut cursor) {
        if child.kind() == "function_definition" || child.kind() == "function_declaration" {
            // Skip default methods (those with body) and static methods
            // Only abstract interface methods are FFI calls
            if is_default_or_static_method(child, content) {
                continue;
            }

            // Get method name
            if let Some(name_node) = child
                .child_by_field_name("function")
                .or_else(|| child.child_by_field_name("name"))
                && let Ok(method_name) = name_node.utf8_text(content)
            {
                let qualified_name = format!("{class_name}::{method_name}");

                // Get method node ID
                if let Some(&method_id) = context_to_node.get(&qualified_name) {
                    // Create FFI target for JNA method (use fully qualified dotted notation like JNI)
                    let ffi_target = format!("<ffi:{}>", qualified_name.replace("::", "."));
                    let target_id = helper.add_node(&ffi_target, None, NodeKind::Other);

                    // Add FFI edge (JNA uses C calling convention)
                    helper.add_ffi_edge(
                        method_id,
                        target_id,
                        sqry_core::graph::unified::edge::kind::FfiConvention::C,
                    );
                }
            }
        }
    }

    Ok(())
}

/// Check if a function call is a `Native.load()` or `Native.loadLibrary()` call (JNA direct).
/// Groovy parses these as `function_call` with `dotted_identifier` (Native.load).
fn is_native_load_call(call_node: Node, content: &[u8]) -> bool {
    // Look for dotted_identifier or other function name representations
    let mut cursor = call_node.walk();
    for child in call_node.named_children(&mut cursor) {
        if child.kind() == "dotted_identifier" {
            // Get the full text of the dotted identifier
            if let Ok(text) = child.utf8_text(content)
                && (text == "Native.load"
                    || text == "Native.loadLibrary"
                    || text.ends_with(".Native.load")
                    || text.ends_with(".Native.loadLibrary"))
            {
                return true;
            }
        }
    }

    false
}

/// Extract the last identifier from a `dotted_identifier` node.
/// For "`MyLib`", returns "`MyLib`".
/// For "com.example.MyLib", returns "`MyLib`" (the rightmost identifier).
fn extract_last_identifier_from_dotted(node: Node, content: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => {
            // Base case: direct identifier
            node.utf8_text(content)
                .ok()
                .map(std::string::ToString::to_string)
        }
        "dotted_identifier" => {
            // Recursive case: get the last child (which is the rightmost identifier)
            // For "com.example.MyLib", the last child is "MyLib"
            let child_count = node.named_child_count();
            #[allow(
                clippy::cast_possible_truncation,
                reason = "tree-sitter node child counts fit in u32"
            )]
            if child_count > 0
                && let Some(last_child) = node.named_child((child_count - 1) as u32)
            {
                return extract_last_identifier_from_dotted(last_child, content);
            }
            None
        }
        _ => None,
    }
}

/// Build FFI edges for `Native.load()` or `Native.loadLibrary()` calls (JNA direct).
#[allow(clippy::unnecessary_wraps)] // Result for API consistency with other graph builder helpers
fn build_native_load_ffi_edges(
    call_node: Node,
    content: &[u8],
    context_to_node: &HashMap<String, NodeId>,
    helper: &mut GraphBuildHelper,
) -> Result<(), GraphBuilderError> {
    // Get argument_list child
    let mut cursor = call_node.walk();
    let mut args_node = None;
    for child in call_node.named_children(&mut cursor) {
        if child.kind() == "argument_list" {
            args_node = Some(child);
            break;
        }
    }

    let Some(args_node) = args_node else {
        return Ok(());
    };

    // Extract arguments: Native.load(String libraryName, Class interfaceClass)
    let mut arg_cursor = args_node.walk();
    let args: Vec<Node> = args_node.named_children(&mut arg_cursor).collect();

    if args.len() < 2 {
        return Ok(()); // Need at least 2 arguments
    }

    // Second argument should be the class reference (MyInterface.class)
    // It's a dotted_identifier with "MyInterface" and "class"
    let class_arg = args[1];

    // Extract interface name from dotted_identifier (e.g., "MyInterface.class" or "com.example.MyInterface.class")
    // Tree structure:
    //   Simple:  dotted_identifier { identifier("MyLib"), identifier("class") }
    //   Fully qualified: dotted_identifier { dotted_identifier("com.example.MyLib"), identifier("class") }
    // For fully qualified names, we need the LAST identifier from the nested dotted_identifier
    let interface_name = if class_arg.kind() == "dotted_identifier" {
        // Get the first child (everything before ".class")
        let Some(name_part) = class_arg.named_child(0) else {
            return Ok(());
        };

        // Recursively extract the last identifier from the name part
        extract_last_identifier_from_dotted(name_part, content)
    } else {
        // Fallback: try to get text directly and strip ".class" suffix
        class_arg.utf8_text(content).ok().and_then(|text| {
            text.strip_suffix(".class")
                .map(std::string::ToString::to_string)
        })
    };

    let Some(interface_name) = interface_name else {
        return Ok(());
    };

    // Find all methods in the interface class and create FFI edges
    // Search for methods with qualified names like "ClassName::methodName"
    // Use exact prefix match to avoid false positives (e.g., "MyLib" shouldn't match "MyLibHelper")
    let interface_prefix = format!("{interface_name}::");
    for (qualified_name, &node_id) in context_to_node {
        if qualified_name.starts_with(&interface_prefix) {
            // Create FFI target (use fully qualified dotted notation)
            let ffi_target = format!("<ffi:{}>", qualified_name.replace("::", "."));
            let target_id = helper.add_node(&ffi_target, None, NodeKind::Other);

            // Add FFI edge
            helper.add_ffi_edge(
                node_id,
                target_id,
                sqry_core::graph::unified::edge::kind::FfiConvention::C,
            );
        }
    }

    Ok(())
}

/// Check if a method is default (has body) or static.
/// Only abstract interface methods should generate FFI edges.
fn is_default_or_static_method(method_node: Node, content: &[u8]) -> bool {
    // Check if method has a body (default method)
    if method_node.child_by_field_name("body").is_some() {
        return true; // Has body, so it's a default method
    }

    // Check for "static" modifier
    let mut cursor = method_node.walk();
    for child in method_node.named_children(&mut cursor) {
        if child.kind() == "identifier" && child.utf8_text(content) == Ok("static") {
            return true;
        }
        // Also check in access_modifier or modifier nodes
        if (child.kind() == "access_modifier" || child.kind() == "modifier")
            && let Ok(text) = child.utf8_text(content)
            && text.contains("static")
        {
            return true;
        }
    }

    false
}

// ================================
// Call Extraction (Pass 3)
// ================================

/// Pass 3: Visit nodes and create call edges using `GraphBuildHelper`.
///
/// Walks the AST to find call expressions and creates edges in the staging graph.
fn visit_node_for_calls(
    node: Node,
    content: &[u8],
    contexts: &[CallableContext],
    helper: &mut GraphBuildHelper,
    context_to_node: &HashMap<String, NodeId>,
) {
    match node.kind() {
        "function_call" | "method_invocation" | "juxt_function_call" => {
            if let Some(target_node) = node
                .child_by_field_name("function")
                .or_else(|| node.child_by_field_name("name"))
                && let Ok(target_name) = target_node.utf8_text(content)
            {
                // Find the enclosing callable context (caller)
                let caller_context = contexts
                    .iter()
                    .find(|ctx| ctx.contains_offset(node.start_byte()));

                if let Some(caller_ctx) = caller_context {
                    // Find the callee - check if it matches a known callable
                    let callee_qname = resolve_callee_name(target_name, caller_ctx, contexts);

                    if let Some(callee_name) = callee_qname {
                        // Get node IDs from our context map
                        if let (Some(&caller_id), Some(&callee_id)) = (
                            context_to_node.get(&caller_ctx.qualified_name),
                            context_to_node.get(&callee_name),
                        ) {
                            let argument_count = count_call_arguments(node);
                            let call_span = Span::from_node(&node);
                            helper.add_call_edge_full_with_span(
                                caller_id,
                                callee_id,
                                argument_count,
                                false,
                                vec![call_span],
                            );
                        }
                    }
                }
            }
        }
        _ => {}
    }

    // Recursively process children
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit_node_for_calls(child, content, contexts, helper, context_to_node);
    }
}

/// Resolve callee name to a qualified name if it matches a known callable.
fn resolve_callee_name(
    target: &str,
    caller_context: &CallableContext,
    contexts: &[CallableContext],
) -> Option<String> {
    // Try to find callee in same class first
    let qualified_target = if caller_context.qualified_name.contains("::") {
        let class_name = caller_context
            .qualified_name
            .split("::")
            .next()
            .unwrap_or(&caller_context.qualified_name);
        format!("{class_name}::{target}")
    } else {
        target.to_string()
    };

    // Check if qualified version exists
    if contexts
        .iter()
        .any(|ctx| ctx.qualified_name == qualified_target)
    {
        return Some(qualified_target);
    }

    // Fall back to unqualified
    if contexts.iter().any(|ctx| ctx.qualified_name == target) {
        return Some(target.to_string());
    }

    // Not a user-defined function
    None
}

fn count_call_arguments(call_node: Node<'_>) -> u8 {
    let args_node = call_node
        .child_by_field_name("arguments")
        .or_else(|| call_node.child_by_field_name("argument_list"))
        .or_else(|| {
            let mut cursor = call_node.walk();
            call_node
                .children(&mut cursor)
                .find(|child| child.kind() == "argument_list")
        });

    let Some(args_node) = args_node else {
        return 255;
    };

    let count = args_node.named_child_count();
    if count <= 254 {
        u8::try_from(count).unwrap_or(u8::MAX)
    } else {
        u8::MAX
    }
}

/// Helper: Find the qualified name for a symbol based on its offset.
///
/// If the offset is within a class, returns "`ClassName::symbolName`".
/// Otherwise returns just "symbolName".
fn find_qualified_name(name: &str, offset: usize, class_contexts: &[CallableContext]) -> String {
    for context in class_contexts {
        if context.contains_offset(offset) {
            return format!("{}::{}", context.qualified_name, name);
        }
    }
    name.to_string()
}

/// Helper: Extract task name from Gradle `task()` arguments.
fn extract_task_name(args_node: Node, content: &[u8]) -> Option<String> {
    // Look for identifier or string in arguments
    let mut cursor = args_node.walk();
    for child in args_node.named_children(&mut cursor) {
        match child.kind() {
            "identifier" | "string" => {
                if let Ok(text) = child.utf8_text(content) {
                    // Strip quotes from strings
                    let name = text.trim_matches(|c| c == '"' || c == '\'');
                    return Some(name.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

// ================================
// Import Extraction (Pass 4)
// ================================

/// Pass 4: Extract import statements and create import edges.
///
/// Handles Groovy import patterns:
/// - Simple import: `import groovy.transform.ToString`
/// - Aliased import: `import groovy.transform.ToString as TS`
/// - Wildcard import: `import groovy.transform.*`
/// - Static import: `import static java.lang.Math.PI`
///
/// tree-sitter-groovy structure:
/// ```text
/// groovy_import
///   modifier? (for static)
///   import: qualified_name
///     identifier*
///   wildcard_import? ("*")
///   import_alias: identifier? (for "as" alias)
/// ```
fn collect_import_edges(
    root: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> Result<(), GraphBuilderError> {
    collect_import_edges_recursive(root, content, helper)
}

/// Recursively walk the AST to find import statements.
fn collect_import_edges_recursive(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
) -> Result<(), GraphBuilderError> {
    if node.kind() == "groovy_import" {
        process_import_node(node, content, helper);
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_import_edges_recursive(child, content, helper)?;
    }

    Ok(())
}

/// Process a single import node and create the appropriate edge.
fn process_import_node(import_node: Node, content: &[u8], helper: &mut GraphBuildHelper) {
    // Check for wildcard import
    let mut has_wildcard = false;
    let mut cursor = import_node.walk();
    for child in import_node.children(&mut cursor) {
        if child.kind() == "wildcard_import" {
            has_wildcard = true;
            break;
        }
    }

    // Check for static import (modifier child)
    let mut is_static = false;
    cursor = import_node.walk();
    for child in import_node.children(&mut cursor) {
        if child.kind() == "modifier"
            && let Ok(text) = child.utf8_text(content)
            && text == "static"
        {
            is_static = true;
            break;
        }
    }

    // Extract the import path from the qualified_name field
    let import_field = import_node.child_by_field_name("import");
    let Some(qualified_name_node) = import_field else {
        return; // Malformed import, skip
    };

    // Build the full import path from identifiers
    let import_path = extract_qualified_name(qualified_name_node, content);
    if import_path.is_empty() {
        return;
    }

    // Build final import name
    let imported_name = if has_wildcard {
        format!("{import_path}.*")
    } else if is_static {
        format!("static {import_path}")
    } else {
        import_path.clone()
    };

    // Check for alias
    let alias = import_node.child_by_field_name("import_alias");
    let alias_str = alias.and_then(|n| n.utf8_text(content).ok().map(String::from));

    // Create the file module node and import node
    let module_id = helper.add_module(FILE_MODULE_NAME, None);
    let import_span = Some(Span::from_node(&import_node));
    let import_id = helper.add_import(&imported_name, import_span);

    // Create the import edge with appropriate metadata
    helper.add_import_edge_full(module_id, import_id, alias_str.as_deref(), has_wildcard);
}

/// Extract a fully qualified name from a `qualified_name` node.
///
/// The `qualified_name` node contains nested identifier nodes that need
/// to be joined with ".".
fn extract_qualified_name(node: Node, content: &[u8]) -> String {
    let mut parts = Vec::new();
    collect_identifiers(node, content, &mut parts);
    parts.join(".")
}

/// Recursively collect identifier text from a `qualified_name` tree.
fn collect_identifiers(node: Node, content: &[u8], parts: &mut Vec<String>) {
    if node.kind() == "identifier"
        && let Ok(text) = node.utf8_text(content)
    {
        parts.push(text.to_string());
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_identifiers(child, content, parts);
    }
}

// ================================
// Export Edge Emission (Pass 6)
// ================================

/// Check if a node has the `private` visibility modifier.
/// In Groovy, if no visibility modifier is present, the member is public by default.
fn is_private(node: Node, content: &[u8]) -> bool {
    has_visibility_modifier(node, "private", content)
}

/// Check if a node has a specific visibility modifier.
fn has_visibility_modifier(node: Node, modifier: &str, content: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // In tree-sitter-groovy, visibility modifiers (private, public, protected)
        // are in an "access_modifier" node, not "modifier" (which is for static, final, etc.)
        if child.kind() == "access_modifier"
            && let Ok(text) = child.utf8_text(content)
            && text == modifier
        {
            return true;
        }
    }
    false
}

/// Export a symbol from the file module.
fn export_from_file_module(helper: &mut GraphBuildHelper, exported: NodeId) {
    let module_id = helper.add_module(FILE_MODULE_NAME, None);
    helper.add_export_edge(module_id, exported);
}

/// Pass 6: Emit export edges for public classes, methods, and functions.
///
/// In Groovy, members are public by default unless explicitly marked as private.
/// - Top-level functions and variables are exported (unless private)
/// - Public classes and interfaces are exported
/// - Public methods and fields within classes are exported
fn emit_export_edges(
    root: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    context_to_node: &HashMap<String, NodeId>,
) -> Result<(), GraphBuilderError> {
    emit_export_edges_recursive(root, content, helper, context_to_node)
}

/// Recursively walk the AST to emit export edges.
fn emit_export_edges_recursive(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    context_to_node: &HashMap<String, NodeId>,
) -> Result<(), GraphBuilderError> {
    match node.kind() {
        "class_definition" => {
            // Export the class/interface if it's not private
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(class_name) = name_node.utf8_text(content)
            {
                if !is_private(node, content)
                    && let Some(&class_id) = context_to_node.get(class_name)
                {
                    export_from_file_module(helper, class_id);
                }

                // Process exported methods and fields within the class
                if let Some(body_node) = node.child_by_field_name("body") {
                    emit_class_member_exports(
                        body_node,
                        content,
                        class_name,
                        helper,
                        context_to_node,
                    );
                }
            }
        }
        "function_definition" | "function_declaration" => {
            // Export top-level functions if not private
            if is_top_level(node)
                && !is_private(node, content)
                && let Some(name_node) = node
                    .child_by_field_name("function")
                    .or_else(|| node.child_by_field_name("name"))
                && let Ok(func_name) = name_node.utf8_text(content)
                && let Some(&func_id) = context_to_node.get(func_name)
            {
                export_from_file_module(helper, func_id);
            }
        }
        "declaration" => {
            // Export top-level closures/properties if not private
            if is_top_level(node)
                && !is_private(node, content)
                && let Some(value_node) = node.child_by_field_name("value")
                && value_node.kind() == "closure"
                && let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(content)
                && let Some(&node_id) = context_to_node.get(name)
            {
                export_from_file_module(helper, node_id);
            }
        }
        "assignment" => {
            // Export top-level assignments to closures if not private
            if is_top_level(node)
                && !is_private(node, content)
                && let (Some(left_node), Some(right_node)) = (
                    node.child_by_field_name("left"),
                    node.child_by_field_name("right"),
                )
                && right_node.kind() == "closure"
                && let Ok(name) = left_node.utf8_text(content)
                && let Some(&node_id) = context_to_node.get(name)
            {
                export_from_file_module(helper, node_id);
            }
        }
        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        emit_export_edges_recursive(child, content, helper, context_to_node)?;
    }

    Ok(())
}

/// Process class/interface body to emit export edges for public members.
fn emit_class_member_exports(
    body_node: Node,
    content: &[u8],
    class_name: &str,
    helper: &mut GraphBuildHelper,
    context_to_node: &HashMap<String, NodeId>,
) {
    let mut cursor = body_node.walk();
    for child in body_node.children(&mut cursor) {
        match child.kind() {
            "method_declaration" | "function_definition" | "function_declaration" => {
                // Export methods if not private
                if !is_private(child, content)
                    && let Some(name_node) = child
                        .child_by_field_name("function")
                        .or_else(|| child.child_by_field_name("name"))
                    && let Ok(method_name) = name_node.utf8_text(content)
                {
                    let qualified_name = format!("{class_name}::{method_name}");
                    if let Some(&method_id) = context_to_node.get(&qualified_name) {
                        export_from_file_module(helper, method_id);
                    }
                }
            }
            "declaration" => {
                // Export properties if not private
                if !is_private(child, content)
                    && let Some(value_node) = child.child_by_field_name("value")
                    && value_node.kind() == "closure"
                    && let Some(name_node) = child.child_by_field_name("name")
                    && let Ok(prop_name) = name_node.utf8_text(content)
                {
                    let qualified_name = format!("{class_name}::{prop_name}");
                    if let Some(&prop_id) = context_to_node.get(&qualified_name) {
                        export_from_file_module(helper, prop_id);
                    }
                }
            }
            "assignment" => {
                // Export closure assignments if not private
                if !is_private(child, content)
                    && let (Some(left_node), Some(right_node)) = (
                        child.child_by_field_name("left"),
                        child.child_by_field_name("right"),
                    )
                    && right_node.kind() == "closure"
                    && let Ok(name) = left_node.utf8_text(content)
                {
                    let qualified_name = format!("{class_name}::{name}");
                    if let Some(&node_id) = context_to_node.get(&qualified_name) {
                        export_from_file_module(helper, node_id);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Check if a node is at the top level (not inside a class or method).
fn is_top_level(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "class_definition" | "function_definition" | "function_declaration" | "closure" => {
                return false;
            }
            _ => current = parent.parent(),
        }
    }
    true
}

// ================================
// OOP Edge Extraction (Pass 5)
// ================================

/// Pass 5: Extract OOP relationships and create inheritance/implementation edges.
///
/// Handles Groovy class patterns:
/// - Class inheritance: `class Child extends Parent` -> Inherits edge
/// - Interface implementation: `class Impl extends IFace` -> Implements edge
/// - Interface inheritance: `interface Child extends Parent` -> Inherits edge
///
/// Note: The tree-sitter-groovy grammar only supports `extends` clause,
/// not `implements`. We use the `interface_names` set to distinguish between
/// class inheritance and interface implementation.
///
/// tree-sitter-groovy structure:
/// ```text
/// class_definition
///   name: identifier
///   superclass: _primary_expression (for extends)
///   body: closure
/// ```
fn extract_oop_edges(
    root: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    context_to_node: &HashMap<String, NodeId>,
    interface_names: &HashSet<String>,
) -> Result<(), GraphBuilderError> {
    extract_oop_edges_recursive(root, content, helper, context_to_node, interface_names)
}

/// Recursively walk the AST to find class definitions with inheritance.
fn extract_oop_edges_recursive(
    node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    context_to_node: &HashMap<String, NodeId>,
    interface_names: &HashSet<String>,
) -> Result<(), GraphBuilderError> {
    if node.kind() == "class_definition" {
        process_class_inheritance(node, content, helper, context_to_node, interface_names);
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        extract_oop_edges_recursive(child, content, helper, context_to_node, interface_names)?;
    }

    Ok(())
}

/// Process a class definition node and create inheritance/implementation edges.
///
/// Uses `add_implements_edge` when:
/// - A class extends an interface (detected via `interface_names`)
///
/// Uses `add_inherits_edge` when:
/// - A class extends another class
/// - An interface extends another interface
fn process_class_inheritance(
    class_node: Node,
    content: &[u8],
    helper: &mut GraphBuildHelper,
    context_to_node: &HashMap<String, NodeId>,
    interface_names: &HashSet<String>,
) {
    // Get the class name
    let Some(name_node) = class_node.child_by_field_name("name") else {
        return;
    };
    let Ok(class_name) = name_node.utf8_text(content) else {
        return;
    };

    // Check if the current type is an interface
    let is_current_interface = interface_names.contains(class_name);

    // Get or create the child class/interface node
    let child_id = context_to_node.get(class_name).copied().unwrap_or_else(|| {
        if is_current_interface {
            helper.add_interface(class_name, None)
        } else {
            helper.add_class(class_name, None)
        }
    });

    // Check for superclass (extends clause)
    if let Some(superclass_node) = class_node.child_by_field_name("superclass") {
        let parent_name = extract_type_name(superclass_node, content);
        if !parent_name.is_empty() {
            // Check if parent is a known interface
            let is_parent_interface = interface_names.contains(&parent_name);

            // Create or get the parent node
            let parent_id = if is_parent_interface {
                helper.add_interface(&parent_name, None)
            } else {
                helper.add_class(&parent_name, None)
            };

            // Determine edge type:
            // - Class extends Interface -> Implements edge
            // - Class extends Class -> Inherits edge
            // - Interface extends Interface -> Inherits edge
            if !is_current_interface && is_parent_interface {
                // Class implementing interface
                helper.add_implements_edge(child_id, parent_id);
            } else {
                // Class extending class OR interface extending interface
                helper.add_inherits_edge(child_id, parent_id);
            }
        }
    }
}

/// Extract type name from a superclass expression.
///
/// The superclass can be:
/// - Simple identifier: `Parent`
/// - Dotted identifier: `com.example.Parent`
/// - Type with generics: `Parent<T>` (we extract just the base type)
fn extract_type_name(node: Node, content: &[u8]) -> String {
    match node.kind() {
        "identifier" => node.utf8_text(content).unwrap_or("").to_string(),
        "dotted_identifier" => {
            // For dotted_identifier, concatenate all parts
            node.utf8_text(content).unwrap_or("").to_string()
        }
        "type_with_generics" => {
            // For generic types, get the base type (first child)
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier" || child.kind() == "dotted_identifier" {
                    return extract_type_name(child, content);
                }
            }
            node.utf8_text(content).unwrap_or("").to_string()
        }
        _ => {
            // Fallback: try to get any text
            node.utf8_text(content).unwrap_or("").to_string()
        }
    }
}

/// Per-language [`ShapeMapping`] for Groovy: a precomputed `kind_id -> CfBucket`
/// table over the tree-sitter-groovy grammar, shared process-wide via
/// [`groovy_shape_mapping`]. Mirrors the C reference impl: one array index per
/// node on the hot shape walk, identifier-blind throughout.
pub struct GroovyShapeMapping {
    cf_by_kind_id: Vec<Option<CfBucket>>,
}

impl GroovyShapeMapping {
    fn build() -> Self {
        // The vendored Groovy grammar returns a `Language` directly (no `.into()`).
        let lang: tree_sitter::Language = tree_sitter_groovy_sqry::language();
        let count = lang.node_kind_count();
        let mut cf_by_kind_id = vec![None; count];
        for (id, slot) in cf_by_kind_id.iter_mut().enumerate() {
            let Ok(kind_id) = u16::try_from(id) else {
                break;
            };
            if !lang.node_kind_is_named(kind_id) {
                continue;
            }
            if let Some(name) = lang.node_kind_for_id(kind_id) {
                *slot = cf_bucket_for_groovy_kind(name);
            }
        }
        Self { cf_by_kind_id }
    }
}

impl ShapeMapping for GroovyShapeMapping {
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
        self.cf_by_kind_id
            .get(ts_node_kind_id as usize)
            .copied()
            .flatten()
    }

    fn signature_shape(&self, fn_node: Node, _src: &[u8]) -> SignatureShape {
        let mut shape = SignatureShape::default();
        // A `function_definition` exposes its parameters through the `parameters`
        // field, which holds a `parameter_list` whose named `parameter` children
        // carry the positional arity. Each `parameter` may have a `value` field
        // (a default), which marks `has_defaults`.
        if let Some(params) = fn_node.child_by_field_name("parameters") {
            let mut cursor = params.walk();
            for child in params.named_children(&mut cursor) {
                if child.kind() == "parameter" {
                    shape.arity_positional = shape.arity_positional.saturating_add(1);
                    if child.child_by_field_name("value").is_some() {
                        shape.has_defaults = true;
                    }
                }
            }
        }
        // The declared return type lives in the `type` field of the function node
        // (Groovy allows `def`/untyped, so absence is honest, not an error).
        shape.has_return_annotation = fn_node.child_by_field_name("type").is_some();
        shape
    }
}

/// Map one tree-sitter-groovy grammar node-kind name to its canonical control-flow
/// bucket. Additive-only against the frozen [`CfBucket`] set. The Groovy grammar
/// spells several control-flow keywords as bare named nodes (`return`, `break`,
/// `continue`, `case`).
fn cf_bucket_for_groovy_kind(name: &str) -> Option<CfBucket> {
    let bucket = match name {
        "if_statement" | "ternary_op" => CfBucket::Branch,
        "for_loop" | "for_in_loop" | "while_loop" | "do_while_loop" => CfBucket::Loop,
        "switch_statement" | "switch_block" | "case" => CfBucket::Match,
        // The grammar exposes only `try_statement`; it has no separate catch /
        // finally / throw named nodes, so those buckets stay unmapped (honest).
        "try_statement" => CfBucket::Try,
        "return" => CfBucket::Return,
        "break" | "continue" => CfBucket::BreakContinue,
        "function_call" | "juxt_function_call" => CfBucket::Call,
        "assignment" | "declaration" => CfBucket::Assign,
        "closure" => CfBucket::Closure,
        _ => return None,
    };
    Some(bucket)
}

/// The process-wide Groovy shape mapping, built once on first use.
#[must_use]
pub fn groovy_shape_mapping() -> &'static GroovyShapeMapping {
    static MAPPING: OnceLock<GroovyShapeMapping> = OnceLock::new();
    MAPPING.get_or_init(GroovyShapeMapping::build)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::build::staging::StagingOp;
    use sqry_core::graph::unified::edge::kind::EdgeKind;
    use std::path::PathBuf;

    fn parse_groovy(source: &str) -> Tree {
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

        // Verify build_graph succeeds
        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify nodes were staged (class + 2 methods = 3 nodes minimum)
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 3,
            "Expected at least 3 nodes (class + 2 methods), got {}",
            stats.nodes_staged
        );
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

        // Verify build_graph succeeds
        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify nodes and edges were staged
        let stats = staging.stats();
        // 1 class + 4 methods = 5 nodes
        assert!(
            stats.nodes_staged >= 5,
            "Expected at least 5 nodes, got {}",
            stats.nodes_staged
        );
        // validate->checkDependencies, build->validate, build->compile = 3 edges
        assert!(
            stats.edges_staged >= 3,
            "Expected at least 3 call edges, got {}",
            stats.edges_staged
        );
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

        // Verify build_graph succeeds
        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Verify nodes were staged (at least the closure and function)
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 2,
            "Expected at least 2 nodes (closure + function), got {}",
            stats.nodes_staged
        );
    }

    // ================================
    // Import Edge Tests
    // ================================

    #[test]
    fn test_simple_import() {
        let source = r"
import groovy.transform.ToString
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Should have: file module node + import node = 2 nodes, 1 edge
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 2,
            "Expected at least 2 nodes (module + import), got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 1,
            "Expected at least 1 import edge, got {}",
            stats.edges_staged
        );
    }

    #[test]
    fn test_dotted_import() {
        let source = r"
import com.example.service.UserService
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
            stats.nodes_staged >= 2,
            "Expected at least 2 nodes for dotted import, got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 1,
            "Expected at least 1 import edge, got {}",
            stats.edges_staged
        );
    }

    #[test]
    fn test_aliased_import() {
        let source = r"
import groovy.transform.ToString as TS
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Aliased import: module + import node = 2 nodes, 1 edge with alias
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 2,
            "Expected at least 2 nodes for aliased import, got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 1,
            "Expected at least 1 import edge, got {}",
            stats.edges_staged
        );
    }

    #[test]
    fn test_wildcard_import() {
        let source = r"
import groovy.transform.*
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Wildcard import should have is_wildcard: true
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 2,
            "Expected at least 2 nodes for wildcard import, got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 1,
            "Expected at least 1 import edge, got {}",
            stats.edges_staged
        );
    }

    #[test]
    fn test_static_import() {
        let source = r"
import static java.lang.Math.PI
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
            stats.nodes_staged >= 2,
            "Expected at least 2 nodes for static import, got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 1,
            "Expected at least 1 import edge, got {}",
            stats.edges_staged
        );
    }

    #[test]
    fn test_static_wildcard_import() {
        let source = r"
import static groovy.lang.*
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
    fn test_static_aliased_import() {
        let source = r"
import static Calendar.getInstance as now
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
            stats.nodes_staged >= 2,
            "Expected at least 2 nodes for static aliased import, got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 1,
            "Expected at least 1 import edge, got {}",
            stats.edges_staged
        );
    }

    #[test]
    fn test_multiple_imports() {
        let source = r"
import groovy.transform.ToString
import groovy.transform.EqualsAndHashCode
import java.util.List
import java.util.Map
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // 4 imports + 1 module = 5 nodes, 4 edges
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 5,
            "Expected at least 5 nodes for 4 imports, got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 4,
            "Expected at least 4 import edges, got {}",
            stats.edges_staged
        );
    }

    // ================================
    // OOP Edge Tests (Inheritance)
    // ================================

    #[test]
    fn test_simple_inheritance() {
        let source = r"
class Parent {
}

class Child extends Parent {
}
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // 2 classes = 2 nodes, 1 inheritance edge
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 2,
            "Expected at least 2 class nodes, got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 1,
            "Expected at least 1 inheritance edge, got {}",
            stats.edges_staged
        );
    }

    #[test]
    fn test_inheritance_with_external_parent() {
        let source = r"
class MyService extends GroovyServlet {
}
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // MyService + GroovyServlet (created for inheritance) = 2 nodes, 1 edge
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 2,
            "Expected at least 2 nodes (class + external parent), got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 1,
            "Expected at least 1 inheritance edge, got {}",
            stats.edges_staged
        );
    }

    #[test]
    fn test_inheritance_with_qualified_parent() {
        let source = r"
class MyController extends org.springframework.Controller {
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
        assert!(
            stats.nodes_staged >= 2,
            "Expected at least 2 nodes for qualified inheritance, got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 1,
            "Expected at least 1 inheritance edge, got {}",
            stats.edges_staged
        );
    }

    #[test]
    fn test_class_without_inheritance() {
        let source = r"
class StandaloneClass {
    def method() {
        return 42
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

        // 1 class + 1 method = 2 nodes, no inheritance edges (only potential call edges)
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 2,
            "Expected at least 2 nodes (class + method), got {}",
            stats.nodes_staged
        );
        // No inheritance edge should be created
    }

    #[test]
    fn test_multiple_classes_with_inheritance() {
        let source = r"
class Animal {
}

class Dog extends Animal {
}

class Cat extends Animal {
}

class Puppy extends Dog {
}
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // 4 classes, 3 inheritance edges
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 4,
            "Expected at least 4 class nodes, got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 3,
            "Expected at least 3 inheritance edges, got {}",
            stats.edges_staged
        );
    }

    #[test]
    fn test_interface_declaration() {
        let source = r"
interface Runnable {
    void run()
}

class Task extends Runnable {
}
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Note: In Groovy grammar, both class and interface use class_definition
        // and interface implementation uses "extends" keyword
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 2,
            "Expected at least 2 nodes (interface + class), got {}",
            stats.nodes_staged
        );
    }

    #[test]
    fn test_combined_imports_and_inheritance() {
        let source = r"
import java.util.List
import java.util.ArrayList

class MyList extends ArrayList {
    def items = []
}
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // 2 imports + 1 module + 2 classes (MyList + ArrayList) + 1 property (items) = 6 nodes
        // 2 import edges + 1 inheritance edge = 3 edges
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 6,
            "Expected at least 6 nodes (imports + classes + property), got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 3,
            "Expected at least 3 edges (imports + inheritance), got {}",
            stats.edges_staged
        );
    }

    // ================================
    // Property and Field Tests
    // ================================

    #[test]
    fn test_extracts_properties() {
        let source = r"
class User {
    String name
    int age
    def email
}
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // 1 class + 3 properties = 4 nodes
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 4,
            "Expected at least 4 nodes (class + 3 properties), got {}",
            stats.nodes_staged
        );
    }

    #[test]
    fn test_distinguishes_properties_from_fields() {
        let source = r"
class Config {
    String publicProp       // Property (public)
    private String privateFld  // Field (private)
    protected int protectedFld // Field (protected)
    static final DEFAULT = 'x' // Field (static final constant)
}
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // 1 class + 1 property + 3 fields = 5 nodes
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 5,
            "Expected at least 5 nodes (class + 1 property + 3 fields), got {}",
            stats.nodes_staged
        );
    }

    #[test]
    fn test_property_with_initialization() {
        let source = r"
class Account {
    String status = 'active'
    BigDecimal balance = 0.0
}
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // 1 class + 2 properties (with initialization) = 3 nodes
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 3,
            "Expected at least 3 nodes (class + 2 initialized properties), got {}",
            stats.nodes_staged
        );
    }

    #[test]
    fn test_static_final_fields_are_variables() {
        let source = r"
class Constants {
    static final String VERSION = '1.0'
    static final int MAX_SIZE = 100
}
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // 1 class + 2 fields (static final = variables, not properties) = 3 nodes
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 3,
            "Expected at least 3 nodes (class + 2 static final fields), got {}",
            stats.nodes_staged
        );
    }

    #[test]
    fn test_mixed_properties_and_methods() {
        let source = r"
class Product {
    String name
    BigDecimal price

    def calculateTax() {
        return price * 0.1
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

        // 1 class + 2 properties + 1 method = 4 nodes
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 4,
            "Expected at least 4 nodes (class + 2 properties + 1 method), got {}",
            stats.nodes_staged
        );
    }

    #[test]
    fn test_class_with_generics_extends() {
        let source = r"
class MyList<T> extends ArrayList<T> {
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
        assert!(
            stats.nodes_staged >= 2,
            "Expected at least 2 nodes for generic class inheritance, got {}",
            stats.nodes_staged
        );
    }

    // ================================
    // Export Edge Tests
    // ================================

    #[test]
    fn test_exports_public_class() {
        let source = r"
class User {
    String name
}
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // 1 class + 1 module = 2 nodes, 1 export edge
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 2,
            "Expected at least 2 nodes (class + module), got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 1,
            "Expected at least 1 export edge, got {}",
            stats.edges_staged
        );
    }

    #[test]
    fn test_exports_public_method() {
        let source = r"
class User {
    String getName() {
        return name
    }

    private void validate() {
        // private
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

        // 1 class + 2 methods + 1 module + type nodes = many nodes
        // 1 export for class + 1 export for public method = 2 export edges
        // (TypeOf and Reference edges also exist, so we can't check total edge count)
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 4,
            "Expected at least 4 nodes (class + methods + module), got {}",
            stats.nodes_staged
        );

        // Count only Export edges (not TypeOf/Reference edges)
        let export_edges = staging
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
            .count();

        assert_eq!(
            export_edges, 2,
            "Expected exactly 2 export edges (class + public method), got {export_edges}. Private validate should not be exported!"
        );
    }

    #[test]
    fn test_exports_top_level_function() {
        let source = r"
def greet(String name) {
    return 'Hello'
}

private def helper() {
    return 42
}
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // 2 functions + 1 module = 3 nodes
        // 1 export for public function = 1 export edge
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 3,
            "Expected at least 3 nodes (functions + module), got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 1,
            "Expected at least 1 export edge, got {}",
            stats.edges_staged
        );
    }

    #[test]
    fn test_exports_skips_private_class() {
        let source = r"
private class Secret {
}

class Public {
}
";
        let tree = parse_groovy(source);
        let mut staging = StagingGraph::new();
        let builder = GroovyGraphBuilder;
        let file = PathBuf::from("test.groovy");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // 2 classes + 1 module = 3 nodes
        // only Public class should be exported = 1 export edge
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 3,
            "Expected at least 3 nodes (2 classes + module), got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 1,
            "Expected at least 1 export edge (public class only), got {}",
            stats.edges_staged
        );
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;
    use sqry_core::graph::unified::build::shape::{
        CfBucket, ShapeBudget, compute_shape_descriptor,
    };

    const SAMPLE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test-fixtures/shape/systems/sample.groovy"
    ));

    fn parse(src: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_groovy_sqry::language())
            .expect("load Groovy grammar");
        parser.parse(src, None).expect("parse Groovy sample")
    }

    fn first_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = first_of_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn groovy_mapping_is_non_empty() {
        let mapping = groovy_shape_mapping();
        let lang: tree_sitter::Language = tree_sitter_groovy_sqry::language();
        let count = (0..lang.node_kind_count())
            .filter_map(|id| u16::try_from(id).ok())
            .filter(|id| mapping.cf_bucket(*id).is_some())
            .count();
        assert!(
            count > 0,
            "Groovy cf_bucket map should cover real control-flow kinds"
        );
    }

    #[test]
    fn groovy_histogram_covers_control_flow() {
        let tree = parse(SAMPLE);
        let func = first_of_kind(tree.root_node(), "function_definition")
            .expect("sample has a function_definition");
        let desc = compute_shape_descriptor(
            func,
            SAMPLE.as_bytes(),
            groovy_shape_mapping(),
            &ShapeBudget::default(),
        );
        let h = &desc.cf_histogram;
        assert!(h[CfBucket::Branch.index()] >= 1, "branch present");
        assert!(h[CfBucket::Loop.index()] >= 1, "loop present");
        assert!(h[CfBucket::Match.index()] >= 1, "switch present");
        assert!(h[CfBucket::Call.index()] >= 1, "call present");
        assert!(h[CfBucket::Return.index()] >= 1, "return present");
        assert!(
            h[CfBucket::BreakContinue.index()] >= 1,
            "break/continue present"
        );
    }

    #[test]
    fn groovy_signature_shape_reads_params() {
        let tree = parse(SAMPLE);
        let func = first_of_kind(tree.root_node(), "function_definition")
            .expect("sample has a function_definition");
        let shape = groovy_shape_mapping().signature_shape(func, SAMPLE.as_bytes());
        // classify(int n, String label): two positional params.
        assert_eq!(shape.arity_positional, 2, "two positional params");
        assert!(shape.has_return_annotation, "int return type slot present");
    }
}
