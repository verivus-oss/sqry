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
        // Delegates to `short_name` so the emitted spelling has exactly one
        // definition. This is the legacy wire form (`ts`, `js`, `py`) and is
        // deliberately unchanged: manifest confidence keys and textual
        // `NodeId` both persist it (issue #714).
        f.write_str(self.short_name())
    }
}

impl Language {
    /// Every language variant, in declaration order.
    ///
    /// The single list every generated surface derives from: the query
    /// field registry's accepted values, validation help text, and the
    /// documented language table. Hand-maintained copies drift, this does
    /// not.
    pub const ALL: &'static [Self] = &[
        Self::C,
        Self::Cpp,
        Self::CSharp,
        Self::Css,
        Self::JavaScript,
        Self::Python,
        Self::TypeScript,
        Self::Rust,
        Self::Go,
        Self::Java,
        Self::Ruby,
        Self::Php,
        Self::Swift,
        Self::Kotlin,
        Self::Scala,
        Self::Sql,
        Self::Dart,
        Self::Lua,
        Self::Perl,
        Self::Shell,
        Self::Groovy,
        Self::Elixir,
        Self::R,
        Self::Haskell,
        Self::Html,
        Self::Svelte,
        Self::Vue,
        Self::Zig,
        Self::Terraform,
        Self::Puppet,
        Self::Pulumi,
        Self::Http,
        Self::Plsql,
        Self::Apex,
        Self::Abap,
        Self::ServiceNow,
        Self::Json,
    ];

    /// The canonical machine identifier for this language.
    ///
    /// This is the vocabulary every query predicate and machine-facing
    /// filter speaks (`typescript`, not `ts`). Distinct from
    /// [`Self::short_name`], which is the legacy display spelling.
    #[must_use]
    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::CSharp => "csharp",
            Self::Css => "css",
            Self::JavaScript => "javascript",
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Java => "java",
            Self::Ruby => "ruby",
            Self::Php => "php",
            Self::Swift => "swift",
            Self::Kotlin => "kotlin",
            Self::Scala => "scala",
            Self::Sql => "sql",
            Self::Dart => "dart",
            Self::Lua => "lua",
            Self::Perl => "perl",
            Self::Shell => "shell",
            Self::Groovy => "groovy",
            Self::Elixir => "elixir",
            Self::R => "r",
            Self::Haskell => "haskell",
            Self::Html => "html",
            Self::Svelte => "svelte",
            Self::Vue => "vue",
            Self::Zig => "zig",
            Self::Terraform => "terraform",
            Self::Puppet => "puppet",
            Self::Pulumi => "pulumi",
            Self::Http => "http",
            Self::Plsql => "plsql",
            Self::Apex => "apex",
            Self::Abap => "abap",
            Self::ServiceNow => "servicenow",
            Self::Json => "json",
        }
    }

    /// The short display spelling, as emitted by [`fmt::Display`].
    ///
    /// Differs from [`Self::canonical_name`] only for JavaScript (`js`),
    /// Python (`py`), and TypeScript (`ts`). Kept as a separate concept
    /// because it is persisted in graph manifests and textual node ids, so
    /// it cannot be changed without a migration.
    #[must_use]
    pub const fn short_name(self) -> &'static str {
        match self {
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::CSharp => "csharp",
            Self::Css => "css",
            Self::JavaScript => "js",
            Self::Python => "py",
            Self::TypeScript => "ts",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Java => "java",
            Self::Ruby => "ruby",
            Self::Php => "php",
            Self::Swift => "swift",
            Self::Kotlin => "kotlin",
            Self::Scala => "scala",
            Self::Sql => "sql",
            Self::Dart => "dart",
            Self::Lua => "lua",
            Self::Perl => "perl",
            Self::Shell => "shell",
            Self::Groovy => "groovy",
            Self::Elixir => "elixir",
            Self::R => "r",
            Self::Haskell => "haskell",
            Self::Html => "html",
            Self::Svelte => "svelte",
            Self::Vue => "vue",
            Self::Zig => "zig",
            Self::Terraform => "terraform",
            Self::Puppet => "puppet",
            Self::Pulumi => "pulumi",
            Self::Http => "http",
            Self::Plsql => "plsql",
            Self::Apex => "apex",
            Self::Abap => "abap",
            Self::ServiceNow => "servicenow",
            Self::Json => "json",
        }
    }

    /// Additional accepted spellings beyond the canonical and short names.
    ///
    /// [`Self::from_id`] accepts the union of canonical, short, and these,
    /// so an alias added here is immediately valid everywhere input is
    /// parsed.
    #[must_use]
    pub const fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::C => &[],
            Self::Cpp => &["c++", "cplusplus", "cxx"],
            Self::CSharp => &["c#", "cs"],
            Self::Css => &[],
            Self::JavaScript => &[],
            Self::Python => &[],
            Self::TypeScript => &[],
            Self::Rust => &["rs"],
            Self::Go => &["golang"],
            Self::Java => &[],
            Self::Ruby => &["rb"],
            Self::Php => &[],
            Self::Swift => &[],
            Self::Kotlin => &["kt"],
            Self::Scala => &[],
            Self::Sql => &[],
            Self::Dart => &[],
            Self::Lua => &[],
            Self::Perl => &["pl"],
            Self::Shell => &["bash", "sh"],
            Self::Groovy => &[],
            Self::Elixir => &["ex", "exs"],
            Self::R => &[],
            Self::Haskell => &["hs"],
            Self::Html => &["html5"],
            Self::Svelte => &[],
            Self::Vue => &[],
            Self::Zig => &[],
            Self::Terraform => &["hcl", "tf"],
            Self::Puppet => &[],
            Self::Pulumi => &[],
            Self::Http => &[],
            Self::Plsql => &["pl/sql", "oracle"],
            Self::Apex => &["salesforce"],
            Self::Abap => &[],
            Self::ServiceNow => &["xanadu"],
            Self::Json => &[],
        }
    }

    /// Every spelling this language accepts on input, canonical first.
    #[must_use]
    pub fn accepted_names(self) -> Vec<&'static str> {
        let mut names = vec![self.canonical_name()];
        if self.short_name() != self.canonical_name() {
            names.push(self.short_name());
        }
        names.extend_from_slice(self.aliases());
        names
    }

    /// The canonical identifier of every language, in declaration order.
    ///
    /// Feeds the `lang` field registry and the documented value table so
    /// neither can fall out of step with the enum.
    #[must_use]
    pub fn canonical_names() -> Vec<&'static str> {
        Self::ALL.iter().map(|lang| lang.canonical_name()).collect()
    }

    /// Parse a language identifier or common alias into a `Language`.
    ///
    /// Accepts the canonical name, the short display name, and every alias,
    /// after trimming and case folding. This is the only input parser: every
    /// surface that accepts a user-supplied language routes through it, so
    /// no surface can accept a different set of spellings than another
    /// (issue #714).
    #[must_use]
    pub fn from_id(value: &str) -> Option<Self> {
        let needle = value.trim().to_ascii_lowercase();
        Self::ALL.iter().copied().find(|lang| {
            lang.canonical_name() == needle
                || lang.short_name() == needle
                || lang.aliases().contains(&needle.as_str())
        })
    }
}

/// Universal node identifier with string interning for memory efficiency
///
/// Per AGENTS.md:149-151, uses `Arc<str>` to reduce memory usage for
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

    /// Create a span from a tree-sitter node's real row and column.
    ///
    /// This is what a plugin should call when it has the node in hand, which
    /// is nearly always. It takes the node by reference to match the private
    /// extension traits that the cpp, kotlin and sql plugins had each already
    /// written for themselves, which this replaces.
    ///
    /// Prefer it over [`Span::from_bytes`] without
    /// exception: the two return the same type and look equally correct at a
    /// call site, but `from_bytes` stores byte offsets in the line and column
    /// fields, so anything built from it reports line 1 with a nonsense
    /// column.
    #[must_use]
    pub fn from_node(node: &tree_sitter::Node<'_>) -> Self {
        let start = node.start_position();
        let end = node.end_position();
        Self {
            start: Position {
                line: start.row,
                column: start.column,
            },
            end: Position {
                line: end.row,
                column: end.column,
            },
        }
    }

    /// Create a span from raw byte offsets, WITHOUT resolving them to a line
    /// and column.
    ///
    /// This is lossy and the loss is invisible: it stores the offsets in the
    /// `line`/`column` fields and sets `line: 0`, which every display path
    /// renders as line 1. A `Span` built this way is indistinguishable by type
    /// from a correct one, which is how ten language plugins came to report
    /// every declaration at line 1 with the byte offset as the column, found
    /// by audit on 2026-08-22.
    ///
    /// Use [`Span::from_node`] when a node is available. This remains only for
    /// call sites that genuinely hold nothing but offsets, such as a scope or
    /// call-context tuple, and whose spans are not surfaced as symbol
    /// positions.
    ///
    /// Deprecated so the rule is enforced by the compiler rather than by this
    /// comment. Every language plugin that used to build declaration spans this
    /// way reported line 1 with a byte offset in the column (issue #725), and a
    /// new plugin reaching for it would reintroduce that. There are now ZERO
    /// call sites: do not add `#[allow(deprecated)]` to create one. Use
    /// [`Span::from_node`] when a node is in hand, or `LineIndex::span` when
    /// only byte offsets are.
    #[must_use]
    #[deprecated(
        since = "31.0.0",
        note = "builds a line-1 span with the byte offset in the column; use Span::from_node, or LineIndex::span when only offsets are available"
    )]
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
    fn every_variant_round_trips_through_from_id() {
        // The invariant that makes the #714 bug class unrepresentable: every
        // spelling a language publishes must parse back to that language.
        for &lang in Language::ALL {
            assert_eq!(
                Language::from_id(lang.canonical_name()),
                Some(lang),
                "{} canonical name does not round-trip",
                lang.canonical_name()
            );
            assert_eq!(
                Language::from_id(lang.short_name()),
                Some(lang),
                "{} short name does not round-trip",
                lang.short_name()
            );
            for alias in lang.aliases() {
                assert_eq!(
                    Language::from_id(alias),
                    Some(lang),
                    "alias {alias} does not round-trip"
                );
            }
            // Case and surrounding whitespace must not change the answer.
            assert_eq!(
                Language::from_id(&format!("  {}  ", lang.canonical_name().to_uppercase())),
                Some(lang)
            );
        }
    }

    #[test]
    fn language_all_is_complete_and_unique() {
        assert_eq!(Language::ALL.len(), 37);
        let names = Language::canonical_names();
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "canonical names must be unique");
    }

    #[test]
    fn canonical_and_short_names_are_pinned() {
        // Guards both directions of the split that caused #714. The canonical
        // name is what every predicate and machine-facing filter speaks; the
        // short name is persisted in manifest confidence keys and textual
        // NodeIds, so neither may drift silently.
        assert_eq!(Language::TypeScript.canonical_name(), "typescript");
        assert_eq!(Language::TypeScript.short_name(), "ts");
        assert_eq!(Language::JavaScript.canonical_name(), "javascript");
        assert_eq!(Language::JavaScript.short_name(), "js");
        assert_eq!(Language::Python.canonical_name(), "python");
        assert_eq!(Language::Python.short_name(), "py");
        // Display is the short name, unchanged: it is a persisted contract.
        assert_eq!(Language::TypeScript.to_string(), "ts");
        assert_eq!(Language::JavaScript.to_string(), "js");
        assert_eq!(Language::Python.to_string(), "py");
    }

    #[test]
    fn unknown_is_not_a_language() {
        // Several surfaces map a language-less file to the literal "unknown".
        // It must never become a Language variant, or those surfaces would
        // start conflating "no language" with a real one.
        assert_eq!(Language::from_id("unknown"), None);
        assert_eq!(Language::from_id("bogus"), None);
        assert_eq!(Language::from_id(""), None);
        // Plugin ids are not language ids.
        assert_eq!(Language::from_id("servicenow-xanadu"), None);
        assert_eq!(Language::from_id("servicenow-xml"), None);
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
