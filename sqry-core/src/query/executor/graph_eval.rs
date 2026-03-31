//! Graph-native predicate evaluation.
//!
//! This module provides graph-native predicate evaluation for queries against
//! `CodeGraph`, bypassing the legacy index path.
//!
//! # Design (v6 - CodeGraph-Native Query Executor)
//!
//! Key semantics preserved from the legacy index path:
//! - `name:` uses `segments_match` for qualified name suffix matching
//! - `kind:`, `lang:`, `visibility:`, `scope.*` are **case-sensitive**
//! - `imports:` uses SUBSTRING (contains) matching at file level
//! - `references:` includes `References` + `Calls` + `Imports` + `FfiCall` edges
//! - `callers:` checks OUTGOING edges (find nodes that call X)
//! - `callees:` checks INCOMING edges (find nodes called by X)
//! - Relation predicates use `segments_match` for qualified names
//!
//! # Limitations (v1)
//!
//! The following predicates are **NOT SUPPORTED** in graph backend v1:
//! - `returns:` - requires signature parsing/metadata not in `NodeEntry`
//! - Plugin fields - requires metadata `HashMap` not in `NodeEntry`
//! - Numeric operators - requires metadata values
//!
//! **Supported boolean predicates**: `async:true`, `static:true`

use crate::graph::unified::FileId;
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::edge::{EdgeKind, StoreEdgeRef};
use crate::graph::unified::node::{NodeId, NodeKind};
use crate::graph::unified::resolution::{
    canonicalize_graph_qualified_name, display_graph_qualified_name,
};
use crate::graph::unified::storage::arena::NodeEntry;
use crate::plugin::PluginManager;
use crate::query::name_matching::segments_match;
use crate::query::regex_cache::get_or_compile_regex;
use crate::query::types::{Condition, Expr, JoinEdgeKind, JoinExpr, Operator, Value};
use anyhow::{Result, anyhow};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

/// Cache of precomputed subquery result sets, keyed by `(span.start, span.end)`.
///
/// Subquery expressions from the AST are evaluated once against all graph nodes
/// and their results cached here. During per-node evaluation, matchers look up
/// the cache instead of re-evaluating the full graph. This reduces subquery cost
/// from O(N^2) to O(N) where N is the number of graph nodes.
type SubqueryCache = HashMap<(usize, usize), Arc<HashSet<NodeId>>>;

/// Context for graph-native predicate evaluation.
///
/// Encapsulates all dependencies needed for evaluating predicates against
/// a `CodeGraph`. The context includes precomputed caches for performance.
pub struct GraphEvalContext<'a> {
    /// Reference to the code graph
    pub graph: &'a CodeGraph,
    /// Plugin manager for detecting plugin fields
    pub plugin_manager: &'a PluginManager,
    /// Workspace root for relative path resolution in `path:` predicates
    pub workspace_root: Option<&'a Path>,
    /// If true, disable parallel execution
    pub disable_parallel: bool,
    /// Precomputed cache: module name -> set of `FileIds` that import it
    /// Used by `match_imports` to avoid O(N^2) per-node scan
    pub importing_files: HashMap<String, HashSet<FileId>>,
    /// Precomputed subquery result sets, keyed by `(span.start, span.end)`.
    /// Populated by `precompute_subqueries()` before the per-node evaluation loop.
    pub subquery_cache: SubqueryCache,
}

impl<'a> GraphEvalContext<'a> {
    /// Creates a new evaluation context.
    #[must_use]
    pub fn new(graph: &'a CodeGraph, plugin_manager: &'a PluginManager) -> Self {
        Self {
            graph,
            plugin_manager,
            workspace_root: None,
            disable_parallel: false,
            importing_files: HashMap::new(),
            subquery_cache: HashMap::new(),
        }
    }

    /// Sets the workspace root for relative path resolution.
    #[must_use]
    pub fn with_workspace_root(mut self, root: &'a Path) -> Self {
        self.workspace_root = Some(root);
        self
    }

    /// Disables parallel execution.
    #[must_use]
    pub fn with_parallel_disabled(mut self, disabled: bool) -> Self {
        self.disable_parallel = disabled;
        self
    }

    /// Precomputes importing files for a given module (call before `evaluate_all`).
    ///
    /// This avoids O(N^2) performance by caching which files import a given module.
    pub fn precompute_imports(&mut self, target_module: &str) {
        if !self.importing_files.contains_key(target_module) {
            let files = precompute_importing_files(self.graph, target_module);
            self.importing_files
                .insert(target_module.to_string(), files);
        }
    }

    /// Precomputes all subquery result sets from the expression tree.
    ///
    /// This must be called before `evaluate_all` to avoid O(N^2) behavior
    /// where each subquery is re-evaluated for every candidate node.
    ///
    /// # Errors
    ///
    /// Returns an error if subquery evaluation fails.
    pub fn precompute_subqueries(&mut self, expr: &Expr) -> Result<()> {
        let mut subquery_exprs = Vec::new();
        collect_subquery_exprs(expr, &mut subquery_exprs);

        for (span_key, inner_expr) in subquery_exprs {
            if !self.subquery_cache.contains_key(&span_key) {
                let result_set = evaluate_subquery(self, inner_expr)?;
                self.subquery_cache.insert(span_key, Arc::new(result_set));
            }
        }
        Ok(())
    }
}

/// Collects all `Value::Subquery(inner)` expressions from the AST for precomputation.
///
/// Returns `(span_key, &Expr)` pairs where `span_key` is `(span.start, span.end)`.
///
/// Uses **post-order** traversal: nested (inner) subqueries appear before their
/// enclosing (outer) subqueries. This ensures `precompute_subqueries()` evaluates
/// dependencies first, so outer subquery evaluation can find inner results in the cache.
fn collect_subquery_exprs<'a>(expr: &'a Expr, out: &mut Vec<((usize, usize), &'a Expr)>) {
    match expr {
        Expr::Condition(cond) => {
            if let Value::Subquery(inner) = &cond.value {
                // Post-order: recurse into nested subqueries FIRST
                collect_subquery_exprs(inner, out);
                // Then record this (outer) subquery
                out.push(((cond.span.start, cond.span.end), inner));
            }
        }
        Expr::And(operands) | Expr::Or(operands) => {
            for op in operands {
                collect_subquery_exprs(op, out);
            }
        }
        Expr::Not(inner) => collect_subquery_exprs(inner, out),
        Expr::Join(join) => {
            collect_subquery_exprs(&join.left, out);
            collect_subquery_exprs(&join.right, out);
        }
    }
}

/// Collects all `imports:` predicate values from the query AST (deduplicated).
///
/// This is used to precompute the importing files cache before evaluation.
#[must_use]
pub fn collect_import_targets(expr: &Expr) -> HashSet<String> {
    let mut targets = HashSet::new();
    collect_import_targets_inner(expr, &mut targets);
    targets
}

fn collect_import_targets_inner(expr: &Expr, targets: &mut HashSet<String>) {
    match expr {
        Expr::Condition(cond) => {
            if cond.field.as_str() == "imports"
                && let Some(value) = cond.value.as_string()
            {
                targets.insert(value.to_string());
            }
        }
        Expr::And(operands) | Expr::Or(operands) => {
            for operand in operands {
                collect_import_targets_inner(operand, targets);
            }
        }
        Expr::Not(inner) => collect_import_targets_inner(inner, targets),
        Expr::Join(join) => {
            collect_import_targets_inner(&join.left, targets);
            collect_import_targets_inner(&join.right, targets);
        }
    }
}

/// Evaluates query against all nodes, returning matching `NodeIds`.
///
/// # Errors
///
/// Returns an error if predicate evaluation fails (e.g., unsupported predicates).
pub fn evaluate_all(ctx: &mut GraphEvalContext, expr: &Expr) -> Result<Vec<NodeId>> {
    // Precompute all subquery result sets before the per-node evaluation loop.
    // This is critical for performance: without precomputation, each subquery
    // matcher would re-evaluate the full graph for every candidate node (O(N^2)).
    ctx.precompute_subqueries(expr)?;

    let arena = ctx.graph.nodes();

    // Create recursion guard
    let recursion_limits = crate::config::RecursionLimits::load_or_default()?;
    let expr_depth = recursion_limits.effective_expr_depth()?;
    let mut guard = crate::query::security::RecursionGuard::new(expr_depth)?;

    if ctx.disable_parallel {
        // Sequential evaluation
        let mut matches = Vec::new();
        for (id, _) in arena.iter() {
            if evaluate_node(ctx, id, expr, &mut guard)? {
                matches.push(id);
            }
        }
        Ok(matches)
    } else {
        // Parallel evaluation - each thread needs its own guard
        use rayon::prelude::*;

        let node_ids: Vec<_> = arena.iter().map(|(id, _)| id).collect();
        let results: Vec<Result<Option<NodeId>>> = node_ids
            .into_par_iter()
            .map(|id| {
                let mut thread_guard = crate::query::security::RecursionGuard::new(expr_depth)?;
                evaluate_node(ctx, id, expr, &mut thread_guard)
                    .map(|m| if m { Some(id) } else { None })
            })
            .collect();

        // Check for any errors and collect matches
        let mut matches = Vec::new();
        for result in results {
            if let Some(id) = result? {
                matches.push(id);
            }
        }
        Ok(matches)
    }
}

/// Evaluates a single node against an expression.
///
/// # Errors
///
/// Returns an error for unsupported predicates or if recursion depth exceeds the guard's limit.
pub fn evaluate_node(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    expr: &Expr,
    guard: &mut crate::query::security::RecursionGuard,
) -> Result<bool> {
    guard.enter()?;

    let result = match expr {
        Expr::Condition(cond) => evaluate_condition(ctx, node_id, cond),
        Expr::And(operands) => {
            for operand in operands {
                if !evaluate_node(ctx, node_id, operand, guard)? {
                    guard.exit();
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Expr::Or(operands) => {
            for operand in operands {
                if evaluate_node(ctx, node_id, operand, guard)? {
                    guard.exit();
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Expr::Not(inner) => Ok(!evaluate_node(ctx, node_id, inner, guard)?),
        Expr::Join(_) => {
            // Join expressions are evaluated at a higher level (execute_join),
            // not per-node. If we reach here, it's a programming error.
            Err(anyhow::anyhow!(
                "Join expressions cannot be evaluated per-node; use execute_join instead"
            ))
        }
    };

    guard.exit();
    result
}

fn evaluate_condition(ctx: &GraphEvalContext, node_id: NodeId, cond: &Condition) -> Result<bool> {
    let Some(entry) = ctx.graph.nodes().get(node_id) else {
        return Ok(false);
    };

    match cond.field.as_str() {
        "kind" => Ok(match_kind(ctx, entry, &cond.operator, &cond.value)),
        "name" => Ok(match_name(ctx, entry, &cond.operator, &cond.value)),
        "path" => Ok(match_path(ctx, entry, &cond.operator, &cond.value)),
        "lang" | "language" => Ok(match_lang(ctx, entry, &cond.operator, &cond.value)),
        "visibility" => Ok(match_visibility(ctx, entry, &cond.operator, &cond.value)),
        "async" => Ok(match_async(entry, &cond.operator, &cond.value)),
        "static" => Ok(match_static(entry, &cond.operator, &cond.value)),
        "callers" => {
            if matches!(cond.value, Value::Subquery(_)) {
                let key = (cond.span.start, cond.span.end);
                let cached = ctx.subquery_cache.get(&key).cloned();
                match_callers_subquery(ctx, node_id, cached.as_deref())
            } else {
                Ok(match_callers(ctx, node_id, &cond.value))
            }
        }
        "callees" => {
            if matches!(cond.value, Value::Subquery(_)) {
                let key = (cond.span.start, cond.span.end);
                let cached = ctx.subquery_cache.get(&key).cloned();
                match_callees_subquery(ctx, node_id, cached.as_deref())
            } else {
                Ok(match_callees(ctx, node_id, &cond.value))
            }
        }
        "imports" => {
            if matches!(cond.value, Value::Subquery(_)) {
                let key = (cond.span.start, cond.span.end);
                let cached = ctx.subquery_cache.get(&key).cloned();
                match_imports_subquery(ctx, node_id, cached.as_deref())
            } else {
                Ok(match_imports(ctx, node_id, &cond.value))
            }
        }
        "exports" => {
            if matches!(cond.value, Value::Subquery(_)) {
                let key = (cond.span.start, cond.span.end);
                let cached = ctx.subquery_cache.get(&key).cloned();
                match_exports_subquery(ctx, node_id, cached.as_deref())
            } else {
                Ok(match_exports(ctx, node_id, &cond.value))
            }
        }
        "references" => {
            if matches!(cond.value, Value::Subquery(_)) {
                let key = (cond.span.start, cond.span.end);
                let cached = ctx.subquery_cache.get(&key).cloned();
                match_references_subquery(ctx, node_id, cached.as_deref())
            } else {
                Ok(match_references(ctx, node_id, &cond.operator, &cond.value))
            }
        }
        "impl" | "implements" => {
            if matches!(cond.value, Value::Subquery(_)) {
                let key = (cond.span.start, cond.span.end);
                let cached = ctx.subquery_cache.get(&key).cloned();
                match_implements_subquery(ctx, node_id, cached.as_deref())
            } else {
                Ok(match_implements(ctx, node_id, &cond.value))
            }
        }
        field if field.starts_with("scope.") => Ok(match_scope(
            ctx,
            node_id,
            field,
            &cond.operator,
            &cond.value,
        )),
        "returns" => Ok(match_returns(ctx, entry, &cond.operator, &cond.value)),
        field if is_plugin_field(ctx, field) => Err(anyhow!(
            "Plugin field '{field}' requires metadata not available in graph backend"
        )),
        _ => Ok(false), // Unknown field
    }
}

/// Checks if a field is a plugin-specific field.
fn is_plugin_field(ctx: &GraphEvalContext, field: &str) -> bool {
    // Check plugin registry for field descriptors
    let is_registered_field = ctx
        .plugin_manager
        .plugins()
        .iter()
        .flat_map(|plugin| plugin.fields().iter())
        .any(|descriptor| descriptor.name == field);

    if is_registered_field {
        return true;
    }

    // Fallback static list (for when registry not fully wired)
    // Note: "async" and "static" are now handled natively via `NodeEntry` flags
    matches!(
        field,
        "abstract" | "final" | "generic" | "parameters" | "arity"
    )
}

// ============================================================================
// Kind predicate (with regex + synonyms)
// ============================================================================

/// Maps synonyms to canonical kind names (case-sensitive).
///
/// Per v6 spec, kind: comparisons are case-sensitive.
/// Only exact matches for known synonyms are normalized.
fn normalize_kind(kind: &str) -> &str {
    match kind {
        // Rust/Go synonyms (case-sensitive)
        "trait" => "interface", // Rust trait = interface
        "impl" => "implementation",
        // Property synonyms
        "field" => "property",
        // Module synonyms
        "namespace" => "module",
        // Component synonyms
        "element" => "component",
        // CSS/Style synonyms
        "style" => "style_rule",
        "at_rule" => "style_at_rule",
        "css_var" | "custom_property" => "style_variable",
        // No lowercasing - return as-is for case-sensitive comparison
        _ => kind,
    }
}

fn match_kind(
    _ctx: &GraphEvalContext,
    entry: &NodeEntry,
    operator: &Operator,
    value: &Value,
) -> bool {
    let actual = entry.kind.as_str();

    match (operator, value) {
        (Operator::Equal, Value::String(expected)) => {
            let normalized_expected = normalize_kind(expected);
            let normalized_actual = normalize_kind(actual);
            normalized_actual == normalized_expected
        }
        (Operator::Regex, Value::Regex(regex_val)) => get_or_compile_regex(
            &regex_val.pattern,
            regex_val.flags.case_insensitive,
            regex_val.flags.multiline,
            regex_val.flags.dot_all,
        )
        .map(|re| re.is_match(actual))
        .unwrap_or(false),
        _ => false,
    }
}

// ============================================================================
// Name predicate (EXACT match for equality, regex supported)
// ============================================================================

fn match_name(
    ctx: &GraphEvalContext,
    entry: &NodeEntry,
    operator: &Operator,
    value: &Value,
) -> bool {
    match (operator, value) {
        // Use segments_match for qualified name matching:
        // - "connect" matches "database::connect" (suffix match)
        // - "foo::bar" matches "baz::foo::bar" (suffix match)
        // - "database::connect" matches "database::connect" (exact match)
        (Operator::Equal, Value::String(expected)) => {
            entry_query_texts(ctx.graph, entry).iter().any(|candidate| {
                language_aware_segments_match(ctx.graph, entry.file, candidate, expected)
            })
        }
        (Operator::Regex, Value::Regex(regex_val)) => get_or_compile_regex(
            &regex_val.pattern,
            regex_val.flags.case_insensitive,
            regex_val.flags.multiline,
            regex_val.flags.dot_all,
        )
        .map(|re| {
            entry_query_texts(ctx.graph, entry)
                .iter()
                .any(|candidate| re.is_match(candidate))
        })
        .unwrap_or(false),
        _ => false,
    }
}

// ============================================================================
// Path predicate (glob + regex)
// ============================================================================

/// Check if a pattern is relative (doesn't start with `/`).
fn is_relative_pattern(pattern: &str) -> bool {
    !pattern.starts_with('/')
}

fn match_path(
    ctx: &GraphEvalContext,
    entry: &NodeEntry,
    operator: &Operator,
    value: &Value,
) -> bool {
    let Some(file_path) = ctx.graph.files().resolve(entry.file) else {
        return false;
    };

    match (operator, value) {
        (Operator::Equal, Value::String(pattern)) => {
            // Only strip workspace_root for RELATIVE patterns (parity with legacy index)
            let match_path = if is_relative_pattern(pattern) {
                if let Some(root) = ctx.workspace_root {
                    file_path
                        .strip_prefix(root)
                        .map_or_else(|_| file_path.to_path_buf(), std::path::Path::to_path_buf)
                } else {
                    file_path.to_path_buf()
                }
            } else {
                // Absolute pattern: match against full path
                file_path.to_path_buf()
            };
            globset::Glob::new(pattern)
                .map(|g| g.compile_matcher().is_match(&match_path))
                .unwrap_or(false)
        }
        (Operator::Regex, Value::Regex(regex_val)) => {
            // For regex, always use full path (matches legacy index behavior)
            get_or_compile_regex(
                &regex_val.pattern,
                regex_val.flags.case_insensitive,
                regex_val.flags.multiline,
                regex_val.flags.dot_all,
            )
            .map(|re| re.is_match(file_path.to_string_lossy().as_ref()))
            .unwrap_or(false)
        }
        _ => false,
    }
}

// ============================================================================
// Language predicate (from FILE REGISTRY, not NodeEntry)
// ============================================================================

/// Convert Language enum to canonical string for parity with legacy index.
///
/// Legacy index uses canonical names like "javascript", "typescript", etc.
/// `Language::Display` uses short forms like "js", "ts".
/// This function provides the canonical mapping for query parity.
fn language_to_canonical(lang: crate::graph::node::Language) -> &'static str {
    use crate::graph::node::Language;
    match lang {
        Language::C => "c",
        Language::Cpp => "cpp",
        Language::CSharp => "csharp",
        Language::Css => "css",
        Language::JavaScript => "javascript",
        Language::Python => "python",
        Language::TypeScript => "typescript",
        Language::Rust => "rust",
        Language::Go => "go",
        Language::Java => "java",
        Language::Ruby => "ruby",
        Language::Php => "php",
        Language::Swift => "swift",
        Language::Kotlin => "kotlin",
        Language::Scala => "scala",
        Language::Sql => "sql",
        Language::Dart => "dart",
        Language::Lua => "lua",
        Language::Perl => "perl",
        Language::Shell => "shell",
        Language::Groovy => "groovy",
        Language::Elixir => "elixir",
        Language::R => "r",
        Language::Haskell => "haskell",
        Language::Html => "html",
        Language::Svelte => "svelte",
        Language::Vue => "vue",
        Language::Zig => "zig",
        Language::Terraform => "terraform",
        Language::Puppet => "puppet",
        Language::Pulumi => "pulumi",
        Language::Http => "http",
        Language::Plsql => "plsql",
        Language::Apex => "apex",
        Language::Abap => "abap",
        Language::ServiceNow => "servicenow",
        Language::Json => "json",
    }
}

fn match_lang(
    ctx: &GraphEvalContext,
    entry: &NodeEntry,
    operator: &Operator,
    value: &Value,
) -> bool {
    // Get language from file registry - NodeEntry has no language field
    let Some(lang) = ctx.graph.files().language_for_file(entry.file) else {
        return false;
    };

    // Use canonical language names for parity with legacy index
    let actual = language_to_canonical(lang);

    // Support both equality and regex operators
    match (operator, value) {
        (Operator::Equal, Value::String(expected)) => actual == expected,
        (Operator::Regex, Value::Regex(rv)) => get_or_compile_regex(
            &rv.pattern,
            rv.flags.case_insensitive,
            rv.flags.multiline,
            rv.flags.dot_all,
        )
        .map(|re| re.is_match(actual))
        .unwrap_or(false),
        _ => false,
    }
}

// ============================================================================
// Visibility predicate
// ============================================================================

fn match_visibility(
    ctx: &GraphEvalContext,
    entry: &NodeEntry,
    operator: &Operator,
    value: &Value,
) -> bool {
    let Some(expected) = value.as_string() else {
        return false;
    };

    let normalized_expected = if expected == "pub" {
        "public"
    } else {
        expected
    };

    let Some(vis_id) = entry.visibility else {
        // No visibility = private by default
        return match operator {
            Operator::Equal => normalized_expected == "private",
            _ => false,
        };
    };

    let Some(actual) = ctx.graph.strings().resolve(vis_id) else {
        return false;
    };
    let normalized_actual = if actual.as_ref().starts_with("pub") {
        "public"
    } else {
        actual.as_ref()
    };

    // Case-sensitive comparison
    match operator {
        Operator::Equal => normalized_actual == normalized_expected,
        _ => false,
    }
}

// ============================================================================
// Returns predicate
// ============================================================================

/// Match `returns:TypeName` predicate.
///
/// Checks the `signature` field on `NodeEntry` for return type matching.
/// Uses substring matching to support both:
/// - Exact: `returns:Optional<User>` matches signature containing "Optional<User>"
/// - Partial: `returns:Optional` matches any signature containing "Optional"
fn match_returns(
    ctx: &GraphEvalContext,
    entry: &NodeEntry,
    operator: &Operator,
    value: &Value,
) -> bool {
    let Some(expected) = value.as_string() else {
        return false;
    };

    // Only functions and methods have return types
    if !matches!(entry.kind, NodeKind::Function | NodeKind::Method) {
        return false;
    }

    let Some(sig_id) = entry.signature else {
        // No signature means no return type info
        return false;
    };

    let Some(signature) = ctx.graph.strings().resolve(sig_id) else {
        return false;
    };

    // The signature typically contains the return type.
    // Use contains for substring matching to support partial matches.
    match operator {
        Operator::Equal => signature.contains(expected),
        _ => false,
    }
}

// ============================================================================
// Boolean predicates (async, static)
// ============================================================================

/// Match `async:true` or `async:false` predicate.
///
/// Checks the `is_async` flag on `NodeEntry`.
/// Handles both Boolean values (from parser) and String values ("true"/"false").
fn match_async(entry: &NodeEntry, operator: &Operator, value: &Value) -> bool {
    let expected = value_to_bool(value);
    let Some(expected) = expected else {
        return false;
    };

    match operator {
        Operator::Equal => entry.is_async == expected,
        _ => false,
    }
}

/// Match `static:true` or `static:false` predicate.
///
/// Checks the `is_static` flag on `NodeEntry`.
/// Handles both Boolean values (from parser) and String values ("true"/"false").
fn match_static(entry: &NodeEntry, operator: &Operator, value: &Value) -> bool {
    let expected = value_to_bool(value);
    let Some(expected) = expected else {
        return false;
    };

    match operator {
        Operator::Equal => entry.is_static == expected,
        _ => false,
    }
}

/// Convert a Value to a boolean.
///
/// Handles:
/// - `Value::Boolean(b)` → `Some(b)`
/// - `Value::String("true"|"yes"|"1")` → `Some(true)`
/// - `Value::String("false"|"no"|"0")` → `Some(false)`
/// - Other values → `None`
fn value_to_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Boolean(b) => Some(*b),
        Value::String(s) => match s.to_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

// ============================================================================
// Relation predicates (CORRECTED DIRECTION)
// ============================================================================

/// `callers:X` - find symbols that CALL X
///
/// When evaluating node Y: does Y call X? → Check Y's OUTGOING edges
///
/// For dynamically-typed languages (Lua, Python, Ruby, etc.), method calls like
/// `target:method()` create edges to `receiver::method` where `receiver` is the
/// variable name, not the class name. To handle this, when querying for a qualified
/// name like `Class::method`, we also match call edges to `X::method` where X is
/// any receiver, as long as a symbol `Class::method` exists in the graph.
fn match_callers(ctx: &GraphEvalContext, node_id: NodeId, value: &Value) -> bool {
    let Some(target_name) = value.as_string() else {
        return false;
    };

    // Extract the method name part for fallback matching in dynamic languages
    // e.g., "Player::takeDamage" -> "takeDamage"
    let method_part = extract_method_name(target_name);

    // Check OUTGOING Calls edges from this node
    for edge in ctx.graph.edges().edges_from(node_id) {
        if let EdgeKind::Calls { .. } = &edge.kind
            && let Some(target_entry) = ctx.graph.nodes().get(edge.target)
        {
            let callee_names = entry_query_texts(ctx.graph, target_entry);

            if callee_names.iter().any(|callee_name| {
                language_aware_segments_match(
                    ctx.graph,
                    target_entry.file,
                    callee_name,
                    target_name,
                )
            }) {
                return true;
            }

            // For dynamic languages: if target_name is qualified (e.g., "Player::takeDamage")
            // and callee is also qualified (e.g., "target::takeDamage"), match on method part
            // This handles cases where the receiver type isn't known at call time
            if let Some(method) = &method_part
                && callee_names
                    .iter()
                    .filter_map(|callee_name| extract_method_name(callee_name))
                    .any(|callee_method| method == &callee_method)
            {
                return true;
            }
        }
    }
    false
}

/// Extract the method name from a qualified name.
/// e.g., "`Player::takeDamage`" -> Some("takeDamage")
/// e.g., "takeDamage" -> None (no separator, not qualified)
fn extract_method_name(qualified: &str) -> Option<String> {
    // Look for separator from the end
    for sep in ["::", ".", "#", ":", "/"] {
        if let Some(pos) = qualified.rfind(sep) {
            let method = &qualified[pos + sep.len()..];
            if !method.is_empty() {
                return Some(method.to_string());
            }
        }
    }
    None
}

/// `callees:X` - find symbols that ARE CALLED BY X
///
/// When evaluating node Y: does X call Y? → Check Y's INCOMING edges for source=X
fn match_callees(ctx: &GraphEvalContext, node_id: NodeId, value: &Value) -> bool {
    let Some(caller_name) = value.as_string() else {
        return false;
    };

    // Check INCOMING Calls edges to this node
    for edge in ctx.graph.edges().edges_to(node_id) {
        if let EdgeKind::Calls { .. } = &edge.kind
            && let Some(source_entry) = ctx.graph.nodes().get(edge.source)
            && entry_query_texts(ctx.graph, source_entry)
                .iter()
                .any(|source_name| {
                    language_aware_segments_match(
                        ctx.graph,
                        source_entry.file,
                        source_name,
                        caller_name,
                    )
                })
        {
            return true;
        }
    }
    false
}

/// `imports:X` - find symbols in files that import X
///
/// Current behavior: all symbols in a file match if that file imports X.
/// Uses SUBSTRING matching (contains) to match legacy import semantics across:
/// - Import node names (module paths)
/// - Imported symbol node names (edge targets)
/// - `EdgeKind::Imports` alias metadata and wildcard flag
///
/// OPTIMIZATION: Precompute `importing_files` set once per query, then check membership.
fn match_imports(ctx: &GraphEvalContext, node_id: NodeId, value: &Value) -> bool {
    let Some(target_module) = value.as_string() else {
        return false;
    };

    // Get this node's file ID
    let Some(entry) = ctx.graph.nodes().get(node_id) else {
        return false;
    };
    let file_id = entry.file;

    // Check if this file imports the target module
    // Uses precomputed importing_files if available, otherwise computes inline
    if let Some(importing_files) = ctx.importing_files.get(target_module) {
        return importing_files.contains(&file_id);
    }

    // Fallback: compute inline (should not happen with proper initialization)
    // Scan Import nodes and Imports edges in this file using SUBSTRING (contains) matching
    for (other_id, other_entry) in ctx.graph.nodes().iter() {
        if other_entry.file != file_id {
            continue;
        }

        // Import node name (module path)
        if other_entry.kind == NodeKind::Import
            && import_entry_matches(ctx.graph, other_entry, target_module)
        {
            return true;
        }

        // Imports edges metadata + imported symbol node names
        for edge in ctx.graph.edges().edges_from(other_id) {
            if import_edge_matches(ctx.graph, &edge, target_module) {
                return true;
            }
        }
    }
    false
}

fn import_edge_matches(graph: &CodeGraph, edge: &StoreEdgeRef, target_module: &str) -> bool {
    let EdgeKind::Imports { alias, is_wildcard } = &edge.kind else {
        return false;
    };

    // Check target node text across simple, canonical, and native-display forms.
    let target_match = graph
        .nodes()
        .get(edge.target)
        .is_some_and(|entry| import_entry_matches(graph, entry, target_module));

    // Check alias
    let alias_match = alias
        .and_then(|sid| graph.strings().resolve(sid))
        .is_some_and(|alias_str| {
            graph.nodes().get(edge.source).is_some_and(|entry| {
                import_text_matches(graph, entry.file, alias_str.as_ref(), target_module)
            })
        });

    // Check wildcard
    let wildcard_match = *is_wildcard && target_module == "*";

    target_match || alias_match || wildcard_match
}

/// Precomputes which files import a given module (call once per unique `imports:` value).
fn precompute_importing_files(graph: &CodeGraph, target_module: &str) -> HashSet<FileId> {
    let mut importing_files = HashSet::new();

    for (node_id, entry) in graph.nodes().iter() {
        if entry.kind == NodeKind::Import && import_entry_matches(graph, entry, target_module) {
            importing_files.insert(entry.file);
        }

        for edge in graph.edges().edges_from(node_id) {
            if import_edge_matches(graph, &edge, target_module) {
                importing_files.insert(entry.file);
            }
        }
    }

    importing_files
}

fn import_text_matches(
    graph: &CodeGraph,
    file_id: FileId,
    candidate: &str,
    target_module: &str,
) -> bool {
    if candidate.contains(target_module) {
        return true;
    }

    graph
        .files()
        .language_for_file(file_id)
        .is_some_and(|language| {
            let canonical_target = canonicalize_graph_qualified_name(language, target_module);
            canonical_target != target_module && candidate.contains(&canonical_target)
        })
}

fn import_entry_matches(graph: &CodeGraph, entry: &NodeEntry, target_module: &str) -> bool {
    entry_query_texts(graph, entry)
        .iter()
        .any(|candidate| import_text_matches(graph, entry.file, candidate, target_module))
}

fn language_aware_segments_match(
    graph: &CodeGraph,
    file_id: FileId,
    candidate: &str,
    expected: &str,
) -> bool {
    if segments_match(candidate, expected) {
        return true;
    }

    graph
        .files()
        .language_for_file(file_id)
        .is_some_and(|language| {
            let canonical_expected = canonicalize_graph_qualified_name(language, expected);
            canonical_expected != expected && segments_match(candidate, &canonical_expected)
        })
}

fn push_unique_query_text(texts: &mut Vec<String>, candidate: impl Into<String>) {
    let candidate = candidate.into();
    if !texts.iter().any(|existing| existing == &candidate) {
        texts.push(candidate);
    }
}

fn entry_query_texts(graph: &CodeGraph, entry: &NodeEntry) -> Vec<String> {
    let mut texts = Vec::with_capacity(3);

    if let Some(name) = graph.strings().resolve(entry.name) {
        push_unique_query_text(&mut texts, name.to_string());
    }

    if let Some(qualified) = entry
        .qualified_name
        .and_then(|qualified_name_id| graph.strings().resolve(qualified_name_id))
    {
        push_unique_query_text(&mut texts, qualified.to_string());

        if let Some(language) = graph.files().language_for_file(entry.file) {
            push_unique_query_text(
                &mut texts,
                display_graph_qualified_name(
                    language,
                    qualified.as_ref(),
                    entry.kind,
                    entry.is_static,
                ),
            );
        }
    }

    texts
}

/// `exports:X` - find symbols that export X
///
/// File scope filtering: Only considers export edges where both source and
/// target are in the same file as the node being evaluated. This prevents
/// re-export edges from causing symbols in other files to match.
fn match_exports(ctx: &GraphEvalContext, node_id: NodeId, value: &Value) -> bool {
    let Some(target_name) = value.as_string() else {
        return false;
    };

    let Some(entry) = ctx.graph.nodes().get(node_id) else {
        return false;
    };
    let node_file = entry.file;

    if !entry_query_texts(ctx.graph, entry).iter().any(|candidate| {
        language_aware_segments_match(ctx.graph, entry.file, candidate, target_name)
    }) {
        return false;
    }

    let edges = ctx.graph.edges();

    // Check OUTGOING export edges: this node exports something
    for edge in edges.edges_from(node_id) {
        if let EdgeKind::Exports { .. } = &edge.kind {
            // File scope filtering: export edge must be in same file
            if let Some(target_entry) = ctx.graph.nodes().get(edge.target)
                && target_entry.file == node_file
            {
                return true;
            }
        }
    }

    // Check INCOMING export edges: something exports this node
    for edge in edges.edges_to(node_id) {
        if let EdgeKind::Exports { .. } = &edge.kind {
            // File scope filtering: export edge source must be in same file
            if let Some(source_entry) = ctx.graph.nodes().get(edge.source)
                && source_entry.file == node_file
            {
                return true;
            }
        }
    }

    false
}

/// `references:X` - find symbols named X that have references TO them.
///
/// Current behavior: references include `Calls`, `Imports`, `FfiCall`, and `References` edges.
fn match_references(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    operator: &Operator,
    value: &Value,
) -> bool {
    // First check if the symbol name matches the target
    let Some(entry) = ctx.graph.nodes().get(node_id) else {
        return false;
    };

    let name_matches = match (operator, value) {
        (Operator::Equal, Value::String(target)) => entry_query_texts(ctx.graph, entry)
            .iter()
            .any(|candidate| candidate == target || candidate.ends_with(&format!("::{target}"))),
        (Operator::Regex, Value::Regex(rv)) => get_or_compile_regex(
            &rv.pattern,
            rv.flags.case_insensitive,
            rv.flags.multiline,
            rv.flags.dot_all,
        )
        .map(|re| {
            entry_query_texts(ctx.graph, entry)
                .iter()
                .any(|candidate| re.is_match(candidate))
        })
        .unwrap_or(false),
        _ => false,
    };

    if !name_matches {
        return false;
    }

    // Check if the symbol has references TO it (incoming edges)
    // Current behavior: References, Calls, Imports, AND FfiCall all count as references
    for edge in ctx.graph.edges().edges_to(node_id) {
        let is_reference = matches!(
            &edge.kind,
            EdgeKind::References
                | EdgeKind::Calls { .. }
                | EdgeKind::Imports { .. }
                | EdgeKind::FfiCall { .. }
        );
        if is_reference {
            return true;
        }
    }

    false
}

/// `implements:X` - find symbols that implement interface/trait X
fn match_implements(ctx: &GraphEvalContext, node_id: NodeId, value: &Value) -> bool {
    let Some(trait_name) = value.as_string() else {
        return false;
    };

    for edge in ctx.graph.edges().edges_from(node_id) {
        if let EdgeKind::Implements = &edge.kind
            && let Some(target_entry) = ctx.graph.nodes().get(edge.target)
            && entry_query_texts(ctx.graph, target_entry)
                .iter()
                .any(|name| {
                    language_aware_segments_match(ctx.graph, target_entry.file, name, trait_name)
                })
        {
            return true;
        }
    }
    false
}

// ============================================================================
// Scope predicates (with regex support)
// ============================================================================

/// Convert `NodeKind` to scope type string for predicate matching.
///
/// This mapping provides parity with legacy index scope predicates:
/// - Trait → interface
/// - Test → function
/// - Service → class
/// - etc.
fn node_kind_to_scope_type(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function | NodeKind::Test => "function",
        NodeKind::Method => "method",
        NodeKind::Class | NodeKind::Service => "class",
        NodeKind::Interface | NodeKind::Trait => "interface",
        NodeKind::Struct => "struct",
        NodeKind::Enum => "enum",
        NodeKind::Module => "module",
        NodeKind::Macro => "macro",
        NodeKind::Component => "component",
        NodeKind::Resource | NodeKind::Endpoint => "resource",
        // Non-container types
        NodeKind::Variable => "variable",
        NodeKind::Constant => "constant",
        NodeKind::Type => "type",
        NodeKind::EnumVariant => "enumvariant",
        NodeKind::Import => "import",
        NodeKind::Export => "export",
        NodeKind::CallSite => "callsite",
        NodeKind::Parameter => "parameter",
        NodeKind::Property => "property",
        NodeKind::StyleRule => "style_rule",
        NodeKind::StyleAtRule => "style_at_rule",
        NodeKind::StyleVariable => "style_variable",
        NodeKind::Lifetime => "lifetime",
        NodeKind::Other => "other",
    }
}

fn match_scope(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    field: &str,
    operator: &Operator,
    value: &Value,
) -> bool {
    let scope_part = field.strip_prefix("scope.").unwrap_or("");
    match scope_part {
        "type" => match_scope_type(ctx, node_id, operator, value),
        "name" => match_scope_name(ctx, node_id, operator, value),
        "parent" => match_scope_parent_name(ctx, node_id, operator, value),
        "ancestor" => match_scope_ancestor_name(ctx, node_id, operator, value),
        _ => false,
    }
}

fn match_scope_type(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    operator: &Operator,
    value: &Value,
) -> bool {
    for edge in ctx.graph.edges().edges_to(node_id) {
        if let EdgeKind::Contains = &edge.kind
            && let Some(parent) = ctx.graph.nodes().get(edge.source)
        {
            // Use mapped scope type for parity with legacy index
            let scope_type = node_kind_to_scope_type(parent.kind);
            return match (operator, value) {
                // Case-sensitive comparison
                (Operator::Equal, Value::String(exp)) => scope_type == exp,
                (Operator::Regex, Value::Regex(rv)) => get_or_compile_regex(
                    &rv.pattern,
                    rv.flags.case_insensitive,
                    rv.flags.multiline,
                    rv.flags.dot_all,
                )
                .map(|re| re.is_match(scope_type))
                .unwrap_or(false),
                _ => false,
            };
        }
    }
    false
}

fn match_scope_name(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    operator: &Operator,
    value: &Value,
) -> bool {
    for edge in ctx.graph.edges().edges_to(node_id) {
        if let EdgeKind::Contains = &edge.kind
            && let Some(parent) = ctx.graph.nodes().get(edge.source)
            && let Some(name) = ctx.graph.strings().resolve(parent.name)
        {
            return match (operator, value) {
                // Use segments_match for qualified name suffix matching
                (Operator::Equal, Value::String(exp)) => segments_match(&name, exp),
                (Operator::Regex, Value::Regex(rv)) => get_or_compile_regex(
                    &rv.pattern,
                    rv.flags.case_insensitive,
                    rv.flags.multiline,
                    rv.flags.dot_all,
                )
                .map(|re| re.is_match(&name))
                .unwrap_or(false),
                _ => false,
            };
        }
    }
    false
}

/// `scope.parent:X` - find nodes with immediate parent NAMED X.
///
/// Uses `segments_match` for qualified name suffix matching (same as name: predicate).
fn match_scope_parent_name(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    operator: &Operator,
    value: &Value,
) -> bool {
    for edge in ctx.graph.edges().edges_to(node_id) {
        if let EdgeKind::Contains = &edge.kind
            && let Some(parent) = ctx.graph.nodes().get(edge.source)
            && let Some(name) = ctx.graph.strings().resolve(parent.name)
        {
            return match (operator, value) {
                // Use segments_match for qualified name suffix matching
                (Operator::Equal, Value::String(exp)) => segments_match(&name, exp),
                (Operator::Regex, Value::Regex(rv)) => get_or_compile_regex(
                    &rv.pattern,
                    rv.flags.case_insensitive,
                    rv.flags.multiline,
                    rv.flags.dot_all,
                )
                .map(|re| re.is_match(&name))
                .unwrap_or(false),
                _ => false,
            };
        }
    }
    false
}

/// `scope.ancestor:X` - find nodes with any ancestor NAMED X.
///
/// CYCLE PROTECTION: Uses visited set to prevent infinite loops.
/// Uses `segments_match` for qualified name suffix matching.
fn match_scope_ancestor_name(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    operator: &Operator,
    value: &Value,
) -> bool {
    let mut current = node_id;
    let mut visited = HashSet::new();
    visited.insert(node_id);

    loop {
        let mut found_parent = false;
        for edge in ctx.graph.edges().edges_to(current) {
            if let EdgeKind::Contains = &edge.kind {
                // CYCLE PROTECTION: Skip if we've already visited this node
                if visited.contains(&edge.source) {
                    continue;
                }
                visited.insert(edge.source);

                found_parent = true;
                current = edge.source;
                if let Some(parent) = ctx.graph.nodes().get(current)
                    && let Some(name) = ctx.graph.strings().resolve(parent.name)
                {
                    let matches = match (operator, value) {
                        // Use segments_match for qualified name suffix matching
                        (Operator::Equal, Value::String(exp)) => segments_match(&name, exp),
                        (Operator::Regex, Value::Regex(rv)) => get_or_compile_regex(
                            &rv.pattern,
                            rv.flags.case_insensitive,
                            rv.flags.multiline,
                            rv.flags.dot_all,
                        )
                        .map(|re| re.is_match(&name))
                        .unwrap_or(false),
                        _ => false,
                    };
                    if matches {
                        return true;
                    }
                }
                break;
            }
        }
        if !found_parent {
            break;
        }
    }
    false
}

// ============================================================================
// Subquery evaluation
// ============================================================================

/// Evaluate an expression against all nodes and return the set of matching node IDs.
///
/// Used for subquery evaluation: `callers:(kind:function AND async:true)`.
///
/// # Errors
///
/// Returns an error if predicate evaluation fails.
pub fn evaluate_subquery(ctx: &GraphEvalContext, expr: &Expr) -> Result<HashSet<NodeId>> {
    let recursion_limits = crate::config::RecursionLimits::load_or_default()?;
    let expr_depth = recursion_limits.effective_expr_depth()?;
    let mut guard = crate::query::security::RecursionGuard::new(expr_depth)?;

    let arena = ctx.graph.nodes();
    let mut matches = HashSet::new();
    for (id, _) in arena.iter() {
        if evaluate_node(ctx, id, expr, &mut guard)? {
            matches.insert(id);
        }
    }
    Ok(matches)
}

/// Match callers using a precomputed subquery result set.
///
/// Checks if any of this node's outgoing Calls edges target a node in the
/// precomputed set. Returns an error if the subquery was not precomputed
/// (indicates a bug in `precompute_subqueries`).
fn match_callers_subquery(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    subquery_matches: Option<&HashSet<NodeId>>,
) -> Result<bool> {
    let Some(matches) = subquery_matches else {
        return Err(anyhow!(
            "subquery cache miss: precompute_subqueries did not populate cache for this relation predicate"
        ));
    };
    for edge in ctx.graph.edges().edges_from(node_id) {
        if let EdgeKind::Calls { .. } = &edge.kind
            && matches.contains(&edge.target)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Match callees using a precomputed subquery result set.
fn match_callees_subquery(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    subquery_matches: Option<&HashSet<NodeId>>,
) -> Result<bool> {
    let Some(matches) = subquery_matches else {
        return Err(anyhow!(
            "subquery cache miss: precompute_subqueries did not populate cache for this relation predicate"
        ));
    };
    for edge in ctx.graph.edges().edges_to(node_id) {
        if let EdgeKind::Calls { .. } = &edge.kind
            && matches.contains(&edge.source)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Match imports using a precomputed subquery result set.
fn match_imports_subquery(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    subquery_matches: Option<&HashSet<NodeId>>,
) -> Result<bool> {
    let Some(matches) = subquery_matches else {
        return Err(anyhow!(
            "subquery cache miss: precompute_subqueries did not populate cache for this relation predicate"
        ));
    };
    let Some(entry) = ctx.graph.nodes().get(node_id) else {
        return Ok(false);
    };

    // Check all nodes in the same file for import edges
    for (other_id, other_entry) in ctx.graph.nodes().iter() {
        if other_entry.file == entry.file {
            for edge in ctx.graph.edges().edges_from(other_id) {
                if let EdgeKind::Imports { .. } = &edge.kind
                    && matches.contains(&edge.target)
                {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// Match exports using a precomputed subquery result set.
fn match_exports_subquery(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    subquery_matches: Option<&HashSet<NodeId>>,
) -> Result<bool> {
    let Some(matches) = subquery_matches else {
        return Err(anyhow!(
            "subquery cache miss: precompute_subqueries did not populate cache for this relation predicate"
        ));
    };
    for edge in ctx.graph.edges().edges_from(node_id) {
        if let EdgeKind::Exports { .. } = &edge.kind
            && matches.contains(&edge.target)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Match implements using a precomputed subquery result set.
fn match_implements_subquery(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    subquery_matches: Option<&HashSet<NodeId>>,
) -> Result<bool> {
    let Some(matches) = subquery_matches else {
        return Err(anyhow!(
            "subquery cache miss: precompute_subqueries did not populate cache for this relation predicate"
        ));
    };
    for edge in ctx.graph.edges().edges_from(node_id) {
        if let EdgeKind::Implements = &edge.kind
            && matches.contains(&edge.target)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Match references using a precomputed subquery result set.
fn match_references_subquery(
    ctx: &GraphEvalContext,
    node_id: NodeId,
    subquery_matches: Option<&HashSet<NodeId>>,
) -> Result<bool> {
    let Some(matches) = subquery_matches else {
        return Err(anyhow!(
            "subquery cache miss: precompute_subqueries did not populate cache for this relation predicate"
        ));
    };
    for edge in ctx.graph.edges().edges_to(node_id) {
        let is_reference = matches!(
            &edge.kind,
            EdgeKind::References
                | EdgeKind::Calls { .. }
                | EdgeKind::Imports { .. }
                | EdgeKind::FfiCall { .. }
        );
        if is_reference && matches.contains(&edge.source) {
            return Ok(true);
        }
    }
    Ok(false)
}

// ============================================================================
// Join evaluation
// ============================================================================

/// Evaluate a join expression, returning matched (left, right) node pairs.
///
/// For `(kind:function AND lang:rust) CALLS (kind:function AND lang:python)`:
/// 1. Evaluate LHS query → set of matching nodes
/// 2. Evaluate RHS query → set of matching nodes
/// 3. For each LHS node, check edges of the specified kind
/// 4. If edge target is in RHS set → add (lhs, rhs) pair
///
/// # Errors
///
/// Returns an error if subquery evaluation or edge traversal fails.
pub fn evaluate_join(
    ctx: &GraphEvalContext,
    join: &JoinExpr,
    max_results: Option<usize>,
) -> Result<JoinEvalResult> {
    let lhs_matches = evaluate_subquery(ctx, &join.left)?;
    let rhs_matches = evaluate_subquery(ctx, &join.right)?;
    let cap = max_results.unwrap_or(DEFAULT_JOIN_RESULT_CAP);

    let mut pairs = Vec::new();
    let mut truncated = false;
    'outer: for &lhs_id in &lhs_matches {
        for edge in ctx.graph.edges().edges_from(lhs_id) {
            if edge_matches_join_kind(&edge.kind, &join.edge) && rhs_matches.contains(&edge.target)
            {
                pairs.push((lhs_id, edge.target));
                if pairs.len() >= cap {
                    truncated = true;
                    break 'outer;
                }
            }
        }
    }
    Ok(JoinEvalResult { pairs, truncated })
}

/// Result of a join evaluation including truncation metadata.
pub struct JoinEvalResult {
    /// The matched (source, target) node ID pairs.
    pub pairs: Vec<(NodeId, NodeId)>,
    /// Whether the result set was truncated by the result cap.
    pub truncated: bool,
}

/// Default result cap for join queries.
///
/// Prevents unbounded memory growth when a join produces a large number of matching pairs.
const DEFAULT_JOIN_RESULT_CAP: usize = 10_000;

/// Check if an edge kind matches a join edge kind.
fn edge_matches_join_kind(edge_kind: &EdgeKind, join_kind: &JoinEdgeKind) -> bool {
    match join_kind {
        JoinEdgeKind::Calls => matches!(edge_kind, EdgeKind::Calls { .. }),
        JoinEdgeKind::Imports => matches!(edge_kind, EdgeKind::Imports { .. }),
        JoinEdgeKind::Inherits => matches!(edge_kind, EdgeKind::Inherits),
        JoinEdgeKind::Implements => matches!(edge_kind, EdgeKind::Implements),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Language;
    use crate::query::types::{Condition, Field, Span};
    use std::path::Path;

    #[test]
    fn test_collect_import_targets_empty() {
        // Empty And expression has no import targets
        let expr = Expr::And(vec![]);
        let targets = collect_import_targets(&expr);
        assert!(targets.is_empty());
    }

    #[test]
    fn test_collect_import_targets_single() {
        let cond = Condition {
            field: Field("imports".to_string()),
            operator: Operator::Equal,
            value: Value::String("react".to_string()),
            span: Span::default(),
        };
        let expr = Expr::Condition(cond);
        let targets = collect_import_targets(&expr);
        assert_eq!(targets.len(), 1);
        assert!(targets.contains("react"));
    }

    #[test]
    fn test_collect_import_targets_nested() {
        let cond1 = Condition {
            field: Field("imports".to_string()),
            operator: Operator::Equal,
            value: Value::String("react".to_string()),
            span: Span::default(),
        };
        let cond2 = Condition {
            field: Field("imports".to_string()),
            operator: Operator::Equal,
            value: Value::String("lodash".to_string()),
            span: Span::default(),
        };
        let expr = Expr::And(vec![Expr::Condition(cond1), Expr::Condition(cond2)]);
        let targets = collect_import_targets(&expr);
        assert_eq!(targets.len(), 2);
        assert!(targets.contains("react"));
        assert!(targets.contains("lodash"));
    }

    #[test]
    fn test_import_text_matches_canonicalized_qualified_imports() {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(Path::new("src/FileProcessor.cs"))
            .unwrap();
        assert!(graph.files_mut().set_language(file_id, Language::CSharp));

        assert!(import_text_matches(
            &graph,
            file_id,
            "System::IO",
            "System.IO"
        ));
        assert!(import_text_matches(
            &graph,
            file_id,
            "System::Collections::Generic",
            "System.Collections.Generic"
        ));
        assert!(!import_text_matches(
            &graph,
            file_id,
            "System::Text",
            "System.IO"
        ));
    }

    #[test]
    fn test_language_aware_segments_match_supports_ruby_method_separators() {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(Path::new("app/models/user.rb"))
            .unwrap();
        assert!(graph.files_mut().set_language(file_id, Language::Ruby));

        assert!(language_aware_segments_match(
            &graph,
            file_id,
            "Admin::Users::Controller::show",
            "Admin::Users::Controller#show"
        ));
        assert!(language_aware_segments_match(
            &graph,
            file_id,
            "Admin::Users::Controller::show",
            "show"
        ));
        assert!(!language_aware_segments_match(
            &graph,
            file_id,
            "Admin::Users::Controller::index",
            "Admin::Users::Controller#show"
        ));
    }

    #[test]
    fn test_normalize_kind() {
        // Case-sensitive synonyms
        assert_eq!(normalize_kind("trait"), "interface");
        assert_eq!(normalize_kind("TRAIT"), "TRAIT"); // Case-sensitive: not a synonym
        assert_eq!(normalize_kind("field"), "property");
        assert_eq!(normalize_kind("namespace"), "module");
        assert_eq!(normalize_kind("function"), "function"); // unchanged
    }

    #[test]
    fn test_graph_eval_context_builder() {
        let graph = CodeGraph::new();
        let pm = PluginManager::new();
        let ctx = GraphEvalContext::new(&graph, &pm)
            .with_workspace_root(Path::new("/test"))
            .with_parallel_disabled(true);

        assert!(ctx.disable_parallel);
        assert_eq!(ctx.workspace_root, Some(Path::new("/test")));
    }

    // ================================================================
    // collect_subquery_exprs tests
    // ================================================================

    /// Helper: build `field:(inner_expr)` as a Condition with Value::Subquery.
    fn subquery_condition(field: &str, inner: Expr, start: usize, end: usize) -> Expr {
        Expr::Condition(Condition {
            field: Field(field.to_string()),
            operator: Operator::Equal,
            value: Value::Subquery(Box::new(inner)),
            span: Span::with_position(start, end, 1, start + 1),
        })
    }

    /// Helper: build a simple `kind:function` condition.
    fn kind_condition(kind: &str) -> Expr {
        Expr::Condition(Condition {
            field: Field("kind".to_string()),
            operator: Operator::Equal,
            value: Value::String(kind.to_string()),
            span: Span::default(),
        })
    }

    #[test]
    fn test_collect_subquery_exprs_post_order_depth_2() {
        // Build: callers:(callees:(kind:function))
        // Inner subquery: callees:(kind:function) at span (20, 40)
        // Outer subquery: callers:(...) at span (0, 50)
        let inner_subquery = subquery_condition("callees", kind_condition("function"), 20, 40);
        let outer_subquery = subquery_condition("callers", inner_subquery, 0, 50);

        let mut out = Vec::new();
        collect_subquery_exprs(&outer_subquery, &mut out);

        // Post-order: inner appears before outer
        assert_eq!(
            out.len(),
            2,
            "should collect both inner and outer subqueries"
        );
        assert_eq!(out[0].0, (20, 40), "inner subquery span should come first");
        assert_eq!(out[1].0, (0, 50), "outer subquery span should come second");
    }

    #[test]
    fn test_collect_subquery_exprs_post_order_depth_3() {
        // Build: callers:(callees:(imports:(kind:function)))
        let innermost = subquery_condition("imports", kind_condition("function"), 30, 50);
        let middle = subquery_condition("callees", innermost, 15, 55);
        let outer = subquery_condition("callers", middle, 0, 60);

        let mut out = Vec::new();
        collect_subquery_exprs(&outer, &mut out);

        assert_eq!(out.len(), 3, "should collect all three nested subqueries");
        assert_eq!(out[0].0, (30, 50), "innermost should come first");
        assert_eq!(out[1].0, (15, 55), "middle should come second");
        assert_eq!(out[2].0, (0, 60), "outer should come last");
    }

    #[test]
    fn test_collect_subquery_exprs_and_or_branches() {
        // Build: callers:(kind:function) AND callees:(kind:method)
        let left = subquery_condition("callers", kind_condition("function"), 0, 25);
        let right = subquery_condition("callees", kind_condition("method"), 30, 55);
        let expr = Expr::And(vec![left, right]);

        let mut out = Vec::new();
        collect_subquery_exprs(&expr, &mut out);

        assert_eq!(out.len(), 2, "should collect subqueries from both branches");
        assert_eq!(out[0].0, (0, 25), "left branch subquery");
        assert_eq!(out[1].0, (30, 55), "right branch subquery");
    }

    #[test]
    fn test_collect_subquery_exprs_no_subqueries() {
        // Simple condition with no subqueries: kind:function
        let expr = kind_condition("function");

        let mut out = Vec::new();
        collect_subquery_exprs(&expr, &mut out);

        assert!(
            out.is_empty(),
            "should collect nothing for plain conditions"
        );
    }

    // ================================================================
    // FfiCall edge in references/referenced_by predicates
    // ================================================================

    use crate::graph::unified::edge::{BidirectionalEdgeStore, FfiConvention};
    use crate::graph::unified::storage::{
        AuxiliaryIndices, FileRegistry, NodeArena, StringInterner,
    };

    /// Build a `CodeGraph` with: `caller --FfiCall(C)--> target`
    fn build_ffi_graph() -> (CodeGraph, NodeId, NodeId) {
        let mut arena = NodeArena::new();
        let edges = BidirectionalEdgeStore::new();
        let mut strings = StringInterner::new();
        let mut files = FileRegistry::new();
        let mut indices = AuxiliaryIndices::new();

        let caller_name = strings.intern("caller_fn").unwrap();
        let target_name = strings.intern("ffi_target").unwrap();
        let file_id = files.register(Path::new("test.r")).unwrap();

        let caller_id = arena
            .alloc(NodeEntry {
                kind: NodeKind::Function,
                name: caller_name,
                file: file_id,
                start_byte: 0,
                end_byte: 100,
                start_line: 1,
                start_column: 0,
                end_line: 5,
                end_column: 0,
                signature: None,
                doc: None,
                qualified_name: None,
                visibility: None,
                is_async: false,
                is_static: false,
                is_unsafe: false,
                body_hash: None,
            })
            .unwrap();

        let target_id = arena
            .alloc(NodeEntry {
                kind: NodeKind::Function,
                name: target_name,
                file: file_id,
                start_byte: 200,
                end_byte: 300,
                start_line: 10,
                start_column: 0,
                end_line: 15,
                end_column: 0,
                signature: None,
                doc: None,
                qualified_name: None,
                visibility: None,
                is_async: false,
                is_static: false,
                is_unsafe: false,
                body_hash: None,
            })
            .unwrap();

        indices.add(caller_id, NodeKind::Function, caller_name, None, file_id);
        indices.add(target_id, NodeKind::Function, target_name, None, file_id);

        edges.add_edge(
            caller_id,
            target_id,
            EdgeKind::FfiCall {
                convention: FfiConvention::C,
            },
            file_id,
        );

        let graph = CodeGraph::from_components(
            arena,
            edges,
            strings,
            files,
            indices,
            crate::graph::unified::NodeMetadataStore::new(),
        );
        (graph, caller_id, target_id)
    }

    #[test]
    fn test_ffi_call_edge_in_references_predicate() {
        let (graph, _caller_id, target_id) = build_ffi_graph();
        let pm = PluginManager::new();
        let ctx = GraphEvalContext::new(&graph, &pm);

        // `references:ffi_target` should match because there is an FfiCall edge to it
        let result = match_references(
            &ctx,
            target_id,
            &Operator::Equal,
            &Value::String("ffi_target".to_string()),
        );
        assert!(result, "references: predicate should match FfiCall edges");
    }

    #[test]
    fn test_ffi_call_edge_in_references_subquery() {
        let (graph, caller_id, target_id) = build_ffi_graph();
        let pm = PluginManager::new();
        let ctx = GraphEvalContext::new(&graph, &pm);

        // Simulate a subquery that matched the caller node
        let mut subquery_results = HashSet::new();
        subquery_results.insert(caller_id);

        // match_references_subquery checks incoming edges to target_id
        // and verifies the source is in the subquery result set
        let result = match_references_subquery(&ctx, target_id, Some(&subquery_results)).unwrap();
        assert!(
            result,
            "references subquery should match FfiCall edge sources"
        );
    }
}
