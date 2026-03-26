//! Node types for the unified code graph
//!
//! This module defines the core node types that represent code entities
//! (functions, classes, modules, etc.) in the unified graph architecture.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

/// Language identifier
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub enum Language {
    /// C language
    C,
    /// C++ language
    Cpp,
    /// C# language
    CSharp,
    /// CSS language
    Css,
    /// JavaScript language
    JavaScript,
    /// Python language
    Python,
    /// TypeScript language
    TypeScript,
    /// Rust language
    Rust,
    /// Go language
    Go,
    /// Java language
    Java,
    /// Ruby language
    Ruby,
    /// PHP language
    Php,
    /// Swift language
    Swift,
    /// Kotlin language
    Kotlin,
    /// Scala language
    Scala,
    /// SQL language
    Sql,
    /// Dart language
    Dart,
    /// Lua language
    Lua,
    /// Perl language
    Perl,
    /// Shell (Bash) language
    Shell,
    /// Groovy language
    Groovy,
    /// Elixir language
    Elixir,
    /// R language
    R,
    /// Haskell language
    Haskell,
    /// HTML language
    Html,
    /// Svelte language
    Svelte,
    /// Vue language
    Vue,
    /// Zig language
    Zig,
    /// Terraform (HCL) language
    Terraform,
    /// Puppet language
    Puppet,
    /// Pulumi language
    Pulumi,
    /// Virtual language for HTTP endpoints
    Http,
    /// Oracle PL/SQL language
    Plsql,
    /// Salesforce Apex language
    Apex,
    /// SAP ABAP language
    Abap,
    /// `ServiceNow` (Xanadu) language
    ServiceNow,
    /// JSON configuration files
    Json,
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Language::C => write!(f, "c"),
            Language::Cpp => write!(f, "cpp"),
            Language::CSharp => write!(f, "csharp"),
            Language::Css => write!(f, "css"),
            Language::JavaScript => write!(f, "js"),
            Language::Python => write!(f, "py"),
            Language::TypeScript => write!(f, "ts"),
            Language::Rust => write!(f, "rust"),
            Language::Go => write!(f, "go"),
            Language::Java => write!(f, "java"),
            Language::Ruby => write!(f, "ruby"),
            Language::Php => write!(f, "php"),
            Language::Swift => write!(f, "swift"),
            Language::Kotlin => write!(f, "kotlin"),
            Language::Scala => write!(f, "scala"),
            Language::Sql => write!(f, "sql"),
            Language::Dart => write!(f, "dart"),
            Language::Lua => write!(f, "lua"),
            Language::Perl => write!(f, "perl"),
            Language::Shell => write!(f, "shell"),
            Language::Groovy => write!(f, "groovy"),
            Language::Elixir => write!(f, "elixir"),
            Language::R => write!(f, "r"),
            Language::Haskell => write!(f, "haskell"),
            Language::Html => write!(f, "html"),
            Language::Svelte => write!(f, "svelte"),
            Language::Vue => write!(f, "vue"),
            Language::Zig => write!(f, "zig"),
            Language::Terraform => write!(f, "terraform"),
            Language::Puppet => write!(f, "puppet"),
            Language::Pulumi => write!(f, "pulumi"),
            Language::Http => write!(f, "http"),
            Language::Plsql => write!(f, "plsql"),
            Language::Apex => write!(f, "apex"),
            Language::Abap => write!(f, "abap"),
            Language::ServiceNow => write!(f, "servicenow"),
            Language::Json => write!(f, "json"),
        }
    }
}

impl Language {
    /// Parse a language identifier or common alias into a `Language`.
    #[must_use]
    pub fn from_id(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "c" => Some(Self::C),
            "cpp" | "c++" => Some(Self::Cpp),
            "csharp" | "c#" | "cs" => Some(Self::CSharp),
            "css" => Some(Self::Css),
            "javascript" | "js" => Some(Self::JavaScript),
            "python" | "py" => Some(Self::Python),
            "typescript" | "ts" => Some(Self::TypeScript),
            "rust" | "rs" => Some(Self::Rust),
            "go" | "golang" => Some(Self::Go),
            "java" => Some(Self::Java),
            "ruby" | "rb" => Some(Self::Ruby),
            "php" => Some(Self::Php),
            "swift" => Some(Self::Swift),
            "kotlin" | "kt" => Some(Self::Kotlin),
            "scala" => Some(Self::Scala),
            "sql" => Some(Self::Sql),
            "dart" => Some(Self::Dart),
            "lua" => Some(Self::Lua),
            "perl" | "pl" => Some(Self::Perl),
            "shell" | "bash" | "sh" => Some(Self::Shell),
            "groovy" => Some(Self::Groovy),
            "elixir" | "ex" | "exs" => Some(Self::Elixir),
            "r" => Some(Self::R),
            "haskell" | "hs" => Some(Self::Haskell),
            "html" => Some(Self::Html),
            "svelte" => Some(Self::Svelte),
            "vue" => Some(Self::Vue),
            "zig" => Some(Self::Zig),
            "terraform" | "hcl" => Some(Self::Terraform),
            "puppet" => Some(Self::Puppet),
            "pulumi" => Some(Self::Pulumi),
            "http" => Some(Self::Http),
            "plsql" => Some(Self::Plsql),
            "apex" | "salesforce" => Some(Self::Apex),
            "abap" => Some(Self::Abap),
            "servicenow" => Some(Self::ServiceNow),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// Universal node identifier with string interning for memory efficiency
///
/// symbol-heavy data structures (saves 10-50 MB for typical repos).
///
/// # Examples
///
/// ```
/// use sqry_core::graph::node::{NodeId, Language};
/// use std::sync::Arc;
///
/// let node_id = NodeId::new(
///     Language::Cpp,
///     "src/main.cpp",
///     "main"
/// );
///
/// // Arc<str> makes cloning cheap (only refcount increment)
/// let cloned = node_id.clone();
/// assert_eq!(node_id, cloned);
/// ```
#[derive(Debug, Clone, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub struct NodeId {
    /// Language of origin
    pub language: Language,
    /// File path (interned via `Arc<str>`)
    pub file: Arc<str>,
    /// Qualified name (interned via `Arc<str>`)
    /// Examples: "`std::vector::push_back`", "MyClass.process", "__main__"
    pub qualified_name: Arc<str>,
}

impl NodeId {
    /// Create a new `NodeId` with string interning
    ///
    /// Automatically interns strings via `Arc<str>` for memory efficiency.
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::graph::node::{NodeId, Language};
    ///
    /// let id = NodeId::new(Language::Python, "api.py", "User.authenticate");
    /// println!("{}", id); // "py:api.py:User.authenticate"
    /// ```
    pub fn new(language: Language, file: impl AsRef<str>, qualified_name: impl AsRef<str>) -> Self {
        Self {
            language,
            file: Arc::from(file.as_ref()),
            qualified_name: Arc::from(qualified_name.as_ref()),
        }
    }

    /// Get the symbol name without namespace qualification
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::graph::node::{NodeId, Language};
    ///
    /// let id = NodeId::new(Language::Cpp, "main.cpp", "std::vector::push_back");
    /// assert_eq!(id.symbol_name(), "push_back");
    /// ```
    #[must_use]
    pub fn symbol_name(&self) -> &str {
        // Try C++ style first (::), then Python/Java style (.)
        if let Some(name) = self.qualified_name.rsplit("::").next()
            && name != self.qualified_name.as_ref()
        {
            return name;
        }

        if let Some(name) = self.qualified_name.rsplit('.').next() {
            return name;
        }

        &self.qualified_name
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:{}:{}", self.language, self.file, self.qualified_name)
    }
}

/// Source code span (line and column information)
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Default, Serialize, Deserialize)]
pub struct Span {
    /// Starting position
    pub start: Position,
    /// Ending position
    pub end: Position,
}

impl Span {
    /// Create a new span
    #[must_use]
    pub fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    /// Create a span from byte offsets (legacy compatibility)
    #[must_use]
    pub fn from_bytes(start: usize, end: usize) -> Self {
        Self {
            start: Position {
                line: 0,
                column: start,
            },
            end: Position {
                line: 0,
                column: end,
            },
        }
    }
}

/// Position in source code (line and column)
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Default, Serialize, Deserialize)]
pub struct Position {
    /// Line number (0-indexed)
    pub line: usize,
    /// Column number (0-indexed)
    pub column: usize,
}

impl Position {
    /// Create a new position
    #[must_use]
    pub fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

/// Type of code entity
#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    /// Function or method
    Function {
        /// Function parameters
        params: Vec<Param>,
        /// Return type (if known)
        return_type: Option<Type>,
        /// Whether the function is async
        is_async: bool,
    },
    /// Class or struct
    Class {
        /// Base classes
        bases: Vec<NodeId>,
        /// Implemented interfaces
        interfaces: Vec<NodeId>,
    },
    /// Module or namespace
    Module {
        /// Exported symbols
        exports: Vec<NodeId>,
    },
    /// Variable, constant, or field
    Variable {
        /// Variable type (if known)
        var_type: Option<Type>,
    },
}

/// Function parameter
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// Parameter name
    pub name: String,
    /// Parameter type (if known)
    pub param_type: Option<Type>,
}

/// Type information (simplified for now)
#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    /// Type name
    pub name: String,
}

/// Additional metadata for a node
#[derive(Debug, Clone, Default)]
pub struct NodeMetadata {
    /// Visibility (public, private, etc.)
    pub visibility: Option<String>,
    /// Documentation string
    pub doc_comment: Option<String>,
    /// Attributes/decorators
    pub attributes: Vec<String>,
}

/// A node in the code graph representing a code entity
#[derive(Debug, Clone)]
pub struct CodeNode {
    /// Unique identifier
    pub id: NodeId,
    /// Node type (function, class, module, etc.)
    pub kind: NodeKind,
    /// Source location
    pub span: Span,
    /// Additional metadata
    pub metadata: NodeMetadata,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_id_creation() {
        let id = NodeId::new(Language::Cpp, "src/main.cpp", "main");
        assert_eq!(id.language, Language::Cpp);
        assert_eq!(id.file.as_ref(), "src/main.cpp");
        assert_eq!(id.qualified_name.as_ref(), "main");
    }

    #[test]
    fn test_node_id_display() {
        let id = NodeId::new(Language::Python, "api.py", "User.authenticate");
        assert_eq!(id.to_string(), "py:api.py:User.authenticate");
    }

    #[test]
    fn test_node_id_hash() {
        use std::collections::HashSet;

        let id1 = NodeId::new(Language::JavaScript, "api.js", "fetchUsers");
        let id2 = NodeId::new(Language::JavaScript, "api.js", "fetchUsers");
        let id3 = NodeId::new(Language::JavaScript, "api.js", "createUser");

        let mut set = HashSet::new();
        set.insert(id1.clone());
        set.insert(id2.clone());
        set.insert(id3.clone());

        assert_eq!(set.len(), 2); // id1 and id2 are equal
    }

    #[test]
    fn test_node_id_clone_cheap() {
        let id1 = NodeId::new(Language::Cpp, "src/utils.cpp", "std::vector::push_back");
        let id2 = id1.clone();

        // Arc<str> means the underlying string is NOT copied
        assert_eq!(Arc::as_ptr(&id1.file), Arc::as_ptr(&id2.file));
        assert_eq!(
            Arc::as_ptr(&id1.qualified_name),
            Arc::as_ptr(&id2.qualified_name)
        );
    }

    #[test]
    fn test_symbol_name_extraction() {
        let id1 = NodeId::new(Language::Cpp, "main.cpp", "std::vector::push_back");
        assert_eq!(id1.symbol_name(), "push_back");

        let id2 = NodeId::new(Language::Python, "api.py", "User.authenticate");
        assert_eq!(id2.symbol_name(), "authenticate");

        let id3 = NodeId::new(Language::JavaScript, "api.js", "fetchUsers");
        assert_eq!(id3.symbol_name(), "fetchUsers");
    }

    #[test]
    fn test_span_creation() {
        let span = Span::new(Position::new(10, 0), Position::new(20, 1));

        assert_eq!(span.start.line, 10);
        assert_eq!(span.end.line, 20);
    }

    #[test]
    fn test_language_display() {
        assert_eq!(Language::Cpp.to_string(), "cpp");
        assert_eq!(Language::JavaScript.to_string(), "js");
        assert_eq!(Language::Python.to_string(), "py");
        assert_eq!(Language::Ruby.to_string(), "ruby");
        assert_eq!(Language::Php.to_string(), "php");
        assert_eq!(Language::Swift.to_string(), "swift");
        assert_eq!(Language::Kotlin.to_string(), "kotlin");
        assert_eq!(Language::Scala.to_string(), "scala");
        assert_eq!(Language::Http.to_string(), "http");
    }

    #[test]
    fn test_language_from_id() {
        assert_eq!(Language::from_id("javascript"), Some(Language::JavaScript));
        assert_eq!(Language::from_id("js"), Some(Language::JavaScript));
        assert_eq!(Language::from_id("c#"), Some(Language::CSharp));
        assert_eq!(Language::from_id("rb"), Some(Language::Ruby));
        assert_eq!(Language::from_id("json"), Some(Language::Json));
        assert_eq!(Language::from_id("unknown"), None);
    }
}
