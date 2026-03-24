//! AST query system for semantic code search
//!
//! This module provides a query language for searching code by AST structure,
//! not just text patterns. It enables queries like:
//!
//! - `kind:function AND name~=^test_` - Find test functions
//! - `kind:method AND parent:class` - Find methods in classes
//! - `kind:function AND depth:>3` - Find deeply nested functions
//!
//! # Architecture
//!
//! The AST query system consists of three main components:
//!
//! - **types**: Core types (Context, ContextualMatch, ContextKind)
//! - **context**: Context extraction from ASTs (parent, ancestors, depth)
//! - **query**: Query parser and evaluator (predicate language + matching)
//!
//! # Examples
//!
//! ## Basic Query
//!
//! ```rust,no_run
//! use sqry_core::ast::{parse_query, ContextExtractor};
//! use std::path::Path;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Parse query
//! let query = parse_query("kind:function AND name~=test")?;
//!
//! // Extract context from file
//! let extractor = ContextExtractor::new();
//! let matches = extractor.extract_from_file(Path::new("src/lib.rs"))?;
//!
//! // Filter by query
//! let filtered: Vec<_> = matches.into_iter()
//!     .filter(|m| query.matches(&m))
//!     .collect();
//!
//! println!("Found {} test functions", filtered.len());
//! # Ok(())
//! # }
//! ```
//!
//! ## Complex Query with Multiple Predicates
//!
//! ```rust,no_run
//! use sqry_core::ast::{parse_query, ContextExtractor};
//! use std::path::Path;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Find methods inside classes, but not in test classes
//! let query = parse_query("kind:method AND parent:class AND NOT in:TestClass")?;
//!
//! let extractor = ContextExtractor::new();
//! let matches = extractor.extract_from_file(Path::new("src/lib.rs"))?;
//!
//! for ctx_match in matches.iter().filter(|m| query.matches(m)) {
//!     println!("{} at {}:{}",
//!              ctx_match.name,
//!              ctx_match.file_path.display(),
//!              ctx_match.start_line);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Depth-Based Filtering
//!
//! ```rust,no_run
//! use sqry_core::ast::{parse_query, ContextExtractor};
//! use std::path::Path;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Find deeply nested functions (more than 2 levels deep)
//! let query = parse_query("kind:function AND depth:>2")?;
//!
//! let extractor = ContextExtractor::new();
//! let matches = extractor.extract_from_file(Path::new("src/lib.rs"))?;
//!
//! for ctx_match in matches.iter().filter(|m| query.matches(m)) {
//!     println!("{} depth={} path={}",
//!              ctx_match.name,
//!              ctx_match.context.depth(),
//!              ctx_match.context.path());
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Directory Traversal
//!
//! ```rust,no_run
//! use sqry_core::ast::{parse_query, ContextExtractor};
//! use std::path::Path;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Find all public functions across entire codebase
//! let query = parse_query("kind:function")?;
//!
//! let extractor = ContextExtractor::new();
//! let matches = extractor.extract_from_directory(Path::new("src"))?;
//!
//! println!("Found {} functions across codebase",
//!          matches.iter().filter(|m| query.matches(m)).count());
//! # Ok(())
//! # }
//! ```
//!
//! # Query Language
//!
//! The query language supports 7 predicate types:
//!
//! - `kind:TYPE` - Match symbol kind (function, method, class, struct, enum, etc.)
//! - `name~=REGEX` - Match symbol name with regex
//! - `parent:TYPE` - Match parent kind
//! - `in:NAME` - Match if inside ancestor with given name
//! - `depth:N` - Match nesting depth (supports >, <, >=, <=, =)
//! - `path:TEXT` - Match if symbol path contains text
//! - `lang:LANG` - Match language (rust, javascript, typescript, python, go)
//!
//! Boolean operators: `AND`, `OR`, `NOT` (case-insensitive) with parentheses for grouping.
//!
//! ## Example Queries
//!
//! - `kind:function` - All functions
//! - `kind:method AND parent:class` - Methods in classes
//! - `name~=^test_` - Names starting with "test_"
//! - `depth:>3` - Deeply nested symbols
//! - `lang:rust AND kind:struct` - Rust structs only
//! - `(kind:function OR kind:method) AND NOT name~=test` - Functions/methods excluding tests
//! - `in:MyClass AND depth:>=2` - Symbols inside MyClass at depth 2+

pub mod context;
pub mod error;
pub mod incremental_parse;
pub mod query;
pub mod scope;
pub mod scope_nesting;
pub mod types;

pub use context::ContextExtractor;
pub use error::AstQueryError;
pub use incremental_parse::{IncrementalParser, InputEditCalculator, TreeCache};
pub use query::{AstExpr, AstPredicate, DepthOp, parse_query};
pub use scope::{Scope, ScopeId};
pub use scope_nesting::{get_ancestor_scopes, get_parent_scope, is_nested_in, link_nested_scopes};
pub use types::{Context, ContextItem, ContextKind, ContextualMatch};

/// Type alias for Results in AST query module
pub type Result<T> = std::result::Result<T, AstQueryError>;
