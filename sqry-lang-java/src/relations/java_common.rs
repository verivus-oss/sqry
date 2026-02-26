//! Java relation extraction helpers
//! Migrated from relations-shared/src/hooks/java/common.rs (FR-2025-021)

use sqry_lang_support::relations::{build_qualified_name, normalize_type_signature};
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Tree};

/// Context for Java relation extraction
#[derive(Debug)]
pub struct JavaRelationContext<'a> {
    /// Path to the file currently being processed.
    pub file: &'a Path,
    /// Optional package declaration discovered in the file.
    pub package_name: Option<String>,
}

impl<'a> JavaRelationContext<'a> {
    /// Construct a new context with an optional package name.
    #[must_use]
    pub fn new(file: &'a Path, package_name: Option<String>) -> Self {
        Self { file, package_name }
    }
}

/// Package resolution for Java symbols
#[derive(Debug, Clone)]
pub struct PackageResolver {
    source_root: PathBuf,
}

impl PackageResolver {
    /// Create a resolver rooted at the provided source directory.
    pub fn new(source_root: impl Into<PathBuf>) -> Self {
        Self {
            source_root: source_root.into(),
        }
    }

    /// Convert a Java package name (e.g., `com.example.app`) into a relative path.
    #[must_use]
    pub fn package_to_path(&self, package_name: &str) -> PathBuf {
        let relative = package_name.replace('.', std::path::MAIN_SEPARATOR_STR);
        self.source_root.join(relative)
    }

    /// Extract package name directly from the AST, when present.
    #[must_use]
    pub fn package_from_ast(tree: &Tree, content: &[u8]) -> Option<String> {
        let root = tree.root_node();
        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            if child.kind() != "package_declaration" {
                continue;
            }
            if let Some(candidate) = extract_package_name(child, content) {
                return Some(candidate);
            }
        }
        None
    }
}

fn extract_package_name(node: Node<'_>, content: &[u8]) -> Option<String> {
    if let Some(candidate) = node
        .child_by_field_name("name")
        .and_then(|name_node| extract_package_candidate(name_node, content))
    {
        return Some(candidate);
    }

    let mut name_cursor = node.walk();
    node.named_children(&mut name_cursor)
        .filter(|child| matches!(child.kind(), "scoped_identifier" | "identifier"))
        .find_map(|child| extract_package_candidate(child, content))
}

fn extract_package_candidate(node: Node<'_>, content: &[u8]) -> Option<String> {
    node.utf8_text(content)
        .ok()
        .and_then(normalize_package_name)
}

fn normalize_package_name(text: &str) -> Option<String> {
    let candidate: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if candidate.is_empty() {
        None
    } else {
        Some(candidate)
    }
}

/// Build a member symbol (method, field)
#[must_use]
pub fn build_member_symbol(package: Option<&str>, class_stack: &[String], member: &str) -> String {
    build_qualified_name(
        package,
        class_stack.iter().map(std::string::String::as_str),
        member,
    )
}

/// Build a symbol (class, interface)
#[must_use]
pub fn build_symbol(package: Option<&str>, class_stack: &[String], name: &str) -> String {
    build_qualified_name(
        package,
        class_stack.iter().map(std::string::String::as_str),
        name,
    )
}

/// Normalize Java type names
#[must_use]
pub fn normalize_type_name(node: &Node, content: &[u8]) -> String {
    let raw = node.utf8_text(content).unwrap_or_default();
    normalize_type_signature(raw)
}
