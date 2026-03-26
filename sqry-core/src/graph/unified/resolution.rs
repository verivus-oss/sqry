//! Shared, file-aware symbol resolution for unified graph snapshots.
//!
//! This module centralizes strict single-symbol lookup and ordered candidate
//! discovery so MCP, LSP, and CLI consumers do not drift semantically.

use std::path::Path;

use crate::graph::node::Language;
use crate::graph::unified::concurrent::GraphSnapshot;
use crate::graph::unified::file::id::FileId;
use crate::graph::unified::node::id::NodeId;
use crate::graph::unified::node::kind::NodeKind;

/// File scoping policy for a symbol query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileScope<'a> {
    /// Search without file restriction.
    Any,
    /// Restrict lookup to a concrete file path.
    Path(&'a Path),
    /// Restrict lookup to a resolved file id.
    FileId(FileId),
}

/// Resolution mode for a symbol query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionMode {
    /// Only exact qualified-name and exact simple-name buckets are eligible.
    Strict,
    /// Allow bounded canonical `::` suffix candidates after exact buckets.
    AllowSuffixCandidates,
}

/// Symbol lookup input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolQuery<'a> {
    /// Raw symbol text from the caller.
    pub symbol: &'a str,
    /// File scoping policy.
    pub file_scope: FileScope<'a>,
    /// Resolution mode.
    pub mode: ResolutionMode,
}

/// Resolved file scope once path normalization has completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedFileScope {
    /// No file restriction.
    Any,
    /// Restriction to one indexed file.
    File(FileId),
}

/// File-scope resolution failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileScopeError {
    /// Requested file is not indexed in the current graph snapshot.
    FileNotIndexed,
}

/// Normalized symbol query used internally by the resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedSymbolQuery {
    /// Canonical graph symbol text.
    pub symbol: String,
    /// Resolved file scope.
    pub file_scope: ResolvedFileScope,
    /// Resolution mode.
    pub mode: ResolutionMode,
}

/// Single-node resolution outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolResolutionOutcome {
    /// Exactly one node matched.
    Resolved(NodeId),
    /// No node matched.
    NotFound,
    /// Requested file is valid but absent from indexed graph data.
    FileNotIndexed,
    /// More than one node matched after deterministic ordering.
    Ambiguous(Vec<NodeId>),
}

/// Candidate enumeration outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolCandidateOutcome {
    /// Ordered candidates from the first non-empty bucket.
    Candidates(Vec<NodeId>),
    /// No node matched.
    NotFound,
    /// Requested file is valid but absent from indexed graph data.
    FileNotIndexed,
}

impl GraphSnapshot {
    /// Resolves one symbol with explicit file-aware outcome classification.
    #[must_use]
    pub fn resolve_symbol(&self, query: &SymbolQuery<'_>) -> SymbolResolutionOutcome {
        match self.find_symbol_candidates(query) {
            SymbolCandidateOutcome::Candidates(candidates) => match candidates.as_slice() {
                [] => SymbolResolutionOutcome::NotFound,
                [node_id] => SymbolResolutionOutcome::Resolved(*node_id),
                _ => SymbolResolutionOutcome::Ambiguous(candidates),
            },
            SymbolCandidateOutcome::NotFound => SymbolResolutionOutcome::NotFound,
            SymbolCandidateOutcome::FileNotIndexed => SymbolResolutionOutcome::FileNotIndexed,
        }
    }

    /// Finds ordered candidates from the first eligible non-empty bucket.
    #[must_use]
    pub fn find_symbol_candidates(&self, query: &SymbolQuery<'_>) -> SymbolCandidateOutcome {
        let resolved_file_scope = match self.resolve_file_scope(&query.file_scope) {
            Ok(scope) => scope,
            Err(FileScopeError::FileNotIndexed) => return SymbolCandidateOutcome::FileNotIndexed,
        };

        let normalized_query = self.normalize_symbol_query(query, &resolved_file_scope);

        let exact_qualified = self.filtered_bucket(
            self.exact_qualified_bucket(&normalized_query),
            resolved_file_scope,
        );
        if !exact_qualified.is_empty() {
            return SymbolCandidateOutcome::Candidates(exact_qualified);
        }

        let exact_simple = self.filtered_bucket(
            self.exact_simple_bucket(&normalized_query),
            resolved_file_scope,
        );
        if !exact_simple.is_empty() {
            return SymbolCandidateOutcome::Candidates(exact_simple);
        }

        if matches!(normalized_query.mode, ResolutionMode::AllowSuffixCandidates) {
            let suffix_candidates = self.filtered_bucket(
                self.bounded_suffix_bucket(&normalized_query),
                resolved_file_scope,
            );
            if !suffix_candidates.is_empty() {
                return SymbolCandidateOutcome::Candidates(suffix_candidates);
            }
        }

        SymbolCandidateOutcome::NotFound
    }

    /// Resolves an external file scope into an indexed file scope.
    ///
    /// # Errors
    ///
    /// Returns [`FileScopeError::FileNotIndexed`] when the requested file scope
    /// is not present in the loaded graph indices.
    pub fn resolve_file_scope(
        &self,
        file_scope: &FileScope<'_>,
    ) -> Result<ResolvedFileScope, FileScopeError> {
        match *file_scope {
            FileScope::Any => Ok(ResolvedFileScope::Any),
            FileScope::Path(path) => self
                .files()
                .get(path)
                .filter(|file_id| !self.indices().by_file(*file_id).is_empty())
                .map_or(Err(FileScopeError::FileNotIndexed), |file_id| {
                    Ok(ResolvedFileScope::File(file_id))
                }),
            FileScope::FileId(file_id) => {
                let is_indexed = self.files().resolve(file_id).is_some()
                    && !self.indices().by_file(file_id).is_empty();
                if is_indexed {
                    Ok(ResolvedFileScope::File(file_id))
                } else {
                    Err(FileScopeError::FileNotIndexed)
                }
            }
        }
    }

    /// Normalizes a raw symbol query into canonical graph form.
    #[must_use]
    pub fn normalize_symbol_query(
        &self,
        query: &SymbolQuery<'_>,
        file_scope: &ResolvedFileScope,
    ) -> NormalizedSymbolQuery {
        let normalized_symbol = match *file_scope {
            ResolvedFileScope::Any => query.symbol.to_string(),
            ResolvedFileScope::File(file_id) => {
                self.files().language_for_file(file_id).map_or_else(
                    || query.symbol.to_string(),
                    |language| canonicalize_graph_qualified_name(language, query.symbol),
                )
            }
        };

        NormalizedSymbolQuery {
            symbol: normalized_symbol,
            file_scope: *file_scope,
            mode: query.mode,
        }
    }

    fn exact_qualified_bucket(&self, query: &NormalizedSymbolQuery) -> Vec<NodeId> {
        self.strings()
            .get(&query.symbol)
            .map_or_else(Vec::new, |string_id| {
                self.indices().by_qualified_name(string_id).to_vec()
            })
    }

    fn exact_simple_bucket(&self, query: &NormalizedSymbolQuery) -> Vec<NodeId> {
        self.strings()
            .get(&query.symbol)
            .map_or_else(Vec::new, |string_id| {
                self.indices().by_name(string_id).to_vec()
            })
    }

    fn bounded_suffix_bucket(&self, query: &NormalizedSymbolQuery) -> Vec<NodeId> {
        if !query.symbol.contains("::") {
            return Vec::new();
        }

        let Some(leaf_symbol) = query.symbol.rsplit("::").next() else {
            return Vec::new();
        };
        let Some(leaf_id) = self.strings().get(leaf_symbol) else {
            return Vec::new();
        };
        let suffix_pattern = format!("::{}", query.symbol);

        self.indices()
            .by_name(leaf_id)
            .iter()
            .copied()
            .filter(|node_id| {
                self.get_node(*node_id)
                    .and_then(|entry| entry.qualified_name)
                    .and_then(|qualified_name_id| self.strings().resolve(qualified_name_id))
                    .is_some_and(|qualified_name| {
                        qualified_name.as_ref() == query.symbol
                            || qualified_name.as_ref().ends_with(&suffix_pattern)
                    })
            })
            .collect()
    }

    fn filtered_bucket(
        &self,
        mut bucket: Vec<NodeId>,
        file_scope: ResolvedFileScope,
    ) -> Vec<NodeId> {
        if let ResolvedFileScope::File(file_id) = file_scope {
            let file_nodes = self.indices().by_file(file_id);
            bucket.retain(|node_id| file_nodes.contains(node_id));
        }

        bucket.sort_by(|left, right| {
            self.candidate_sort_key(*left)
                .cmp(&self.candidate_sort_key(*right))
        });
        bucket.dedup();
        bucket
    }

    fn candidate_sort_key(&self, node_id: NodeId) -> CandidateSortKey {
        let Some(entry) = self.get_node(node_id) else {
            return CandidateSortKey::default_for(node_id);
        };

        let file_path = self
            .files()
            .resolve(entry.file)
            .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
        let qualified_name = entry
            .qualified_name
            .and_then(|string_id| self.strings().resolve(string_id))
            .map_or_else(String::new, |value| value.to_string());
        let simple_name = self
            .strings()
            .resolve(entry.name)
            .map_or_else(String::new, |value| value.to_string());

        CandidateSortKey {
            file_path,
            start_line: entry.start_line,
            start_column: entry.start_column,
            end_line: entry.end_line,
            end_column: entry.end_column,
            kind: entry.kind.as_str().to_string(),
            qualified_name,
            simple_name,
            node_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CandidateSortKey {
    file_path: String,
    start_line: u32,
    start_column: u32,
    end_line: u32,
    end_column: u32,
    kind: String,
    qualified_name: String,
    simple_name: String,
    node_id: NodeId,
}

impl CandidateSortKey {
    fn default_for(node_id: NodeId) -> Self {
        Self {
            file_path: String::new(),
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
            kind: String::new(),
            qualified_name: String::new(),
            simple_name: String::new(),
            node_id,
        }
    }
}

/// Canonicalize a language-native qualified name into graph-internal `::` form.
#[must_use]
pub(crate) fn canonicalize_graph_qualified_name(language: Language, symbol: &str) -> String {
    if should_skip_qualified_name_normalization(symbol) {
        return symbol.to_string();
    }

    if language == Language::R {
        return canonicalize_r_qualified_name(symbol);
    }

    let mut normalized = symbol.to_string();
    for delimiter in native_delimiters(language) {
        if normalized.contains(delimiter) {
            normalized = normalized.replace(delimiter, "::");
        }
    }
    normalized
}

/// Returns `true` when a qualified name is already in graph-canonical form.
#[must_use]
pub(crate) fn is_canonical_graph_qualified_name(language: Language, symbol: &str) -> bool {
    should_skip_qualified_name_normalization(symbol)
        || canonicalize_graph_qualified_name(language, symbol) == symbol
}

fn should_skip_qualified_name_normalization(symbol: &str) -> bool {
    symbol.starts_with('<')
        || symbol.contains('/')
        || symbol.starts_with("wasm::")
        || symbol.starts_with("ffi::")
        || symbol.starts_with("extern::")
        || symbol.starts_with("native::")
}

fn canonicalize_r_qualified_name(symbol: &str) -> String {
    let search_start = usize::from(symbol.starts_with('.'));
    let Some(relative_split_index) = symbol[search_start..].rfind('.') else {
        return symbol.to_string();
    };

    let split_index = search_start + relative_split_index;
    let prefix = &symbol[..split_index];
    let suffix = &symbol[split_index + 1..];
    if suffix.is_empty() {
        return symbol.to_string();
    }

    format!("{prefix}::{suffix}")
}

/// Convert a canonical graph qualified name into native language display form.
#[must_use]
pub fn display_graph_qualified_name(
    language: Language,
    qualified: &str,
    kind: NodeKind,
    is_static: bool,
) -> String {
    if should_skip_qualified_name_normalization(qualified) {
        return qualified.to_string();
    }

    match language {
        Language::Ruby => display_ruby_qualified_name(qualified, kind, is_static),
        Language::Php => display_php_qualified_name(qualified, kind),
        _ => native_display_separator(language).map_or_else(
            || qualified.to_string(),
            |separator| qualified.replace("::", separator),
        ),
    }
}

pub(crate) fn native_delimiters(language: Language) -> &'static [&'static str] {
    match language {
        Language::JavaScript
        | Language::Python
        | Language::TypeScript
        | Language::Java
        | Language::CSharp
        | Language::Kotlin
        | Language::Scala
        | Language::Go
        | Language::Css
        | Language::Sql
        | Language::Dart
        | Language::Lua
        | Language::Perl
        | Language::Groovy
        | Language::Elixir
        | Language::R
        | Language::Haskell
        | Language::Html
        | Language::Svelte
        | Language::Vue
        | Language::Terraform
        | Language::Puppet
        | Language::Pulumi
        | Language::Http
        | Language::Plsql
        | Language::Apex
        | Language::Abap
        | Language::ServiceNow
        | Language::Swift
        | Language::Zig
        | Language::Json => &["."],
        Language::Ruby => &["#", "."],
        Language::Php => &["\\", "->"],
        Language::C | Language::Cpp | Language::Rust | Language::Shell => &[],
    }
}

fn native_display_separator(language: Language) -> Option<&'static str> {
    match language {
        Language::C
        | Language::Cpp
        | Language::Rust
        | Language::Shell
        | Language::Php
        | Language::Ruby => None,
        _ => Some("."),
    }
}

fn display_ruby_qualified_name(qualified: &str, kind: NodeKind, is_static: bool) -> String {
    if qualified.contains('#') || qualified.contains('.') || !qualified.contains("::") {
        return qualified.to_string();
    }

    match kind {
        NodeKind::Method => {
            replace_last_separator(qualified, if is_static { "." } else { "#" }, false)
        }
        NodeKind::Variable if should_display_ruby_member_variable(qualified) => {
            replace_last_separator(qualified, "#", false)
        }
        _ => qualified.to_string(),
    }
}

fn should_display_ruby_member_variable(qualified: &str) -> bool {
    let Some((_, suffix)) = qualified.rsplit_once("::") else {
        return false;
    };

    if suffix.starts_with("@@")
        || suffix
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_uppercase())
    {
        return false;
    }

    suffix.starts_with('@')
        || suffix
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase() || character == '_')
}

fn display_php_qualified_name(qualified: &str, kind: NodeKind) -> String {
    if !qualified.contains("::") {
        return qualified.to_string();
    }

    if matches!(kind, NodeKind::Method | NodeKind::Property) {
        return replace_last_separator(qualified, "::", true);
    }

    qualified.replace("::", "\\")
}

fn replace_last_separator(qualified: &str, final_separator: &str, preserve_prefix: bool) -> String {
    let Some((prefix, suffix)) = qualified.rsplit_once("::") else {
        return qualified.to_string();
    };

    let display_prefix = if preserve_prefix {
        prefix.replace("::", "\\")
    } else {
        prefix.to_string()
    };

    if display_prefix.is_empty() {
        suffix.to_string()
    } else {
        format!("{display_prefix}{final_separator}{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::graph::node::Language;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::node::id::NodeId;
    use crate::graph::unified::node::kind::NodeKind;
    use crate::graph::unified::storage::arena::NodeEntry;

    use super::{
        FileScope, NormalizedSymbolQuery, ResolutionMode, ResolvedFileScope,
        SymbolCandidateOutcome, SymbolQuery, SymbolResolutionOutcome,
        canonicalize_graph_qualified_name, display_graph_qualified_name,
    };

    struct TestNode {
        node_id: NodeId,
    }

    #[test]
    fn test_resolve_symbol_exact_qualified_same_file() {
        let mut graph = CodeGraph::new();
        let file_path = abs_path("src/lib.rs");
        let symbol = add_node(
            &mut graph,
            NodeKind::Function,
            "target",
            Some("pkg::target"),
            &file_path,
            Some(Language::Rust),
            10,
            2,
        );

        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "pkg::target",
            file_scope: FileScope::Path(&file_path),
            mode: ResolutionMode::Strict,
        };

        assert_eq!(
            snapshot.resolve_symbol(&query),
            SymbolResolutionOutcome::Resolved(symbol.node_id)
        );
    }

    #[test]
    fn test_resolve_symbol_exact_simple_same_file_wins() {
        let mut graph = CodeGraph::new();
        let requested_path = abs_path("src/requested.rs");
        let other_path = abs_path("src/other.rs");

        let requested = add_node(
            &mut graph,
            NodeKind::Function,
            "target",
            Some("requested::target"),
            &requested_path,
            Some(Language::Rust),
            4,
            0,
        );
        let _other = add_node(
            &mut graph,
            NodeKind::Function,
            "target",
            Some("other::target"),
            &other_path,
            Some(Language::Rust),
            1,
            0,
        );

        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "target",
            file_scope: FileScope::Path(&requested_path),
            mode: ResolutionMode::Strict,
        };

        assert_eq!(
            snapshot.resolve_symbol(&query),
            SymbolResolutionOutcome::Resolved(requested.node_id)
        );
    }

    #[test]
    fn test_resolve_symbol_returns_not_found_without_wrong_file_fallback() {
        let mut graph = CodeGraph::new();
        let requested_path = abs_path("src/requested.rs");
        let other_path = abs_path("src/other.rs");

        let _requested_index_anchor = add_node(
            &mut graph,
            NodeKind::Function,
            "anchor",
            Some("requested::anchor"),
            &requested_path,
            Some(Language::Rust),
            1,
            0,
        );
        let _other = add_node(
            &mut graph,
            NodeKind::Function,
            "target",
            Some("other::target"),
            &other_path,
            Some(Language::Rust),
            3,
            0,
        );

        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "target",
            file_scope: FileScope::Path(&requested_path),
            mode: ResolutionMode::Strict,
        };

        assert_eq!(
            snapshot.resolve_symbol(&query),
            SymbolResolutionOutcome::NotFound
        );
    }

    #[test]
    fn test_resolve_symbol_returns_file_not_indexed_for_valid_unindexed_path() {
        let mut graph = CodeGraph::new();
        let indexed_path = abs_path("src/indexed.rs");
        let unindexed_path = abs_path("src/unindexed.rs");

        add_node(
            &mut graph,
            NodeKind::Function,
            "indexed",
            Some("pkg::indexed"),
            &indexed_path,
            Some(Language::Rust),
            1,
            0,
        );
        graph
            .files_mut()
            .register_with_language(&unindexed_path, Some(Language::Rust))
            .unwrap();

        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "indexed",
            file_scope: FileScope::Path(&unindexed_path),
            mode: ResolutionMode::Strict,
        };

        assert_eq!(
            snapshot.resolve_symbol(&query),
            SymbolResolutionOutcome::FileNotIndexed
        );
    }

    #[test]
    fn test_resolve_symbol_returns_ambiguous_for_multi_match_bucket() {
        let mut graph = CodeGraph::new();
        let file_path = abs_path("src/lib.rs");

        let first = add_node(
            &mut graph,
            NodeKind::Function,
            "dup",
            Some("pkg::dup"),
            &file_path,
            Some(Language::Rust),
            2,
            0,
        );
        let second = add_node(
            &mut graph,
            NodeKind::Method,
            "dup",
            Some("pkg::dup_method"),
            &file_path,
            Some(Language::Rust),
            8,
            0,
        );

        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "dup",
            file_scope: FileScope::Path(&file_path),
            mode: ResolutionMode::Strict,
        };

        assert_eq!(
            snapshot.resolve_symbol(&query),
            SymbolResolutionOutcome::Ambiguous(vec![first.node_id, second.node_id])
        );
    }

    #[test]
    fn test_find_symbol_candidates_uses_first_non_empty_bucket_only() {
        let mut graph = CodeGraph::new();
        let qualified_path = abs_path("src/qualified.rs");
        let simple_path = abs_path("src/simple.rs");

        let qualified = add_node(
            &mut graph,
            NodeKind::Function,
            "target",
            Some("pkg::target"),
            &qualified_path,
            Some(Language::Rust),
            1,
            0,
        );
        let simple_only = add_node(
            &mut graph,
            NodeKind::Function,
            "pkg::target",
            None,
            &simple_path,
            Some(Language::Rust),
            1,
            0,
        );

        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "pkg::target",
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        };

        assert_eq!(
            snapshot.find_symbol_candidates(&query),
            SymbolCandidateOutcome::Candidates(vec![qualified.node_id])
        );
        assert_ne!(qualified.node_id, simple_only.node_id);
    }

    #[test]
    fn test_find_symbol_candidates_preserves_file_not_indexed() {
        let mut graph = CodeGraph::new();
        let indexed_path = abs_path("src/indexed.rs");
        let unindexed_path = abs_path("src/unindexed.rs");

        add_node(
            &mut graph,
            NodeKind::Function,
            "target",
            Some("pkg::target"),
            &indexed_path,
            Some(Language::Rust),
            1,
            0,
        );
        let unindexed_file_id = graph
            .files_mut()
            .register_with_language(&unindexed_path, Some(Language::Rust))
            .unwrap();

        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "target",
            file_scope: FileScope::FileId(unindexed_file_id),
            mode: ResolutionMode::AllowSuffixCandidates,
        };

        assert_eq!(
            snapshot.find_symbol_candidates(&query),
            SymbolCandidateOutcome::FileNotIndexed
        );
    }

    #[test]
    fn test_suffix_candidates_disabled_in_strict_mode() {
        let mut graph = CodeGraph::new();
        let file_path = abs_path("src/lib.rs");

        let suffix_match = add_node(
            &mut graph,
            NodeKind::Function,
            "target",
            Some("outer::pkg::target"),
            &file_path,
            Some(Language::Rust),
            1,
            0,
        );

        let snapshot = graph.snapshot();
        let strict_query = SymbolQuery {
            symbol: "pkg::target",
            file_scope: FileScope::Any,
            mode: ResolutionMode::Strict,
        };
        let suffix_query = SymbolQuery {
            mode: ResolutionMode::AllowSuffixCandidates,
            ..strict_query
        };

        assert_eq!(
            snapshot.resolve_symbol(&strict_query),
            SymbolResolutionOutcome::NotFound
        );
        assert_eq!(
            snapshot.find_symbol_candidates(&suffix_query),
            SymbolCandidateOutcome::Candidates(vec![suffix_match.node_id])
        );
    }

    #[test]
    fn test_suffix_candidates_require_canonical_qualified_query() {
        let mut graph = CodeGraph::new();
        let file_path = abs_path("src/mod.py");

        add_node(
            &mut graph,
            NodeKind::Function,
            "target",
            Some("pkg::target"),
            &file_path,
            Some(Language::Python),
            1,
            0,
        );

        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "pkg.target",
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        };

        assert_eq!(
            snapshot.find_symbol_candidates(&query),
            SymbolCandidateOutcome::NotFound
        );
    }

    #[test]
    fn test_suffix_candidates_filter_same_leaf_bucket_only() {
        let mut graph = CodeGraph::new();
        let file_path = abs_path("src/lib.rs");

        let exact_suffix = add_node(
            &mut graph,
            NodeKind::Function,
            "target",
            Some("outer::pkg::target"),
            &file_path,
            Some(Language::Rust),
            2,
            0,
        );
        let another_suffix = add_node(
            &mut graph,
            NodeKind::Method,
            "target",
            Some("another::pkg::target"),
            &file_path,
            Some(Language::Rust),
            4,
            0,
        );
        let unrelated = add_node(
            &mut graph,
            NodeKind::Function,
            "target",
            Some("pkg::different::target"),
            &file_path,
            Some(Language::Rust),
            6,
            0,
        );

        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "pkg::target",
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        };

        assert_eq!(
            snapshot.find_symbol_candidates(&query),
            SymbolCandidateOutcome::Candidates(vec![exact_suffix.node_id, another_suffix.node_id])
        );
        assert_ne!(unrelated.node_id, exact_suffix.node_id);
    }

    #[test]
    fn test_normalize_symbol_query_rewrites_native_delimiter_when_file_scope_language_known() {
        let mut graph = CodeGraph::new();
        let file_path = abs_path("src/mod.py");
        let file_id = graph
            .files_mut()
            .register_with_language(&file_path, Some(Language::Python))
            .unwrap();
        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "pkg.mod.fn",
            file_scope: FileScope::Path(&file_path),
            mode: ResolutionMode::Strict,
        };

        let normalized = snapshot.normalize_symbol_query(&query, &ResolvedFileScope::File(file_id));

        assert_eq!(
            normalized,
            NormalizedSymbolQuery {
                symbol: "pkg::mod::fn".to_string(),
                file_scope: ResolvedFileScope::File(file_id),
                mode: ResolutionMode::Strict,
            }
        );
    }

    #[test]
    fn test_normalize_symbol_query_rewrites_native_delimiter_for_csharp() {
        let mut graph = CodeGraph::new();
        let file_path = abs_path("src/Program.cs");
        let file_id = graph
            .files_mut()
            .register_with_language(&file_path, Some(Language::CSharp))
            .unwrap();
        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "System.Console.WriteLine",
            file_scope: FileScope::Path(&file_path),
            mode: ResolutionMode::Strict,
        };

        let normalized = snapshot.normalize_symbol_query(&query, &ResolvedFileScope::File(file_id));

        assert_eq!(normalized.symbol, "System::Console::WriteLine".to_string());
    }

    #[test]
    fn test_normalize_symbol_query_rewrites_native_delimiter_for_zig() {
        let mut graph = CodeGraph::new();
        let file_path = abs_path("src/main.zig");
        let file_id = graph
            .files_mut()
            .register_with_language(&file_path, Some(Language::Zig))
            .unwrap();
        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "std.os.linux.exit",
            file_scope: FileScope::Path(&file_path),
            mode: ResolutionMode::Strict,
        };

        let normalized = snapshot.normalize_symbol_query(&query, &ResolvedFileScope::File(file_id));

        assert_eq!(normalized.symbol, "std::os::linux::exit".to_string());
    }

    #[test]
    fn test_normalize_symbol_query_does_not_rewrite_when_file_scope_any() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "pkg.mod.fn",
            file_scope: FileScope::Any,
            mode: ResolutionMode::Strict,
        };

        let normalized = snapshot.normalize_symbol_query(&query, &ResolvedFileScope::Any);

        assert_eq!(
            normalized,
            NormalizedSymbolQuery {
                symbol: "pkg.mod.fn".to_string(),
                file_scope: ResolvedFileScope::Any,
                mode: ResolutionMode::Strict,
            }
        );
    }

    #[test]
    fn test_global_qualified_query_with_native_delimiter_is_exact_only_and_not_found() {
        let mut graph = CodeGraph::new();
        let file_path = abs_path("src/mod.py");

        add_node(
            &mut graph,
            NodeKind::Function,
            "fn",
            Some("pkg::mod::fn"),
            &file_path,
            Some(Language::Python),
            1,
            0,
        );

        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "pkg.mod.fn",
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        };

        assert_eq!(
            snapshot.resolve_symbol(&query),
            SymbolResolutionOutcome::NotFound
        );
    }

    #[test]
    fn test_global_canonical_qualified_query_can_hit_exact_qualified_bucket() {
        let mut graph = CodeGraph::new();
        let file_path = abs_path("src/lib.rs");
        let expected = add_node(
            &mut graph,
            NodeKind::Function,
            "fn",
            Some("pkg::mod::fn"),
            &file_path,
            Some(Language::Rust),
            1,
            0,
        );

        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "pkg::mod::fn",
            file_scope: FileScope::Any,
            mode: ResolutionMode::Strict,
        };

        assert_eq!(
            snapshot.resolve_symbol(&query),
            SymbolResolutionOutcome::Resolved(expected.node_id)
        );
    }

    #[test]
    fn test_candidate_order_uses_metadata_then_node_id() {
        let mut graph = CodeGraph::new();
        let file_path = abs_path("src/lib.rs");

        let first = add_node(
            &mut graph,
            NodeKind::Function,
            "dup",
            Some("pkg::dup_a"),
            &file_path,
            Some(Language::Rust),
            1,
            0,
        );
        let second = add_node(
            &mut graph,
            NodeKind::Function,
            "dup",
            Some("pkg::dup_b"),
            &file_path,
            Some(Language::Rust),
            1,
            0,
        );

        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "dup",
            file_scope: FileScope::Any,
            mode: ResolutionMode::Strict,
        };

        assert_eq!(
            snapshot.find_symbol_candidates(&query),
            SymbolCandidateOutcome::Candidates(vec![first.node_id, second.node_id])
        );
    }

    #[test]
    fn test_candidate_order_kind_sort_key_uses_node_kind_as_str() {
        let mut graph = CodeGraph::new();
        let file_path = abs_path("src/lib.rs");

        let function_node = add_node(
            &mut graph,
            NodeKind::Function,
            "shared",
            Some("pkg::shared_fn"),
            &file_path,
            Some(Language::Rust),
            1,
            0,
        );
        let variable_node = add_node(
            &mut graph,
            NodeKind::Variable,
            "shared",
            Some("pkg::shared_var"),
            &file_path,
            Some(Language::Rust),
            1,
            0,
        );

        let snapshot = graph.snapshot();
        let query = SymbolQuery {
            symbol: "shared",
            file_scope: FileScope::Any,
            mode: ResolutionMode::Strict,
        };

        assert_eq!(
            snapshot.find_symbol_candidates(&query),
            SymbolCandidateOutcome::Candidates(vec![function_node.node_id, variable_node.node_id])
        );
    }

    fn add_node(
        graph: &mut CodeGraph,
        kind: NodeKind,
        name: &str,
        qualified_name: Option<&str>,
        file_path: &Path,
        language: Option<Language>,
        start_line: u32,
        start_column: u32,
    ) -> TestNode {
        let name_id = graph.strings_mut().intern(name).unwrap();
        let qualified_name_id =
            qualified_name.map(|value| graph.strings_mut().intern(value).unwrap());
        let file_id = graph
            .files_mut()
            .register_with_language(file_path, language)
            .unwrap();

        let entry = NodeEntry::new(kind, name_id, file_id)
            .with_qualified_name_opt(qualified_name_id)
            .with_location(start_line, start_column, start_line, start_column + 1);

        let node_id = graph.nodes_mut().alloc(entry).unwrap();
        graph
            .indices_mut()
            .add(node_id, kind, name_id, qualified_name_id, file_id);

        TestNode { node_id }
    }

    trait NodeEntryExt {
        fn with_qualified_name_opt(
            self,
            qualified_name: Option<crate::graph::unified::string::id::StringId>,
        ) -> Self;
    }

    impl NodeEntryExt for NodeEntry {
        fn with_qualified_name_opt(
            mut self,
            qualified_name: Option<crate::graph::unified::string::id::StringId>,
        ) -> Self {
            self.qualified_name = qualified_name;
            self
        }
    }

    fn abs_path(relative: &str) -> PathBuf {
        PathBuf::from("/resolver-tests").join(relative)
    }

    #[test]
    fn test_display_graph_qualified_name_dot_language() {
        let display = display_graph_qualified_name(
            Language::CSharp,
            "MyApp::User::GetName",
            NodeKind::Method,
            false,
        );
        assert_eq!(display, "MyApp.User.GetName");
    }

    #[test]
    fn test_canonicalize_graph_qualified_name_r_private_name_preserved() {
        assert_eq!(
            canonicalize_graph_qualified_name(Language::R, ".private_func"),
            ".private_func"
        );
    }

    #[test]
    fn test_canonicalize_graph_qualified_name_r_s3_method_uses_last_dot() {
        assert_eq!(
            canonicalize_graph_qualified_name(Language::R, "as.data.frame.myclass"),
            "as.data.frame::myclass"
        );
    }

    #[test]
    fn test_canonicalize_graph_qualified_name_r_leading_dot_s3_generic() {
        assert_eq!(
            canonicalize_graph_qualified_name(Language::R, ".DollarNames.myclass"),
            ".DollarNames::myclass"
        );
    }

    #[test]
    fn test_display_graph_qualified_name_ruby_instance_method() {
        let display = display_graph_qualified_name(
            Language::Ruby,
            "Admin::Users::Controller::show",
            NodeKind::Method,
            false,
        );
        assert_eq!(display, "Admin::Users::Controller#show");
    }

    #[test]
    fn test_display_graph_qualified_name_ruby_singleton_method() {
        let display = display_graph_qualified_name(
            Language::Ruby,
            "Admin::Users::Controller::show",
            NodeKind::Method,
            true,
        );
        assert_eq!(display, "Admin::Users::Controller.show");
    }

    #[test]
    fn test_display_graph_qualified_name_ruby_member_variable() {
        let display = display_graph_qualified_name(
            Language::Ruby,
            "Admin::Users::Controller::username",
            NodeKind::Variable,
            false,
        );
        assert_eq!(display, "Admin::Users::Controller#username");
    }

    #[test]
    fn test_display_graph_qualified_name_ruby_instance_variable() {
        let display = display_graph_qualified_name(
            Language::Ruby,
            "Admin::Users::Controller::@current_user",
            NodeKind::Variable,
            false,
        );
        assert_eq!(display, "Admin::Users::Controller#@current_user");
    }

    #[test]
    fn test_display_graph_qualified_name_ruby_constant_stays_canonical() {
        let display = display_graph_qualified_name(
            Language::Ruby,
            "Admin::Users::Controller::DEFAULT_ROLE",
            NodeKind::Variable,
            false,
        );
        assert_eq!(display, "Admin::Users::Controller::DEFAULT_ROLE");
    }

    #[test]
    fn test_display_graph_qualified_name_ruby_class_variable_stays_canonical() {
        let display = display_graph_qualified_name(
            Language::Ruby,
            "Admin::Users::Controller::@@count",
            NodeKind::Variable,
            false,
        );
        assert_eq!(display, "Admin::Users::Controller::@@count");
    }

    #[test]
    fn test_display_graph_qualified_name_php_namespace_function() {
        let display = display_graph_qualified_name(
            Language::Php,
            "App::Services::send_mail",
            NodeKind::Function,
            false,
        );
        assert_eq!(display, "App\\Services\\send_mail");
    }

    #[test]
    fn test_display_graph_qualified_name_php_method() {
        let display = display_graph_qualified_name(
            Language::Php,
            "App::Services::Mailer::deliver",
            NodeKind::Method,
            false,
        );
        assert_eq!(display, "App\\Services\\Mailer::deliver");
    }

    #[test]
    fn test_display_graph_qualified_name_preserves_path_like_symbols() {
        let display = display_graph_qualified_name(
            Language::Go,
            "route::GET::/health",
            NodeKind::Endpoint,
            false,
        );
        assert_eq!(display, "route::GET::/health");
    }

    #[test]
    fn test_display_graph_qualified_name_preserves_ffi_symbols() {
        let display = display_graph_qualified_name(
            Language::Haskell,
            "ffi::C::sin",
            NodeKind::Function,
            false,
        );
        assert_eq!(display, "ffi::C::sin");
    }

    #[test]
    fn test_display_graph_qualified_name_preserves_native_cffi_symbols() {
        let display = display_graph_qualified_name(
            Language::Python,
            "native::cffi::calculate",
            NodeKind::Function,
            false,
        );
        assert_eq!(display, "native::cffi::calculate");
    }

    #[test]
    fn test_display_graph_qualified_name_preserves_native_php_ffi_symbols() {
        let display = display_graph_qualified_name(
            Language::Php,
            "native::ffi::crypto_encrypt",
            NodeKind::Function,
            false,
        );
        assert_eq!(display, "native::ffi::crypto_encrypt");
    }

    #[test]
    fn test_display_graph_qualified_name_preserves_native_panama_symbols() {
        let display = display_graph_qualified_name(
            Language::Java,
            "native::panama::nativeLinker",
            NodeKind::Function,
            false,
        );
        assert_eq!(display, "native::panama::nativeLinker");
    }

    #[test]
    fn test_canonicalize_graph_qualified_name_preserves_wasm_symbols() {
        assert_eq!(
            canonicalize_graph_qualified_name(Language::TypeScript, "wasm::module.wasm"),
            "wasm::module.wasm"
        );
    }

    #[test]
    fn test_canonicalize_graph_qualified_name_preserves_native_symbols() {
        assert_eq!(
            canonicalize_graph_qualified_name(Language::TypeScript, "native::binding.node"),
            "native::binding.node"
        );
    }

    #[test]
    fn test_display_graph_qualified_name_preserves_wasm_symbols() {
        let display = display_graph_qualified_name(
            Language::TypeScript,
            "wasm::module.wasm",
            NodeKind::Module,
            false,
        );
        assert_eq!(display, "wasm::module.wasm");
    }

    #[test]
    fn test_display_graph_qualified_name_preserves_native_symbols() {
        let display = display_graph_qualified_name(
            Language::TypeScript,
            "native::binding.node",
            NodeKind::Module,
            false,
        );
        assert_eq!(display, "native::binding.node");
    }

    #[test]
    fn test_canonicalize_graph_qualified_name_still_normalizes_dot_language_symbols() {
        assert_eq!(
            canonicalize_graph_qualified_name(Language::TypeScript, "Foo.bar"),
            "Foo::bar"
        );
    }
}
