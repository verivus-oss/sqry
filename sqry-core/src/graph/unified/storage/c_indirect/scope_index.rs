//! Per-file local scope index — storage shape + lookup (DESIGN §4.1).
//!
//! This submodule hosts the *data shape* of the C block-scope arena that
//! Phase A's indirect-call resolver consumes. The tree-sitter Builder that
//! populates the arena from C source code lives in `sqry-lang-c`
//! (`relations::scope_index`) — `sqry-core` cannot depend on a specific
//! language plugin, so the builder constructs this type via
//! [`LocalScopeIndex::from_parts`] and the C plugin re-exports the type.
//!
//! # Correctness invariant
//!
//! The lookup walks the scope chain innermost-out and only considers
//! declarations whose `decl_span.0` lexically precedes the use-site
//! offset. This closes the codex U08 iter-1 gap where a *later-lexically*
//! shadowing declaration in the same block could incorrectly capture an
//! earlier use of the same name. See [`LocalScopeIndex::resolve_type`].
//!
//! # Serde shape
//!
//! All three types derive `Serialize + Deserialize + PartialEq + Eq +
//! Clone + Debug` so a populated [`super::CIndirectSideTables`] can
//! deep-roundtrip through postcard inside the V11 snapshot envelope.

use serde::{Deserialize, Serialize};

/// Per-file side table mapping local identifiers to their declared type
/// tokens, scoped by C block-scope rules.
///
/// See module docs for the correctness invariant and the construction
/// algorithm. Note that the construction (tree-sitter walk) lives in
/// `sqry-lang-c::relations::scope_index`; this type only stores the
/// already-built arena and exposes the lookup that the indirect-call
/// resolver depends on.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalScopeIndex {
    /// Block-scope arena. Entries are stored in pre-order; a scope's
    /// parent is always at a lower index than the scope itself. Index 0
    /// is the outermost scope encountered for the file (typically the
    /// translation-unit scope).
    scopes: Vec<ScopeEntry>,
    /// Declarations indexed by scope.
    /// Invariant: `decls_by_scope.len() == scopes.len()`.
    decls_by_scope: Vec<Vec<LocalDeclaration>>,
}

impl LocalScopeIndex {
    /// Construct a [`LocalScopeIndex`] from pre-built scope and
    /// declaration arenas.
    ///
    /// This is the bridge between `sqry-lang-c`'s tree-sitter Builder and
    /// `sqry-core`'s side-table storage. The C plugin walks its tree,
    /// produces parallel `Vec`s, then assembles them via this
    /// constructor.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `scopes.len() != decls_by_scope.len()`,
    /// which would violate the per-scope declaration-index invariant.
    /// Release builds tolerate the mismatch but [`Self::resolve_type`]
    /// will silently miss for scopes outside the shorter `Vec`'s range.
    #[must_use]
    pub fn from_parts(scopes: Vec<ScopeEntry>, decls_by_scope: Vec<Vec<LocalDeclaration>>) -> Self {
        debug_assert_eq!(
            scopes.len(),
            decls_by_scope.len(),
            "LocalScopeIndex: scopes.len() ({}) must equal decls_by_scope.len() ({})",
            scopes.len(),
            decls_by_scope.len(),
        );
        Self {
            scopes,
            decls_by_scope,
        }
    }

    /// Number of scopes in the arena.
    ///
    /// Primarily for unit-test assertions; the resolver does not consult
    /// this directly.
    #[inline]
    #[must_use]
    pub fn scope_count(&self) -> usize {
        self.scopes.len()
    }

    /// Number of declarations across all scopes.
    #[must_use]
    pub fn declaration_count(&self) -> usize {
        self.decls_by_scope.iter().map(Vec::len).sum()
    }

    /// Resolve `name` at byte offset `use_site_offset` to its declared
    /// type token, if any.
    ///
    /// The walk runs innermost-out:
    ///
    /// 1. Find the innermost [`ScopeEntry`] whose `span` contains
    ///    `use_site_offset`.
    /// 2. Scan `decls_by_scope[scope_index]` for a declaration matching
    ///    `name` whose `decl_span.0 <= use_site_offset` (the declaration
    ///    must lexically precede the use site). If multiple matches exist
    ///    within the same scope, the one with the largest `decl_span.0`
    ///    wins (latest-preceding-declaration).
    /// 3. If no match is found, recurse into the parent scope.
    /// 4. Return `None` if no enclosing scope contains a matching
    ///    declaration.
    #[must_use]
    pub fn resolve_type(&self, name: &str, use_site_offset: usize) -> Option<&str> {
        let innermost = self.innermost_scope_containing(use_site_offset)?;
        let mut cursor = Some(innermost);
        while let Some(scope_idx) = cursor {
            // Latest-preceding-declaration: pick the candidate with the
            // largest `decl_span.0` that still satisfies the constraint.
            let mut best: Option<&LocalDeclaration> = None;
            // Defensive bounds check: in release builds with a malformed
            // arena (mismatched lengths) we silently miss rather than
            // panic.
            if scope_idx >= self.decls_by_scope.len() {
                break;
            }
            for decl in &self.decls_by_scope[scope_idx] {
                if decl.name == name && decl.decl_span.0 <= use_site_offset {
                    match best {
                        None => best = Some(decl),
                        Some(prev) if decl.decl_span.0 > prev.decl_span.0 => best = Some(decl),
                        _ => {}
                    }
                }
            }
            if let Some(d) = best {
                return Some(d.type_token.as_str());
            }
            cursor = self.scopes[scope_idx].parent;
        }
        None
    }

    /// Find the innermost (deepest) scope whose `span` contains the given
    /// byte offset. Scopes are stored in pre-order so any later-indexed
    /// scope that also contains the offset is necessarily nested deeper.
    fn innermost_scope_containing(&self, offset: usize) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (idx, scope) in self.scopes.iter().enumerate() {
            if scope.span.0 <= offset && offset < scope.span.1 {
                best = Some(idx);
            }
        }
        best
    }
}

/// One scope-introducing region in the AST.
///
/// Public surface is intentionally narrow — only the constructor and the
/// fields the builder needs to populate are exposed. Fields stay private
/// so that the storage shape can evolve (e.g. add per-scope kind tag)
/// without breaking external producers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeEntry {
    /// Byte range of the scope's source extent — `(start_byte, end_byte)`.
    pub(crate) span: (usize, usize),
    /// Parent scope index in [`LocalScopeIndex::scopes`]. `None` for the
    /// outermost scope.
    pub(crate) parent: Option<usize>,
}

impl ScopeEntry {
    /// Construct a new [`ScopeEntry`] for the builder.
    #[inline]
    #[must_use]
    pub fn new(span: (usize, usize), parent: Option<usize>) -> Self {
        Self { span, parent }
    }

    /// Byte range of the scope.
    #[inline]
    #[must_use]
    pub fn span(&self) -> (usize, usize) {
        self.span
    }

    /// Parent-scope index, if any.
    #[inline]
    #[must_use]
    pub fn parent(&self) -> Option<usize> {
        self.parent
    }
}

/// One local declaration bound to a single scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalDeclaration {
    /// Identifier name as it appears in source.
    pub(crate) name: String,
    /// Source-level type token (not width-aliased, not typedef-resolved).
    pub(crate) type_token: String,
    /// Byte range of the declaration itself — `(start_byte, end_byte)`.
    pub(crate) decl_span: (usize, usize),
    /// Owning [`ScopeEntry`] index.
    pub(crate) scope_index: usize,
}

impl LocalDeclaration {
    /// Construct a new [`LocalDeclaration`] for the builder.
    #[inline]
    #[must_use]
    pub fn new(
        name: String,
        type_token: String,
        decl_span: (usize, usize),
        scope_index: usize,
    ) -> Self {
        Self {
            name,
            type_token,
            decl_span,
            scope_index,
        }
    }

    /// Identifier name.
    #[inline]
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Source-level type token.
    #[inline]
    #[must_use]
    pub fn type_token(&self) -> &str {
        &self.type_token
    }

    /// Byte range of the declaration.
    #[inline]
    #[must_use]
    pub fn decl_span(&self) -> (usize, usize) {
        self.decl_span
    }

    /// Owning scope index.
    #[inline]
    #[must_use]
    pub fn scope_index(&self) -> usize {
        self.scope_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `from_parts` accepts the parallel `Vec`s and `resolve_type` walks
    /// the parent chain innermost-out, returning the innermost matching
    /// declaration. Mirrors DESIGN §4.1's latest-preceding-declaration
    /// semantics on a hand-built arena (no tree-sitter dependency here).
    #[test]
    fn from_parts_and_innermost_resolution() {
        let scopes = vec![
            ScopeEntry::new((0, 100), None),    // outer (file-level)
            ScopeEntry::new((20, 60), Some(0)), // inner (nested in outer)
        ];
        let decls = vec![
            vec![
                LocalDeclaration::new("x".into(), "int".into(), (5, 10), 0),
                LocalDeclaration::new("y".into(), "long".into(), (15, 25), 0),
            ],
            vec![LocalDeclaration::new(
                "x".into(),
                "float".into(),
                (25, 35),
                1,
            )],
        ];
        let idx = LocalScopeIndex::from_parts(scopes, decls);
        assert_eq!(idx.scope_count(), 2);
        assert_eq!(idx.declaration_count(), 3);
        // Use site inside inner scope, AFTER the inner `float x`: should
        // resolve to `float` (innermost shadow).
        assert_eq!(idx.resolve_type("x", 40), Some("float"));
        // Use site inside inner scope BEFORE the inner declaration (offset
        // 22, which is < 25): falls through to outer `int x` because the
        // inner declaration does not yet lexically precede the use.
        assert_eq!(idx.resolve_type("x", 22), Some("int"));
        // `y` is only declared at the outer scope; the inner walk falls
        // through to the parent.
        assert_eq!(idx.resolve_type("y", 40), Some("long"));
        // Outside any scope: None.
        assert_eq!(idx.resolve_type("x", 999), None);
    }

    /// Roundtrip the storage shape via postcard so the V11 snapshot wire
    /// path can rely on deep-equality preservation.
    #[test]
    fn scope_index_postcard_roundtrip() {
        let idx = LocalScopeIndex::from_parts(
            vec![ScopeEntry::new((0, 100), None)],
            vec![vec![LocalDeclaration::new(
                "fp".into(),
                "void (*)(int)".into(),
                (5, 30),
                0,
            )]],
        );
        let bytes = postcard::to_stdvec(&idx).expect("serialize");
        let decoded: LocalScopeIndex = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(decoded, idx);
        assert_eq!(decoded.resolve_type("fp", 50), Some("void (*)(int)"));
    }

    /// The accessors expose the constructor-provided values verbatim.
    #[test]
    fn accessors_expose_constructor_values() {
        let entry = ScopeEntry::new((10, 20), Some(5));
        assert_eq!(entry.span(), (10, 20));
        assert_eq!(entry.parent(), Some(5));

        let decl = LocalDeclaration::new("v".into(), "int".into(), (3, 8), 2);
        assert_eq!(decl.name(), "v");
        assert_eq!(decl.type_token(), "int");
        assert_eq!(decl.decl_span(), (3, 8));
        assert_eq!(decl.scope_index(), 2);
    }
}
