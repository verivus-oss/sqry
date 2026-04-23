//! Query language for AST-aware code search
//!
//! This module provides a powerful query language for searching code based on
//! semantic properties like symbol type, name patterns, language, and more.
//!
//! # Query Parsing
//!
//! All queries are parsed through the boolean AST parser:
//!
//! - **Boolean queries**: Parsed directly via [`QueryParser`](crate::query::QueryParser)
//! - **Execution**: Use [`QueryExecutor::execute_on_graph`](crate::query::QueryExecutor::execute_on_graph) for string queries
//!
//! ## Recommended Entry Points
//!
//! | Use Case | Function | Notes |
//! |----------|----------|-------|
//! | Query execution | [`QueryExecutor::execute_on_graph`](crate::query::QueryExecutor::execute_on_graph) | Most common - handles everything |
//! | AST access | [`QueryParser::parse_query`](crate::query::QueryParser::parse_query) | Boolean queries only |
//!
//! # Query Language
//!
//! The boolean query language supports:
//!
//! - **Boolean operators**: AND, OR, NOT
//! - **Comparison operators**: `:` (equal), `~=` (regex), `>`, `<`, `>=`, `<=`
//! - **Field types**: kind, name, path, lang, text, and plugin-specific fields
//! - **Values**: strings, regex patterns, numbers, booleans
//! - **Grouping**: parentheses for precedence
//!
//! # Core Types
//!
//! - [`Query`](crate::query::types::Query) - Complete query with root expression
//! - [`Expr`](crate::query::Expr) - Expression nodes (Or, And, Not, Condition)
//! - [`Condition`](crate::query::Condition) - Field-operator-value conditions
//! - [`Operator`](crate::query::Operator) - Comparison operators
//! - [`Value`](crate::query::Value) - Values in conditions
//!
//! # Error Handling
//!
//! All errors use [`QueryError`](crate::query::QueryError) which wraps:
//! - [`LexError`](crate::query::LexError) - Tokenization errors
//! - [`ParseError`](crate::query::ParseError) - Parsing errors
//! - [`ValidationError`](crate::query::ValidationError) - Semantic validation errors
//! - [`ExecutionError`](crate::query::ExecutionError) - Runtime execution errors
//!
//! # Performance
//!
//! The query lexer uses a thread-local buffer pool to reduce allocations:
//!
//! - **Allocation efficiency**: 80% fewer heap allocations vs creating fresh lexers
//! - **Memory efficiency**: Reuses token buffers across multiple tokenize calls
//! - **Thread-safe**: Each thread has its own isolated pool
//! - **Configurable**: Can be tuned or disabled via environment variables
//!
//! ## Lexer Pool Configuration
//!
//! The lexer pool can be configured via environment variables:
//!
//! ```bash
//! # Pool size (default: 4 lexers per thread)
//! export SQRY_LEXER_POOL_MAX=8
//!
//! # Buffer capacity limit (default: 256 tokens)
//! export SQRY_LEXER_POOL_MAX_CAP=512
//!
//! # Shrink ratio (default: 8x)
//! export SQRY_LEXER_POOL_SHRINK_RATIO=4
//!
//! # Disable pooling (for latency-critical workloads)
//! export SQRY_LEXER_POOL_MAX=0
//! ```
//!
//! **Performance characteristics**:
//! - Simple queries (<10 tokens): ~380ns (pooled) vs ~223ns (fresh) – pooling adds overhead
//! - Long queries (>100 tokens): ~18.2µs (pooled) vs ~19.1µs (fresh) – pooling saves ~4%
//! - High-throughput workloads: Pool reduces GC pressure and memory fragmentation
//!
//! **Recommendation**: Keep pooling enabled (default) for most workloads. The overhead
//! (~150ns per query) is negligible compared to typical query execution time (>10ms).
//! Disable only for latency-critical single-query scenarios.
//!
//! # Examples
//!
//! ```ignore
//! use sqry_core::query::QueryParser;
//!
//! // Find all async functions
//! let query = QueryParser::parse_query("kind:function AND async:true").unwrap();
//!
//! // Find test functions (using regex)
//! let query = QueryParser::parse_query("kind:function AND name~=/^test_/").unwrap();
//!
//! // Find functions OR methods in Rust files
//! let query = QueryParser::parse_query("(kind:function OR kind:method) AND lang:rust").unwrap();
//!
//! // Find non-test functions
//! let query = QueryParser::parse_query("kind:function AND NOT name~=/test/").unwrap();
//! ```

// Core query modules
pub mod cycles_config;
pub mod executor;
pub mod name_matching;
mod repo_filter;
pub mod unused_config;

// Boolean query language modules
pub mod builder;
pub mod cache;
pub mod error;
pub mod lexer;
pub mod optimizer;
pub mod parsed_query;
pub mod parser_new;
pub mod pipeline;
pub mod plan;
pub mod regex_cache;
pub mod registry;
pub mod results;
pub mod security;
pub mod types;
pub mod validator;

// Existing exports
pub use executor::QueryExecutor;

// CD Predicate exports (duplicate detection, cycle detection, unused analysis)
//
// Cycle detection (`CircularType` + `CircularConfig`) and unused-symbol
// detection (`UnusedScope`) have their *configuration types* in
// `cycles_config` / `unused_config`. The actual queries moved to
// `sqry-db` in Phase 3C DB15-DB19 (cached derived queries). The legacy
// `find_all_cycles_graph` / `is_node_in_cycle` / `find_unused_nodes` /
// `compute_reachable_set_graph` / `is_node_unused` functions were
// deleted in DB19 — all callers route through sqry-db.
pub use cycles_config::{CircularConfig, CircularType};
pub use unused_config::UnusedScope;

// Duplicate detection still lives in the executor (DB20 scope).
pub use executor::build_duplicate_groups_graph;
pub use executor::{DuplicateConfig, DuplicateGroup, DuplicateType};

pub use repo_filter::RepoFilter;

// New exports
pub use error::{
    ExecutionError, LexError, ParseError, QueryError, RichQueryError, ValidationError,
};
pub use lexer::{Lexer, Token, TokenType};
pub use optimizer::Optimizer;
pub use parsed_query::ParsedQuery;
pub use parser_new::Parser as QueryParser;
pub use plan::{CacheStatus, ExecutionStep, QueryPlan};
pub use registry::{FieldRegistry, core_fields};
pub use types::{
    Condition, Expr, Field, FieldDescriptor, FieldType, JoinEdgeKind, JoinExpr, Operator,
    PipelineQuery, PipelineStage, Query as QueryAST, RegexFlags, RegexValue, Span, Value,
};
pub use validator::{ContradictionWarning, Validator};

// Pipeline/aggregation results
pub use pipeline::{AggregationResult, CountResult, GroupByResult, StatsResult, TopResult};

// Pipeline execution
pub use executor::execute_pipeline_stage;

// Builder API exports (P2-10)
pub use builder::{BuildError, ConditionBuilder, QueryBuilder, RegexBuilder};

// CodeGraph-native query results (v6)
pub use results::{JoinMatch, JoinResults, QueryMatch, QueryOutput, QueryResults};
