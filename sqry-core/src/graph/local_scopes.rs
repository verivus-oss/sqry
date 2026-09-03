//! Shared local scope tracking and reference resolution infrastructure.
//!
//! This module provides a generic `ScopeTree<K>` that builds a per-file scope
//! tree, binds local declarations, and resolves identifier usages to declaration
//! nodes for `References` edges.
//!
//! Language plugins implement the [`ScopeKindTrait`] for their language-specific
//! `ScopeKind` enum and provide AST-specific `build_scopes_recursive()` and
//! `bind_declarations_recursive()` functions. The shared infrastructure handles:
//!
//! - Scope interval indexing with O(log n) lookup
//! - Variable binding with overlap detection
//! - Scope chain resolution with self-reference prevention
//! - String interning for memory efficiency
//! - Class member tracking and resolution (for OOP languages)
//! - Debug log rate limiting

use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::sync::Arc;

use log::debug;

use crate::graph::unified::node::NodeId;
use crate::graph::{GraphBuilderError, Position, Span};

/// Alias for scope indices within the tree.
pub type ScopeId = usize;

/// Maximum debug log messages per event category to avoid log flooding.
pub const MAX_DEBUG_LOGS_PER_EVENT: usize = 5;

// ============================================================================
// ScopeKindTrait — Language plugins implement this
// ============================================================================

/// Trait that language-specific `ScopeKind` enums must implement.
///
/// This trait allows the shared `ScopeTree` to query scope semantics
/// without knowing the language-specific scope variants.
pub trait ScopeKindTrait: Copy + Eq + Debug + Send + Sync {
    /// Returns `true` if this scope kind represents a class-like scope
    /// (anonymous class, local class, etc.) that acts as a boundary for
    /// variable capture and overlap detection.
    fn is_class_scope(&self) -> bool;

    /// Returns `true` if this scope kind is an overlap boundary — i.e.,
    /// duplicate variable names in enclosing scopes should be rejected
    /// at this boundary. Typically the same as `is_class_scope()`.
    fn is_overlap_boundary(&self) -> bool;

    /// Returns `true` if this class scope does NOT capture variables
    /// from enclosing scopes. For example, Java's `LocalRecord`,
    /// `LocalEnum`, and `LocalInterface` are non-capturing.
    ///
    /// Default: `false` (all class scopes capture by default).
    fn is_non_capturing_class_scope(&self) -> bool {
        false
    }

    /// Returns `true` if this language allows nested variable shadowing
    /// (same name in nested block scope). Kotlin allows this; Java does not.
    ///
    /// When `true`, `is_overlap()` only checks the current scope, not parents.
    /// Default: `false`.
    fn allows_nested_shadowing(&self) -> bool {
        false
    }

    /// Returns `true` if this scope kind should block capture chain traversal.
    ///
    /// When resolving identifiers past a class boundary, the capture chain
    /// walks outer scopes looking for captured variables. This method controls
    /// which scopes stop that traversal.
    ///
    /// Java overrides to return `is_non_capturing_class_scope()` only, because
    /// anonymous classes and local classes CAN capture effectively-final
    /// variables from enclosing scopes through multiple nesting levels.
    ///
    /// Default: `self.is_non_capturing_class_scope() || self.is_class_scope()`
    fn blocks_capture_chain(&self) -> bool {
        self.is_non_capturing_class_scope() || self.is_class_scope()
    }

    /// Whether unresolved base types should cause ambiguous member resolution.
    ///
    /// When a class has base types that cannot be resolved (e.g., from external
    /// libraries), this controls whether member lookups return `Ambiguous`
    /// (conservative) or `None` (allowing the capture chain to proceed).
    ///
    /// Java: `true` (strict — returns `Ambiguous` when bases are unresolved).
    /// Kotlin: `false` (lenient — returns `None` so capture chain continues).
    ///
    /// Default: `true` (conservative, matches Java behavior).
    fn strict_unresolved_bases(&self) -> bool {
        true
    }
}

// ============================================================================
// Core Data Structures
// ============================================================================

/// Resolves a byte offset in one file's content to a real line and column.
///
/// tree-sitter reports positions as a 0-based row plus a column counted in
/// BYTES from the start of that row. This reproduces that exactly, so a span
/// resolved from an offset here is indistinguishable from one taken straight
/// off a node.
///
/// It exists because bindings are recorded as byte offsets: by the time a
/// declaration node is turned into a graph node, the tree-sitter node that
/// produced it is long out of scope. Resolving the offset centrally is what
/// keeps every plugin from having to thread a span through its whole binder,
/// and from quietly reporting line 1 when it does not.
///
/// Deliberately NOT `Default`: an empty `line_starts` violates the invariant
/// below and makes [`LineIndex::position`] underflow. The only way to get one
/// is [`LineIndex::new`], which always seeds the first line.
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// Byte offset of the first byte of each line. Always begins with 0, so it
    /// is never empty and indexing by row is always in bounds.
    line_starts: Vec<usize>,
    /// Length of the indexed content, used to clamp out-of-range offsets.
    content_len: usize,
}

impl LineIndex {
    /// Build the index for a file's content. O(n) once per file.
    #[must_use]
    pub fn new(content: &[u8]) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(
            content
                .iter()
                .enumerate()
                .filter(|(_, byte)| **byte == b'\n')
                .map(|(offset, _)| offset + 1),
        );
        Self {
            line_starts,
            content_len: content.len(),
        }
    }

    /// 0-based row and byte column of `offset`, matching tree-sitter's `Point`.
    ///
    /// An offset past the end of the content is clamped to the end rather than
    /// panicking, so a malformed range degrades to a position instead of a
    /// crash.
    #[must_use]
    pub fn position(&self, offset: usize) -> Position {
        let clamped = offset.min(self.content_len);
        // partition_point counts the line starts at or before `clamped`, which
        // is the 1-based line number. Subtract one for the 0-based row.
        let row = self.line_starts.partition_point(|start| *start <= clamped) - 1;
        Position {
            line: row,
            column: clamped - self.line_starts[row],
        }
    }

    /// Byte offset of a 0-based row and byte column, the inverse of
    /// [`position`](Self::position).
    ///
    /// Returns `None` for a row past the end of the content. A column past the
    /// end of its line is clamped to the content length rather than running
    /// into the next line, so a stale position degrades instead of pointing
    /// somewhere plausible and wrong.
    #[must_use]
    pub fn byte_for_position(&self, line: u32, column: u32) -> Option<usize> {
        let row = usize::try_from(line).ok()?;
        let start = *self.line_starts.get(row)?;
        Some((start + usize::try_from(column).ok()?).min(self.content_len))
    }

    /// Span covering an entire file, for a node that genuinely IS the file:
    /// the `NodeKind::Module` a script-style plugin mints for the file itself.
    /// It replaces `Span::from_bytes(0, content.len())`, which put the file's
    /// byte length in the end column, and resolves to the same byte range, so a
    /// body hash computed either way is identical.
    ///
    /// NOT for a synthetic `<module>` caller context. Those are
    /// `NodeKind::Function` stubs standing in for code with no enclosing
    /// callable, and a file-sized span makes one pass `has_valid_body_span`, so
    /// a whole file is then hashed and shape-matched as if it were one function
    /// body. Three reviewers measured that regression on this branch. Those
    /// contexts keep `Span::default()`, which reports the honest line 1 column 0
    /// and stays out of the body planes.
    ///
    /// Walks the content once without building an index, because a caller
    /// needs one span rather than repeated lookups.
    #[must_use]
    pub fn whole_file_span(content: &[u8]) -> Span {
        let mut line = 0usize;
        let mut line_start = 0usize;
        for (offset, byte) in content.iter().enumerate() {
            if *byte == b'\n' {
                line += 1;
                line_start = offset + 1;
            }
        }
        Span::new(
            Position { line: 0, column: 0 },
            Position {
                line,
                column: content.len() - line_start,
            },
        )
    }

    /// Span covering `start..end`, in the same shape `Span::from_node` returns.
    #[must_use]
    pub fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.position(start), self.position(end))
    }
}

/// A local variable binding within a scope.
#[derive(Debug, Clone)]
pub struct VariableBinding {
    /// The graph node representing this binding (set after node creation).
    pub node_id: Option<NodeId>,
    /// Start byte of the identifier in the source.
    pub decl_start_byte: usize,
    /// End byte of the identifier in the source.
    pub decl_end_byte: usize,
    /// End byte of the full declarator (e.g., for `int x = 5`, ends after `5`).
    pub declarator_end_byte: usize,
    /// Start byte of the initializer expression (if present).
    /// Used for self-reference prevention: `let x = x + 1` should NOT
    /// resolve the RHS `x` to this same binding.
    pub initializer_start_byte: Option<usize>,
}

/// A scope within the scope tree, parameterized by a language-specific kind.
#[derive(Debug, Clone)]
pub struct Scope<K: ScopeKindTrait> {
    /// Byte offset where this scope begins in the source.
    pub start_byte: usize,
    /// Byte offset where this scope ends in the source.
    pub end_byte: usize,
    /// Parent scope index (None for root scopes).
    pub parent: Option<ScopeId>,
    /// Language-specific scope kind.
    pub kind: K,
    /// Nesting depth from root.
    pub depth: usize,
    /// Variable bindings indexed by interned name.
    pub variables: HashMap<Arc<str>, Vec<VariableBinding>>,
}

/// Interval for binary-search scope lookup.
#[derive(Debug, Clone)]
struct ScopeInterval {
    start: usize,
    end: usize,
    scope_id: ScopeId,
    depth: usize,
}

/// Sorted interval index for O(log n) innermost-scope queries.
#[derive(Debug, Clone, Default)]
struct ScopeIndex {
    intervals: Vec<ScopeInterval>,
}

/// String deduplication cache for variable names.
#[derive(Debug, Clone, Default)]
pub struct StringInterner {
    map: HashMap<String, Arc<str>>,
}

impl StringInterner {
    /// Intern a string, returning a shared `Arc<str>`.
    pub fn intern(&mut self, value: &str) -> Arc<str> {
        if let Some(existing) = self.map.get(value) {
            return existing.clone();
        }
        let arc: Arc<str> = Arc::from(value);
        self.map.insert(value.to_string(), arc.clone());
        arc
    }
}

// ============================================================================
// Class Member Tracking (for OOP languages)
// ============================================================================

/// Source of an inherited member (tracks which base class it came from).
#[derive(Debug, Clone)]
pub struct MemberSource {
    /// Qualified name of the base class that declares/inherits this member.
    pub qualifier: String,
}

/// Class member info associated with a scope.
#[derive(Debug, Clone)]
pub struct ClassMemberInfo {
    /// Qualified name of this class (e.g., `"com.example.MyClass"`).
    pub qualifier: Option<String>,
    /// Members declared directly in this class.
    pub declared_members: HashSet<Arc<str>>,
    /// Members inherited from base classes.
    pub inherited_members: HashMap<Arc<str>, Vec<MemberSource>>,
    /// Count of base types that couldn't be resolved.
    pub unresolved_base_count: usize,
    /// Count of explicitly declared base types.
    pub explicit_base_count: usize,
}

/// Maps scope IDs to class member information.
#[derive(Debug, Clone, Default)]
pub struct ClassMemberIndex {
    /// Per-scope class member information.
    pub by_scope: HashMap<ScopeId, ClassMemberInfo>,
}

/// A class's identity and declared members (for type name resolution).
#[derive(Debug, Clone)]
pub struct ClassInfo {
    /// Qualified name of this class.
    pub qualifier: String,
    /// Members declared directly in this class.
    pub declared_members: HashSet<Arc<str>>,
}

/// Index from class name keys to `ClassInfo` entries.
#[derive(Debug, Clone, Default)]
pub struct ClassInfoIndex {
    by_key: HashMap<String, Vec<usize>>,
    infos: Vec<ClassInfo>,
}

impl ClassInfoIndex {
    /// Insert a class info entry with one or more lookup keys.
    pub fn insert(&mut self, info: ClassInfo, keys: &[String]) {
        let idx = self.infos.len();
        self.infos.push(info);
        for key in keys {
            self.by_key.entry(key.clone()).or_default().push(idx);
        }
    }

    /// Resolve a class by key. Returns `Some` only if there is exactly one match.
    #[must_use]
    pub fn resolve(&self, key: &str) -> Option<&ClassInfo> {
        let candidates = self.by_key.get(key)?;
        if candidates.len() == 1 {
            self.infos.get(candidates[0])
        } else {
            None
        }
    }
}

/// Resolve a class info from the index, trying multiple key formats.
///
/// Attempts resolution with: the raw key, `::` replaced with `.`, and the
/// last segment (simple name) only.
#[must_use]
pub fn resolve_class_info<'a>(index: &'a ClassInfoIndex, base: &str) -> Option<&'a ClassInfo> {
    if let Some(info) = index.resolve(base) {
        return Some(info);
    }
    let dotted = base.replace("::", ".");
    if dotted != base
        && let Some(info) = index.resolve(&dotted)
    {
        return Some(info);
    }
    if let Some(last) = base.rsplit(['.', ':']).next()
        && let Some(info) = index.resolve(last)
    {
        return Some(info);
    }
    None
}

// ============================================================================
// Debug Log Limiter
// ============================================================================

/// Debug event categories for rate-limited logging.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum DebugEvent {
    /// Scope or binding span is invalid (start > end or beyond content length).
    InvalidSpan,
    /// Overlap conflict when adding a binding.
    OverlapConflict,
    /// Ambiguous inherited member resolution.
    InheritedAmbiguous,
    /// Could not find a binding to attach a `NodeId` to.
    MissingBindingNode,
}

/// Rate-limited debug logger to avoid flooding logs.
#[derive(Debug, Default)]
pub struct DebugLogLimiter {
    /// Per-event counters (public for test assertions in language plugins).
    pub counts: HashMap<DebugEvent, usize>,
}

impl DebugLogLimiter {
    /// Log a debug message, but only up to `MAX_DEBUG_LOGS_PER_EVENT` per category.
    pub fn log(&mut self, event: DebugEvent, message: &str) {
        let entry = self.counts.entry(event).or_insert(0);
        if *entry < MAX_DEBUG_LOGS_PER_EVENT {
            *entry += 1;
            debug!("{message}");
        }
    }
}

// ============================================================================
// Resolution Types
// ============================================================================

/// Outcome of resolving an identifier to a declaration.
#[derive(Debug)]
pub enum ResolutionOutcome {
    /// Resolved to a local variable binding.
    Local(LocalBindingMatch),
    /// Resolved to a class member (field, property, enum constant, etc.).
    Member {
        /// Qualified name of the member, if known.
        qualified_name: Option<String>,
    },
    /// Multiple conflicting resolutions found.
    Ambiguous,
    /// No matching declaration found.
    NoMatch,
}

/// A successful local variable resolution.
///
/// `#[non_exhaustive]`: adding `decl_span` to this struct was a major-version
/// break for anyone constructing it with a struct literal, which is what
/// `cargo-semver-checks` reports as `constructible_struct_adds_field`. Callers
/// only ever read a resolution, so the struct is marked non-exhaustive in the
/// same release that takes the break, and the next field costs a minor bump
/// instead of a major one.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct LocalBindingMatch {
    /// The graph node ID for the variable declaration (may be None if not yet created).
    pub node_id: Option<NodeId>,
    /// Start byte of the declaration identifier.
    pub decl_start_byte: usize,
    /// End byte of the declaration identifier.
    pub decl_end_byte: usize,
    /// Real line and column of the declaration, resolved from the byte offsets
    /// above. Hand this to the node builder: building a span from the offsets
    /// instead reports the declaration at line 1 with the offset as its column.
    pub decl_span: Span,
}

// ============================================================================
// ScopeTree<K> — The shared scope tree
// ============================================================================

/// Per-file scope tree with variable binding and resolution.
///
/// Generic over `K`, a language-specific `ScopeKind` enum that implements
/// [`ScopeKindTrait`].
#[derive(Debug)]
pub struct ScopeTree<K: ScopeKindTrait> {
    /// All scopes, indexed by `ScopeId`.
    pub scopes: Vec<Scope<K>>,
    /// Binary-search interval index.
    index: ScopeIndex,
    /// String deduplication cache.
    pub interner: StringInterner,
    /// Class member tracking (for OOP languages).
    pub class_members: ClassMemberIndex,
    /// Class info index (for type name resolution).
    pub class_infos: ClassInfoIndex,
    /// Rate-limited debug logger.
    pub debug: DebugLogLimiter,
    /// Total content length in bytes (for span validation).
    pub content_len: usize,
    /// Byte offset to line/column resolver for this file.
    line_index: LineIndex,
}

impl<K: ScopeKindTrait> ScopeTree<K> {
    /// Create a new empty scope tree over a file's content.
    ///
    /// Language plugins should populate the tree using `add_scope()`,
    /// `add_binding()`, and `rebuild_index()`.
    ///
    /// Takes the content rather than just its length so the tree can resolve a
    /// binding's byte offsets back to a real line and column. Passing the
    /// content is not optional for that reason: a tree built without it would
    /// report every declaration at line 1.
    #[must_use]
    pub fn new(content: &[u8]) -> Self {
        ScopeTree {
            scopes: Vec::new(),
            index: ScopeIndex::default(),
            interner: StringInterner::default(),
            class_members: ClassMemberIndex::default(),
            class_infos: ClassInfoIndex::default(),
            debug: DebugLogLimiter::default(),
            content_len: content.len(),
            line_index: LineIndex::new(content),
        }
    }

    /// Resolve a byte range in this file to a real line and column span.
    #[must_use]
    pub fn span_for_bytes(&self, start: usize, end: usize) -> Span {
        self.line_index.span(start, end)
    }

    // ========================================================================
    // Node ID attachment
    // ========================================================================

    /// Attach a graph `NodeId` to an existing variable binding.
    ///
    /// First tries exact match by name + byte position. Falls back to
    /// a fuzzy match (single unattached binding with the same name).
    /// Returns `true` if a binding was found and updated.
    pub fn attach_node_id(&mut self, name: &str, decl_start_byte: usize, node_id: NodeId) -> bool {
        // Phase 1: exact match on name + byte position
        for scope in &mut self.scopes {
            if let Some(bindings) = scope.variables.get_mut(name) {
                for binding in bindings {
                    if binding.decl_start_byte == decl_start_byte {
                        if binding.node_id.is_none() {
                            binding.node_id = Some(node_id);
                        }
                        return true;
                    }
                }
            }
        }

        // Phase 2: fallback — single unattached binding with same name
        let mut fallback: Option<(ScopeId, usize)> = None;
        let mut ambiguous = false;
        for (scope_id, scope) in self.scopes.iter().enumerate() {
            if let Some(bindings) = scope.variables.get(name) {
                for (idx, binding) in bindings.iter().enumerate() {
                    if binding.node_id.is_none() {
                        if fallback.is_none() {
                            fallback = Some((scope_id, idx));
                        } else {
                            ambiguous = true;
                            break;
                        }
                    }
                }
            }
            if ambiguous {
                break;
            }
        }

        if !ambiguous
            && let Some((scope_id, idx)) = fallback
            && let Some(bindings) = self.scopes[scope_id].variables.get_mut(name)
            && let Some(binding) = bindings.get_mut(idx)
        {
            binding.node_id = Some(node_id);
            return true;
        }

        self.debug.log(
            DebugEvent::MissingBindingNode,
            "Missing binding for node_id attach",
        );
        false
    }

    // ========================================================================
    // Resolution — with class boundary support
    // ========================================================================

    /// Resolve an identifier to a local variable, class member, or no-match.
    ///
    /// This is the primary entry point for resolution. It:
    /// 1. Finds the innermost scope at the usage byte position
    /// 2. Builds a scope chain to the root
    /// 3. Detects class boundaries and handles capture semantics
    /// 4. Falls back to class member resolution for OOP languages
    pub fn resolve_identifier(&mut self, usage_byte: usize, identifier: &str) -> ResolutionOutcome {
        let Some(innermost) = self.innermost_scope_at(usage_byte) else {
            return ResolutionOutcome::NoMatch;
        };
        let chain = self.scope_chain(innermost);

        let class_boundary = chain
            .iter()
            .position(|scope_id| self.scopes[*scope_id].kind.is_class_scope());

        if let Some(boundary_idx) = class_boundary {
            let current_class_scope = chain[boundary_idx];

            // Try local resolution before the class boundary
            if let Some(binding) =
                self.resolve_local_in_chain(identifier, usage_byte, &chain[..boundary_idx])
            {
                return ResolutionOutcome::Local(binding);
            }

            // Try class member resolution
            if let Some(member_resolution) =
                self.resolve_class_member(current_class_scope, identifier)
            {
                return member_resolution;
            }

            // Check if class scope blocks capture
            let class_kind = self.scopes[current_class_scope].kind;
            if class_kind.is_non_capturing_class_scope() {
                return ResolutionOutcome::NoMatch;
            }

            // Try capture chain (variables from enclosing non-class scopes)
            let mut capture_chain = Vec::new();
            for scope_id in &chain[boundary_idx + 1..] {
                if self.scopes[*scope_id].kind.blocks_capture_chain() {
                    break;
                }
                capture_chain.push(*scope_id);
            }
            if let Some(binding) =
                self.resolve_local_in_chain(identifier, usage_byte, &capture_chain)
            {
                return ResolutionOutcome::Local(binding);
            }

            return ResolutionOutcome::NoMatch;
        }

        // No class boundary — resolve in entire chain
        if let Some(binding) = self.resolve_local_in_chain(identifier, usage_byte, &chain) {
            return ResolutionOutcome::Local(binding);
        }

        ResolutionOutcome::NoMatch
    }

    /// Quick check if a name is a known type/class name.
    #[must_use]
    pub fn is_known_type_name(&self, name: &str) -> bool {
        self.class_infos.resolve(name).is_some()
    }

    /// Quick check if a local variable binding exists for `name` at the given byte position.
    #[must_use]
    pub fn has_local_binding(&self, name: &str, byte: usize) -> bool {
        let Some(innermost) = self.innermost_scope_at(byte) else {
            return false;
        };
        let chain = self.scope_chain(innermost);
        self.resolve_local_in_chain(name, byte, &chain).is_some()
    }

    // ========================================================================
    // Internal resolution helpers
    // ========================================================================

    /// Walk a scope chain looking for a binding matching `identifier` before `usage_byte`.
    #[must_use]
    pub fn resolve_local_in_chain(
        &self,
        identifier: &str,
        usage_byte: usize,
        chain: &[ScopeId],
    ) -> Option<LocalBindingMatch> {
        for scope_id in chain {
            if let Some(bindings) = self.scopes[*scope_id].variables.get(identifier)
                && let Some(binding) = find_applicable_binding(bindings, usage_byte)
            {
                return Some(LocalBindingMatch {
                    node_id: binding.node_id,
                    decl_start_byte: binding.decl_start_byte,
                    decl_end_byte: binding.decl_end_byte,
                    decl_span: self
                        .line_index
                        .span(binding.decl_start_byte, binding.decl_end_byte),
                });
            }
        }
        None
    }

    /// Resolve a class member by scope ID and identifier name.
    pub fn resolve_class_member(
        &mut self,
        scope_id: ScopeId,
        identifier: &str,
    ) -> Option<ResolutionOutcome> {
        let info = self.class_members.by_scope.get(&scope_id)?;
        if info.declared_members.contains(identifier) {
            let qualified_name = info
                .qualifier
                .as_ref()
                .map(|qual| format!("{qual}::{identifier}"));
            return Some(ResolutionOutcome::Member { qualified_name });
        }

        if let Some(sources) = info.inherited_members.get(identifier) {
            if sources.len() == 1 {
                let qualified_name = Some(format!("{}::{}", sources[0].qualifier, identifier));
                return Some(ResolutionOutcome::Member { qualified_name });
            }
            if sources.len() > 1 {
                self.debug.log(
                    DebugEvent::InheritedAmbiguous,
                    "Inherited member ambiguity: multiple matches",
                );
                return Some(ResolutionOutcome::Ambiguous);
            }
        }

        if info.explicit_base_count > 0
            && info.unresolved_base_count > 0
            && self.scopes[scope_id].kind.strict_unresolved_bases()
        {
            self.debug.log(
                DebugEvent::InheritedAmbiguous,
                "Inherited member ambiguity: unresolved base",
            );
            return Some(ResolutionOutcome::Ambiguous);
        }

        None
    }

    // ========================================================================
    // Scope lookup
    // ========================================================================

    /// Find the innermost (deepest) scope containing the given byte position.
    ///
    /// Uses a sorted interval index with binary search + reverse scan for
    /// O(log n) lookup.
    #[must_use]
    pub fn innermost_scope_at(&self, byte: usize) -> Option<ScopeId> {
        let intervals = &self.index.intervals;
        if intervals.is_empty() {
            return None;
        }

        // partition_point returns the first index where start > byte,
        // so all candidates with start <= byte are in [0..idx).
        let idx = intervals.partition_point(|iv| iv.start <= byte);
        if idx == 0 {
            return None;
        }

        // Reverse scan: for properly nested intervals sorted by
        // (start ASC, depth ASC), the first containing interval is the
        // deepest. At the same start position, deeper scopes come later
        // in the array and are seen first during reverse iteration.
        for iv in intervals[..idx].iter().rev() {
            if byte < iv.end {
                return Some(iv.scope_id);
            }
        }

        None
    }

    /// Build the parent chain from a scope to the root.
    #[must_use]
    pub fn scope_chain(&self, innermost: ScopeId) -> Vec<ScopeId> {
        let mut chain = Vec::new();
        let mut current = Some(innermost);
        while let Some(scope_id) = current {
            chain.push(scope_id);
            current = self.scopes[scope_id].parent;
        }
        chain
    }

    // ========================================================================
    // Scope and binding construction
    // ========================================================================

    /// Add a new scope to the tree.
    ///
    /// Returns `None` if the span is invalid (start > end or beyond content).
    pub fn add_scope(
        &mut self,
        kind: K,
        start_byte: usize,
        end_byte: usize,
        parent: Option<ScopeId>,
    ) -> Option<ScopeId> {
        if !is_valid_span(start_byte, end_byte, self.content_len) {
            self.debug
                .log(DebugEvent::InvalidSpan, "Invalid scope span");
            return None;
        }
        let depth = parent.map_or(0, |p| self.scopes[p].depth + 1);
        let scope_id = self.scopes.len();
        self.scopes.push(Scope {
            start_byte,
            end_byte,
            parent,
            kind,
            depth,
            variables: HashMap::new(),
        });
        Some(scope_id)
    }

    /// Rebuild the interval index from the current scopes.
    ///
    /// Must be called after adding scopes and before performing lookups.
    pub fn rebuild_index(&mut self) {
        self.index.intervals = self
            .scopes
            .iter()
            .enumerate()
            .map(|(scope_id, scope)| ScopeInterval {
                start: scope.start_byte,
                end: scope.end_byte,
                scope_id,
                depth: scope.depth,
            })
            .collect();
        // Sort by (start ASC, depth ASC) so the deepest scope at each start
        // position comes last — and is found first during reverse scan.
        self.index
            .intervals
            .sort_by(|a, b| a.start.cmp(&b.start).then(a.depth.cmp(&b.depth)));
    }

    /// Add a variable binding to a scope.
    ///
    /// Performs overlap detection before adding. If the binding would
    /// overlap with an existing binding in the same or enclosing scope
    /// (depending on language rules), it is silently rejected.
    pub fn add_binding(
        &mut self,
        scope_id: ScopeId,
        name: &str,
        decl_start_byte: usize,
        decl_end_byte: usize,
        declarator_end_byte: usize,
        initializer_start_byte: Option<usize>,
    ) {
        if !is_valid_span(decl_start_byte, decl_end_byte, self.content_len)
            || !is_valid_span(decl_end_byte, declarator_end_byte, self.content_len)
        {
            self.debug
                .log(DebugEvent::InvalidSpan, "Invalid binding span");
            return;
        }

        if self.is_overlap(scope_id, name, decl_start_byte) {
            self.debug
                .log(DebugEvent::OverlapConflict, "Overlap conflict for binding");
            return;
        }

        let key = self.interner.intern(name);
        let binding = VariableBinding {
            node_id: None,
            decl_start_byte,
            decl_end_byte,
            declarator_end_byte,
            initializer_start_byte,
        };

        let entry = self
            .scopes
            .get_mut(scope_id)
            .map(|scope| scope.variables.entry(key).or_default());
        if let Some(bindings) = entry {
            bindings.push(binding);
            bindings.sort_by_key(|b| b.decl_start_byte);
        }
    }

    /// Check if adding a binding in `scope_id` with `name` would overlap.
    ///
    /// Language-specific behavior:
    /// - If `allows_nested_shadowing()` is true (e.g., Kotlin): only checks
    ///   the current scope for duplicate names.
    /// - Otherwise (e.g., Java): walks parent scopes up to an overlap boundary.
    fn is_overlap(&self, scope_id: ScopeId, name: &str, decl_start: usize) -> bool {
        // Check if the language allows nested shadowing
        if let Some(scope) = self.scopes.get(scope_id)
            && scope.kind.allows_nested_shadowing()
        {
            // Only check the same scope for duplicates
            return scope
                .variables
                .get(name)
                .is_some_and(|bindings| !bindings.is_empty());
        }

        // Walk up parent scopes checking for overlapping bindings
        let mut current = Some(scope_id);
        while let Some(scope) = current {
            if let Some(bindings) = self.scopes[scope].variables.get(name)
                && !bindings.is_empty()
                && decl_start <= self.scopes[scope].end_byte
            {
                return true;
            }

            if self.scopes[scope].kind.is_overlap_boundary() {
                break;
            }

            current = self.scopes[scope].parent;
        }
        false
    }
}

// ============================================================================
// Free functions — language-agnostic utilities
// ============================================================================

/// Find the most recent applicable binding before `usage_byte`.
///
/// A binding is applicable if:
/// 1. Its declaration comes before the usage (`decl_start_byte <= usage_byte`)
/// 2. The usage is NOT within the binding's own initializer (self-reference prevention)
///
/// Returns the binding with the largest `decl_start_byte` (most recent).
#[must_use]
pub fn find_applicable_binding(
    bindings: &[VariableBinding],
    usage_byte: usize,
) -> Option<&VariableBinding> {
    bindings
        .iter()
        .filter(|binding| binding.decl_start_byte <= usage_byte)
        .filter(|binding| {
            // Self-reference prevention: skip if usage is within the
            // binding's own initializer range [self_start, declarator_end).
            let self_start = binding
                .initializer_start_byte
                .unwrap_or(binding.decl_end_byte);
            !(self_start <= usage_byte && usage_byte < binding.declarator_end_byte)
        })
        .max_by_key(|binding| binding.decl_start_byte)
}

/// Validate a byte span against content bounds.
#[must_use]
pub fn is_valid_span(start: usize, end: usize, content_len: usize) -> bool {
    start <= end && end <= content_len
}

/// Create a `RecursionGuard` from the configured recursion limits.
///
/// # Panics
///
/// Panics if recursion limits cannot be loaded or the guard cannot be created.
#[must_use]
pub fn load_recursion_guard() -> crate::query::security::RecursionGuard {
    let recursion_limits =
        crate::config::RecursionLimits::load_or_default().expect("Failed to load recursion limits");
    let file_ops_depth = recursion_limits
        .effective_file_ops_depth()
        .expect("Invalid file_ops_depth configuration");
    crate::query::security::RecursionGuard::new(file_ops_depth)
        .expect("Failed to create recursion guard")
}

/// Find the first child of a tree-sitter node with the given kind.
#[must_use]
pub fn first_child_of_kind<'a>(
    node: tree_sitter::Node<'a>,
    kind: &str,
) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find(|child| child.kind() == kind)
}

/// Map a `RecursionError` to a `GraphBuilderError::ParseError`.
#[must_use]
pub fn recursion_error_to_graph_error(
    e: &crate::query::security::RecursionError,
    node: tree_sitter::Node,
) -> GraphBuilderError {
    GraphBuilderError::ParseError {
        // `Span::from_node`, not `from_bytes`. This span reaches a user:
        // `GraphBuilderError::ParseError` renders as
        // "Failed to parse AST node at {span:?}", so the byte-offset encoding
        // printed line 0 with the offset in the column. A reviewer pointed out
        // that this function already receives the node, and that "not a symbol
        // position" is not a licence to report the wrong line. This was issue
        // #725 surviving on the error path.
        span: Span::from_node(&node),
        reason: format!("Recursion limit: {e}"),
    }
}

// ============================================================================
// Test Helpers (available to all plugins via sqry-core)
// ============================================================================

/// Collect all `References` edges as `(source_name, target_name)` pairs.
///
/// This helper is for plugin test code that verifies local variable
/// reference tracking. It uses the `StagingGraph`'s operations and
/// string lookup to produce human-readable edge pairs.
#[must_use]
/// `References` edges as (source name, target name, target start position).
///
/// The position is what identifies WHICH declaration a reference resolved to.
/// Callers used to parse a `name@<decl_start_byte>` suffix off the target name
/// for that, which worked only while the builder published its binding-site
/// cache key as the node address. It no longer does, because publishing it put
/// that key into planner, MCP and LSP output. The recorded declaration span
/// carries the same information and is the thing the graph actually promises.
pub fn collect_reference_edges_with_target_position(
    staging: &crate::graph::unified::build::StagingGraph,
) -> Vec<(String, String, (u32, u32))> {
    use crate::graph::unified::build::StagingOp;
    use crate::graph::unified::edge::EdgeKind;

    let strings = crate::graph::unified::build::test_helpers::build_string_lookup(staging);
    let node_names = build_node_name_map(staging, &strings);

    let mut positions: HashMap<NodeId, (u32, u32)> = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, expected_id } = op
            && let Some(id) = *expected_id
        {
            // `NodeEntry` stores 1-based lines (staging adds one), while
            // `LineIndex` speaks tree-sitter's 0-based rows. Convert here so
            // the caller gets one coordinate system, not two.
            positions.insert(id, (entry.start_line.saturating_sub(1), entry.start_column));
        }
    }

    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::References,
                ..
            } = op
            {
                let from = node_names.get(source)?.clone();
                let to = node_names.get(target)?.clone();
                let at = *positions.get(target)?;
                Some((from, to, at))
            } else {
                None
            }
        })
        .collect()
}

/// `References` edges as (source name, target name).
///
/// Use [`collect_reference_edges_with_target_position`] when the caller needs
/// to know WHICH declaration a reference resolved to; a name alone no longer
/// distinguishes two same-named bindings in different scopes.
pub fn collect_reference_edges(
    staging: &crate::graph::unified::build::StagingGraph,
) -> Vec<(String, String)> {
    use crate::graph::unified::build::StagingOp;
    use crate::graph::unified::edge::EdgeKind;

    let strings = crate::graph::unified::build::test_helpers::build_string_lookup(staging);
    let node_names = build_node_name_map(staging, &strings);

    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::References,
                ..
            } = op
            {
                let from = node_names.get(source)?.clone();
                let to = node_names.get(target)?.clone();
                Some((from, to))
            } else {
                None
            }
        })
        .collect()
}

/// Check if any `References` edge targets a variable with the given name
/// (pattern: `name@*`).
#[must_use]
/// `References` edges that target a VARIABLE declaration named `name`.
///
/// Kind-aware on purpose. A plugin that publishes a declaration under the bare
/// identifier cannot be matched by name alone, because a `Type` node can carry
/// the same string: `def f(x: int)` emits a reference to a type named `int`,
/// and a bare-name match would read that as a local variable called `int`.
///
/// Accepts both live declaration spellings: the identifier alone, and the
/// binding-site suffixed form `name@<digits>` that a plugin uses when it has
/// not separated node identity from the published name.
fn variable_ref_targets(staging: &crate::graph::unified::build::StagingGraph, name: &str) -> usize {
    use crate::graph::unified::build::StagingOp;
    use crate::graph::unified::edge::EdgeKind;
    use crate::graph::unified::node::kind::NodeKind;

    let strings = crate::graph::unified::build::test_helpers::build_string_lookup(staging);
    let mut variables: HashMap<NodeId, String> = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, expected_id } = op
            && entry.kind == NodeKind::Variable
            && let Some(id) = *expected_id
            && let Some(n) = strings.get(&entry.qualified_name.unwrap_or(entry.name).index())
        {
            variables.insert(id, n.clone());
        }
    }

    staging
        .operations()
        .iter()
        .filter(|op| {
            let StagingOp::AddEdge {
                target,
                kind: EdgeKind::References,
                ..
            } = op
            else {
                return false;
            };
            variables.get(target).is_some_and(|t| {
                t == name
                    || t.strip_prefix(name)
                        .and_then(|r| r.strip_prefix('@'))
                        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
            })
        })
        .count()
}

/// Whether any `References` edge targets a variable declaration named `name`.
#[must_use]
pub fn has_local_variable_ref(
    staging: &crate::graph::unified::build::StagingGraph,
    name: &str,
) -> bool {
    variable_ref_targets(staging, name) > 0
}

/// How many `References` edges target a variable declaration named `name`.
#[must_use]
pub fn count_local_variable_refs(
    staging: &crate::graph::unified::build::StagingGraph,
    name: &str,
) -> usize {
    variable_ref_targets(staging, name)
}

/// Whether any `References` edge targets a node named `name@<offset>`.
///
/// Name-only and therefore kind-blind. Correct for a plugin that publishes its
/// declarations under the suffixed spelling, where no other node kind shares
/// it. Use [`has_local_variable_ref`] when declarations carry the bare
/// identifier, since a `Type` node can then collide with them.
#[must_use]
pub fn has_local_ref(edges: &[(String, String)], name: &str) -> bool {
    let prefix = format!("{name}@");
    edges.iter().any(|(_, target)| target.starts_with(&prefix))
}

/// Count `References` edges that target a variable with the given name.
#[must_use]
pub fn count_local_refs(edges: &[(String, String)], name: &str) -> usize {
    let prefix = format!("{name}@");
    edges
        .iter()
        .filter(|(_, target)| target.starts_with(&prefix))
        .count()
}

/// Get the unique set of target names matching `name@*`.
#[must_use]
pub fn local_ref_targets(edges: &[(String, String)], name: &str) -> HashSet<String> {
    let prefix = format!("{name}@");
    edges
        .iter()
        .filter(|(_, target)| target.starts_with(&prefix))
        .map(|(_, target)| target.clone())
        .collect()
}

/// Internal: build a `NodeId` → name lookup map from staging operations.
fn build_node_name_map(
    staging: &crate::graph::unified::build::StagingGraph,
    strings: &HashMap<u32, String>,
) -> HashMap<NodeId, String> {
    use crate::graph::unified::build::StagingOp;

    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode {
                entry, expected_id, ..
            } = op
            {
                let node_id = (*expected_id)?;
                let name_idx = entry.qualified_name.unwrap_or(entry.name).index();
                let name = strings.get(&name_idx)?.clone();
                Some((node_id, name))
            } else {
                None
            }
        })
        .collect()
}

/// Build a `StringId.index() → String` lookup from staging operations.
///
/// Re-exported from `test_helpers` for convenience.
#[must_use]
pub fn build_string_lookup(
    staging: &crate::graph::unified::build::StagingGraph,
) -> HashMap<u32, String> {
    crate::graph::unified::build::test_helpers::build_string_lookup(staging)
}

#[cfg(test)]
mod line_index_tests {
    use super::LineIndex;

    /// Every offset in the content must resolve to the same row and column
    /// tree-sitter would report: row counted in newlines, column counted in
    /// BYTES from the start of the row.
    #[test]
    fn position_matches_newline_and_byte_column_arithmetic() {
        let content = b"alpha\nbeta\n\ngamma";
        let index = LineIndex::new(content);

        let mut row = 0;
        let mut line_start = 0;
        for offset in 0..=content.len() {
            let position = index.position(offset);
            assert_eq!(
                (position.line, position.column),
                (row, offset - line_start),
                "offset {offset} resolved wrongly"
            );
            if content.get(offset) == Some(&b'\n') {
                row += 1;
                line_start = offset + 1;
            }
        }
    }

    /// A file with no trailing newline still resolves its last byte, and an
    /// empty file resolves offset 0 rather than panicking on an empty index.
    #[test]
    fn handles_no_trailing_newline_and_empty_content() {
        let index = LineIndex::new(b"one\ntwo");
        assert_eq!((index.position(6).line, index.position(6).column), (1, 2));

        let empty = LineIndex::new(b"");
        assert_eq!((empty.position(0).line, empty.position(0).column), (0, 0));
    }

    /// Content ending in a newline has a final, EMPTY line, and the offset at
    /// `content.len()` belongs to it. Dropping that last line start still
    /// satisfies every other test here, so this is the one that pins it.
    #[test]
    fn offset_at_end_of_newline_terminated_content_is_on_the_final_empty_line() {
        let content = b"one\ntwo\n";
        let index = LineIndex::new(content);
        let at_end = index.position(content.len());
        assert_eq!(
            (at_end.line, at_end.column),
            (2, 0),
            "offset {} follows the second newline, so it opens line index 2",
            content.len()
        );

        // A single newline is the degenerate form of the same case.
        let just_a_newline = LineIndex::new(b"\n");
        let after = just_a_newline.position(1);
        assert_eq!((after.line, after.column), (1, 0));
    }

    /// An offset past the end clamps to the end instead of panicking, matching
    /// the behaviour of the per-plugin helper this replaced.
    #[test]
    fn clamps_offsets_past_the_end() {
        let content = b"one\ntwo";
        let index = LineIndex::new(content);
        let clamped = index.position(usize::MAX);
        assert_eq!((clamped.line, clamped.column), (1, 3));
        assert_eq!(clamped, index.position(content.len()));
    }

    /// A CRLF file keeps the carriage return on the preceding line, so the
    /// column of the first byte after a break is 0, as tree-sitter reports it.
    #[test]
    fn carriage_returns_stay_on_the_preceding_line() {
        let index = LineIndex::new(b"one\r\ntwo");
        let after_break = index.position(5);
        assert_eq!((after_break.line, after_break.column), (1, 0));
    }

    /// The whole point: a span built from byte offsets reports the real line,
    /// where `Span::from_bytes` would have reported line 0 with the offset
    /// sitting in the column.
    #[test]
    fn span_reports_real_lines_not_byte_offsets() {
        let content = b"fn a() {}\nfn b() {}\nlet value = 1;";
        let index = LineIndex::new(content);
        let start = 20; // `let` on the third line
        let span = index.span(start, start + 3);

        assert_eq!(span.start.line, 2, "third line is row 2");
        assert_eq!(span.start.column, 0);
        assert_eq!(span.end.line, 2);
        assert_eq!(span.end.column, 3);
    }
}
