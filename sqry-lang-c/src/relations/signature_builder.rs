//! Canonical type-signature builder for C function/function-pointer declarators.
//!
//! Realises **IMP:c-icall-precision-007** / **U07_SIGNATURE_BUILDER** of
//! the Phase A indirect-call-precision DAG. Implements the declarator
//! walker described in DESIGN §3.3 plus base-token normalisation via the
//! width-alias table from U01 ([`normalize_width_alias`]) and typedef
//! chasing per DESIGN §3.3.
//!
//! ## Public API
//!
//! - [`build_function_signature`] — given the tree-sitter node of a
//!   function definition / function declarator (or its abstract variant
//!   used inside function-pointer types), produces the canonical
//!   signature string `"<return>(<param1>,<param2>,...)"`. Returns
//!   `None` when the input is malformed.
//! - [`build_local_type_token`] — given a parameter / variable
//!   declaration node, produces the canonical type token for that local
//!   (e.g. `"int*"`, `"struct file_operations*"`, or a function-pointer
//!   signature). Returns `None` when the input is malformed.
//!
//! ## Algorithm
//!
//! The walker recurses through the declarator chain
//! ([`pointer_declarator`], [`array_declarator`], [`function_declarator`]
//! and their `abstract_*` siblings, plus the parenthesised forms) and
//! builds a structured [`Declarator`] value:
//!
//! 1. **Pass 1** — locate the function_declarator (the node owning the
//!    `parameters` field).
//! 2. **Pass 2** — walk *outwards* from the function declarator to count
//!    pointer levels and detect array decay.
//! 3. **Base-token recovery** — bare type specifiers
//!    (`primitive_type`, `type_identifier`, `struct_specifier`,
//!    `union_specifier`, `enum_specifier`, `sized_type_specifier`) are
//!    fed to the existing [`extract_all_type_names_from_c_type`]
//!    function (in [`super::type_extractor`]). That extractor is the
//!    canonical base-token recovery primitive (DESIGN §3.2.5) — this
//!    module never modifies it.
//! 4. **Normalisation** — base tokens chase typedefs via
//!    [`TypedefChain::resolve`] (depth-bounded at 8), then run through
//!    [`normalize_width_alias`].
//! 5. **Emission** — pointer levels become trailing `*`s; nested
//!    function-pointer parameters are rendered opaquely as the literal
//!    token `"fnptr"` per **DESIGN §3.4** Phase A opacity rule. Phase B
//!    may upgrade `fnptr` to a recursive nested signature.
//!
//! ## Test mapping
//!
//! The eight assertions in `signature_builder_tests` map 1:1 to
//! **TEST:c-icall-precision-019** in
//! `docs/development/c-semantic-phase-a-icall-precision/TRACEABILITY-c-icall-precision.toml`.
//!
//! ## Wiring status
//!
//! This module is **landed independent of its consumer**. The Phase-1
//! parse pipeline that calls into [`build_function_signature`] /
//! [`build_local_type_token`] lands in `U10_PHASE1_INSTRUMENT` on the
//! same feature branch (`feat/c-icall-precision-phase-a`). Until that
//! unit ships, every helper and the two `pub(crate)` entry-points are
//! `#[allow(dead_code)]` to match the pattern set by U01's
//! `WIDTH_ALIAS_TABLE` / `normalize_width_alias` (see
//! `type_extractor.rs:50,99`).

#![allow(dead_code)] // module-wide allowance — consumers land in U10_PHASE1_INSTRUMENT

use std::collections::HashMap;

use sqry_core::graph::CodeGraph;
use sqry_core::graph::unified::edge::kind::{EdgeKind, TypeOfContext};
use sqry_core::graph::unified::node::kind::NodeKind;
use tree_sitter::Node;

use crate::relations::type_extractor::{extract_all_type_names_from_c_type, normalize_width_alias};

/// Maximum depth for typedef-chain resolution (DESIGN §3.3, U07 prompt).
///
/// Matches the recursion-guard limit used elsewhere in the C plugin;
/// chains deeper than this resolve to whichever name we reached at the
/// 8th hop (cycle/runaway safety).
pub(crate) const TYPEDEF_DEPTH_LIMIT: u8 = 8;

// ---------------------------------------------------------------------------
// TypedefChain
// ---------------------------------------------------------------------------

/// Alias-resolution table built from `EdgeKind::TypeOf` edges that
/// originate at typedef alias type-nodes (DESIGN §3.3).
///
/// `aliases[alias_name] = target_name`. `resolve` chases the chain up
/// to [`TYPEDEF_DEPTH_LIMIT`] hops and bails — this is the cycle-safety
/// bound; a perfectly correct C program can't have actual cycles, but
/// half-parsed snippets and corrupt edge sets must not be able to hang
/// the walker.
///
/// **Keying**: this chain is **`String`-keyed** (option (a) per the
/// U07 iter-1 codex fix). The base-token recovery primitive
/// ([`extract_all_type_names_from_c_type`]) emits raw `String`s, so
/// keying the chain by `String` avoids threading a `StringInterner`
/// through the declarator walker. The graph-driven builder
/// ([`Self::from_type_of_edges`]) resolves `StringId`s back to
/// `String`s via the snapshot's `StringInterner` once, at table-build
/// time.
#[derive(Debug, Clone, Default)]
pub(crate) struct TypedefChain {
    aliases: HashMap<String, String>,
}

impl TypedefChain {
    /// Returns an empty chain (no typedef aliases known).
    pub(crate) fn new() -> Self {
        Self {
            aliases: HashMap::new(),
        }
    }

    /// Builds a `TypedefChain` from the live `TypeOf` edges in a
    /// `CodeGraph`.
    ///
    /// Per the C plugin's typedef handling in
    /// `graph_builder.rs::process_single_typedef_declarator` (sqry-lang-c
    /// L2116-2150), every typedef emits an `EdgeKind::TypeOf { context:
    /// Some(Variable), .. }` edge from the alias type-node
    /// ([`NodeKind::Type`], via `helper.add_type`) to the underlying-type
    /// type-node.
    ///
    /// **Source-kind filter (codex iter-2 HIGH fix)**: the C plugin
    /// *also* emits `EdgeKind::TypeOf { context: Some(Variable), .. }`
    /// edges for ordinary variable declarations (`process_variable_node`
    /// at `graph_builder.rs:1735-1752`) — those edges have a
    /// [`NodeKind::Variable`] source rather than a [`NodeKind::Type`]
    /// source. Without filtering, an ordinary `int x;` would be inserted
    /// into the alias map as `"x" -> "int"`, which then causes
    /// `canonicalise_base` to rewrite unrelated base tokens that happen
    /// to match the variable name. We therefore require the source node
    /// to be a [`NodeKind::Type`] before treating the edge as a typedef
    /// alias. The two emission paths use the same `TypeOfContext::Variable`
    /// today (matching the legacy C-plugin convention for "this is a
    /// `T x` shape"), so the source-`NodeKind` discriminator is the only
    /// way to tell them apart from outside the plugin.
    ///
    /// Edges with no matching source/target node entry (stale) are
    /// silently skipped — the table degrades gracefully into "no alias
    /// known for this name" rather than panicking on a partially
    /// loaded graph. `StringId`s that don't resolve through the
    /// interner are likewise skipped.
    pub(crate) fn from_type_of_edges(graph: &CodeGraph) -> Self {
        let mut aliases = HashMap::new();
        let snapshot = graph.snapshot();
        let strings = snapshot.strings();
        for (source, target, kind) in snapshot.iter_edges() {
            if let EdgeKind::TypeOf {
                context: Some(TypeOfContext::Variable),
                ..
            } = kind
            {
                let (Some(source_entry), Some(target_entry)) =
                    (snapshot.get_node(source), snapshot.get_node(target))
                else {
                    continue;
                };
                // Filter: only typedef-alias type-nodes count as
                // typedef aliases. Ordinary variable declarations also
                // emit `TypeOf{Variable}` but with a `NodeKind::Variable`
                // source (see doc-comment above).
                if source_entry.kind != NodeKind::Type {
                    continue;
                }
                let (Some(source_name), Some(target_name)) = (
                    strings.resolve(source_entry.name),
                    strings.resolve(target_entry.name),
                ) else {
                    continue;
                };
                aliases.insert(
                    source_name.as_ref().to_string(),
                    target_name.as_ref().to_string(),
                );
            }
        }
        Self { aliases }
    }

    /// Inserts an explicit alias mapping. Intended for unit-tests and
    /// the (future) phase-1 instrumentation path that builds the chain
    /// from staging data before the snapshot is committed.
    pub(crate) fn insert(&mut self, alias: String, target: String) {
        self.aliases.insert(alias, target);
    }

    /// Returns `true` when no aliases have been recorded.
    pub(crate) fn is_empty(&self) -> bool {
        self.aliases.is_empty()
    }

    /// Resolves `alias` to its terminal target, chasing through at most
    /// [`TYPEDEF_DEPTH_LIMIT`] intermediate aliases.
    ///
    /// At each hop, if the current name is itself a key in `aliases`,
    /// we step. Otherwise we stop. After [`TYPEDEF_DEPTH_LIMIT`] hops
    /// we stop unconditionally (cycle safety) and return whatever we
    /// reached. The returned `&str` borrows from `self`; callers that
    /// need ownership should clone.
    pub(crate) fn resolve<'a>(&'a self, alias: &'a str) -> &'a str {
        let mut current: &str = alias;
        for _ in 0..TYPEDEF_DEPTH_LIMIT {
            match self.aliases.get(current) {
                Some(next) if next.as_str() != current => current = next.as_str(),
                _ => return current,
            }
        }
        current
    }
}

// ---------------------------------------------------------------------------
// Declarator (private machinery)
// ---------------------------------------------------------------------------

/// Structured representation of a C declarator after the walker has
/// classified it.
///
/// Mirrors the prompt's `Declarator { base, pointer_depth,
/// is_array_decayed, is_function_pointer_param }` shape. `base` is the
/// recovered base-type token *before* normalisation; pointer depth
/// captures `*` levels collected during the walk; `is_array_decayed`
/// records that the declarator is an array parameter (one extra `*` of
/// canonical pointer depth in C's standard array-to-pointer decay
/// rule). `is_function_pointer_param` toggles the entirely separate
/// nested-function-pointer rendering path (e.g. `int(*)(int)` as the
/// inner of `void(int(*)(int))`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Declarator {
    base: String,
    pointer_depth: u8,
    is_array_decayed: bool,
    /// When set, [`base`] is the *already-rendered* canonical signature
    /// of the nested function-pointer parameter (e.g.
    /// `"int(*)(int)"`). [`pointer_depth`] / [`is_array_decayed`] are
    /// ignored by [`declarator_to_token`] in that case.
    is_function_pointer_param: bool,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build the canonical type-signature string for the function (or
/// abstract function declarator) at `decl_node`.
///
/// `decl_node` may be a `function_definition`, a `declaration`, a
/// `type_definition`, a `function_declarator`, or an
/// `abstract_function_declarator` (the abstract form is used inside
/// function-pointer types). For wrapper nodes (`function_definition`,
/// `declaration`, `type_definition`) the function descends through
/// pointer/parenthesised declarators until it locates the function
/// declarator that owns the `parameters` field.
///
/// Returns `None` on malformed input — e.g. a node that contains no
/// `parameter_list` at all.
pub(crate) fn build_function_signature(
    decl_node: Node,
    content: &[u8],
    typedef_chain: &TypedefChain,
) -> Option<String> {
    let base_tokens = collect_base_tokens(decl_node, content);
    let func_decl = find_function_declarator(decl_node)?;
    build_function_signature_from_func_decl(func_decl, &base_tokens, content, typedef_chain)
}

/// Build the canonical type token for a local variable / parameter
/// declaration at `decl_node`.
///
/// For a function-pointer-typed variable / parameter, this returns the
/// same string `build_function_signature` would produce for the inner
/// abstract function declarator (i.e. `"int(int)"` for `int (*p)(int)`).
/// For a plain pointer / value type, it returns the base token followed
/// by the right number of `*`s, with width-alias canonicalisation and
/// typedef chasing applied.
///
/// Returns `None` on malformed input.
pub(crate) fn build_local_type_token(
    decl_node: Node,
    content: &[u8],
    typedef_chain: &TypedefChain,
) -> Option<String> {
    let base_tokens = collect_base_tokens(decl_node, content);

    // Function-pointer local? Detect via the presence of a
    // function_declarator nested inside the declarator chain.
    if let Some(func_decl) = find_function_declarator(decl_node) {
        return build_function_signature_from_func_decl(
            func_decl,
            &base_tokens,
            content,
            typedef_chain,
        );
    }

    // Non-function-pointer local: walk the declarator chain to count
    // pointer depth.
    let declarator_node = child_declarator(decl_node);
    let (pointer_depth, is_array_decayed) = match declarator_node {
        Some(d) => walk_pointer_depth(d, false),
        None => (0, false),
    };

    let base = canonicalise_base(&base_tokens, typedef_chain);
    if base.is_empty() {
        return None;
    }
    Some(declarator_to_token(&Declarator {
        base,
        pointer_depth,
        is_array_decayed,
        is_function_pointer_param: false,
    }))
}

// ---------------------------------------------------------------------------
// Base-token recovery
// ---------------------------------------------------------------------------

/// Collect the base-type token list for a declaration-like node.
///
/// Walks the *direct* (non-named-only) children of `decl_node` and
/// hands type-specifier children off to
/// [`extract_all_type_names_from_c_type`]. Qualifiers and storage-class
/// specifiers are skipped. Mirrors the recovery logic at
/// `type_extractor.rs::extract_type_specifiers_from_declaration` but
/// also recurses through nested declarators when invoked on a
/// declarator node directly (so that callers can pass a
/// `function_declarator` whose surrounding declaration is unavailable).
fn collect_base_tokens(decl_node: Node, content: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = decl_node.walk();
    for child in decl_node.children(&mut cursor) {
        match child.kind() {
            "primitive_type"
            | "type_identifier"
            | "struct_specifier"
            | "union_specifier"
            | "enum_specifier"
            | "sized_type_specifier" => {
                out.extend(extract_all_type_names_from_c_type(child, content));
            }
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Declarator chain navigation
// ---------------------------------------------------------------------------

/// Returns the `declarator` field of `node`, if any.
fn child_declarator(node: Node) -> Option<Node> {
    node.child_by_field_name("declarator")
}

/// Locate the **function_declarator** that names this declaration —
/// i.e. the function declarator whose `declarator` field walks down
/// the declarator chain to the leaf identifier (or, for abstract
/// forms, to no further function declarator). In the spiral-reading
/// of a C declaration this is the *innermost* function declarator;
/// its parameter list is the parameter list **of the declared entity
/// itself** (vs. nested function-pointer return types).
///
/// Walks `child_by_field_name("declarator")` repeatedly, descending
/// through `pointer_declarator`, `array_declarator`,
/// `parenthesized_declarator`, and their abstract siblings, plus
/// generic named-child scans to handle wrapper nodes like
/// `type_definition`. Returns the deepest matching node.
fn find_function_declarator(node: Node) -> Option<Node> {
    // Try the `declarator` field directly first — descend as deep as
    // we can find a function declarator further down the chain.
    let mut best: Option<Node> = None;
    if matches!(
        node.kind(),
        "function_declarator" | "abstract_function_declarator"
    ) {
        best = Some(node);
    }
    if let Some(child) = child_declarator(node) {
        if let Some(found) = find_function_declarator(child) {
            best = Some(found);
        }
    } else {
        // …fall back to a generic named-child scan for wrapper nodes
        // (type_definition / declaration / parenthesized) that don't
        // expose a `declarator` field at this layer.
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if let Some(found) = find_function_declarator(child) {
                best = Some(found);
                break;
            }
        }
    }
    best
}

/// Count pointer / array-decay levels by walking *down* the declarator
/// chain from `decl_node` until we reach a leaf (identifier / nothing).
///
/// Returns `(pointer_depth, is_array_decayed)`. `is_array_decayed` is
/// set when at least one array_declarator was traversed at a parameter
/// position (which always decays in C — there's no top-level "non-
/// parameter array decay" concept relevant to a single token).
fn walk_pointer_depth(node: Node, mut array_seen: bool) -> (u8, bool) {
    let mut depth: u8 = 0;
    let mut current = node;
    loop {
        match current.kind() {
            "pointer_declarator" | "abstract_pointer_declarator" => {
                depth = depth.saturating_add(1);
            }
            "array_declarator" | "abstract_array_declarator" => {
                array_seen = true;
            }
            "parenthesized_declarator" | "abstract_parenthesized_declarator" => {
                // Pass through.
            }
            _ => {
                break;
            }
        }
        match child_declarator(current) {
            Some(next) => current = next,
            None => {
                // For abstract_pointer_declarator there may be no
                // `declarator` field but a single named child carrying
                // further structure.
                let mut cursor = current.walk();
                let mut moved = false;
                for child in current.named_children(&mut cursor) {
                    if matches!(
                        child.kind(),
                        "pointer_declarator"
                            | "abstract_pointer_declarator"
                            | "array_declarator"
                            | "abstract_array_declarator"
                            | "parenthesized_declarator"
                            | "abstract_parenthesized_declarator"
                    ) {
                        current = child;
                        moved = true;
                        break;
                    }
                }
                if !moved {
                    break;
                }
            }
        }
    }
    (depth, array_seen)
}

// ---------------------------------------------------------------------------
// Parameter list handling
// ---------------------------------------------------------------------------

/// Walk a `parameter_list` and emit one canonical token per parameter
/// (plus a trailing `"..."` for variadic functions).
///
/// Each parameter is rendered via [`build_parameter_token`].
fn walk_parameter_list(
    params_node: Node,
    content: &[u8],
    typedef_chain: &TypedefChain,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = params_node.walk();
    for child in params_node.named_children(&mut cursor) {
        match child.kind() {
            "parameter_declaration" => {
                if let Some(tok) = build_parameter_token(child, content, typedef_chain) {
                    out.push(tok);
                }
            }
            "variadic_parameter" | "..." => {
                out.push("...".to_string());
            }
            _ => {}
        }
    }
    out
}

/// Build the canonical token for a single `parameter_declaration` node.
///
/// Three shapes:
/// - **nested function pointer** (Phase A opacity, DESIGN §3.4): the
///   parameter's declarator chain contains a `function_declarator`
///   nested inside a pointer/parenthesised declarator. The inner
///   signature is **NOT** recursively examined; the parameter is
///   rendered as the opaque literal token `"fnptr"`. Phase B may
///   upgrade this to recursive nested rendering.
/// - **array parameter**: array decay bumps the canonical pointer
///   depth by 1.
/// - **plain pointer / value parameter**: base token + `*`*pointer_depth.
fn build_parameter_token(
    param_node: Node,
    content: &[u8],
    typedef_chain: &TypedefChain,
) -> Option<String> {
    let base_tokens = collect_base_tokens(param_node, content);

    // Nested function-pointer parameter? DESIGN §3.4: emit the opaque
    // token `"fnptr"` — do NOT recurse into the inner signature.
    if find_function_declarator(param_node).is_some() {
        return Some(declarator_to_token(&Declarator {
            base: "fnptr".to_string(),
            pointer_depth: 0,
            is_array_decayed: false,
            is_function_pointer_param: true,
        }));
    }

    let declarator_node = child_declarator(param_node);
    let (pointer_depth, is_array_decayed) = match declarator_node {
        Some(d) => walk_pointer_depth(d, false),
        None => (0, false),
    };

    let base = canonicalise_base(&base_tokens, typedef_chain);
    if base.is_empty() {
        return None;
    }
    Some(declarator_to_token(&Declarator {
        base,
        pointer_depth,
        is_array_decayed,
        is_function_pointer_param: false,
    }))
}

// ---------------------------------------------------------------------------
// Token rendering
// ---------------------------------------------------------------------------

/// Render a fully-classified [`Declarator`] to its canonical token form.
fn declarator_to_token(d: &Declarator) -> String {
    if d.is_function_pointer_param {
        return d.base.clone();
    }
    let total_pointer = d.pointer_depth.saturating_add(u8::from(d.is_array_decayed));
    let mut s = d.base.clone();
    for _ in 0..total_pointer {
        s.push('*');
    }
    s
}

/// Combine return token + parameter tokens into the final
/// `<ret>(<p1>,<p2>,...)` form. An empty parameter list emits an
/// empty parenthesis pair: `"void()"`.
fn build_signature_string(return_token: &str, params: &[String]) -> String {
    let mut s = String::with_capacity(
        return_token.len() + 2 + params.iter().map(String::len).sum::<usize>() + params.len(),
    );
    s.push_str(return_token);
    s.push('(');
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(p);
    }
    s.push(')');
    s
}

// ---------------------------------------------------------------------------
// Normalisation: typedef chase + width-alias remap
// ---------------------------------------------------------------------------

/// Canonicalise the join of `base_tokens` against `typedef_chain` and
/// the width-alias table.
///
/// Pipeline:
/// 1. Join base tokens into a single string (matches the existing
///    convention used in `process_single_typedef_declarator`).
/// 2. Chase typedef aliases through `typedef_chain.resolve(...)`. The
///    chain is keyed by the **raw base-token string**; resolution
///    walks up to [`TYPEDEF_DEPTH_LIMIT`] hops (cycle safety) and
///    returns the terminal target name.
/// 3. Run the resolved token through [`normalize_width_alias`] so
///    width-fixed aliases (`size_t`, `uint64_t`, …) collapse onto
///    their canonical machine-width representative.
///
/// Plain (non-typedef) tokens that aren't in the chain fall through
/// step 2 unchanged and proceed directly to width-alias
/// normalisation. Per DESIGN §3.3 (typedef chasing).
fn canonicalise_base(base_tokens: &[String], typedef_chain: &TypedefChain) -> String {
    let joined = base_tokens.join(" ");
    let resolved = typedef_chain.resolve(&joined);
    normalize_width_alias(resolved).to_string()
}

// ---------------------------------------------------------------------------
// Inner: build signature once we already have a function_declarator
// ---------------------------------------------------------------------------

/// Inner workhorse: given the `function_declarator` /
/// `abstract_function_declarator` node and the recovered base tokens
/// for the **enclosing declaration's** type specifiers, render the
/// final signature.
fn build_function_signature_from_func_decl(
    func_decl: Node,
    base_tokens: &[String],
    content: &[u8],
    typedef_chain: &TypedefChain,
) -> Option<String> {
    let params_node = func_decl.child_by_field_name("parameters")?;
    let params = walk_parameter_list(params_node, content, typedef_chain);

    // Return-token assembly. We start from the recovered base tokens
    // and add one `*` per pointer level **between** the function
    // declarator and the enclosing declaration — these pointers belong
    // to the return type. Pointers wrapping the function itself (i.e.
    // *AROUND* the function declarator at the *outermost* parenthesised
    // level) are the function-pointer indirection and do NOT bump the
    // return-type pointer depth.
    let mut return_pointer_depth: u8 = 0;
    // Look at the `declarator` field of the function declarator —
    // anything there is *return-type* pointer wrapping (for the form
    // `int *(*)(int)`: the abstract_function_declarator's declarator
    // chain contains an abstract_pointer_declarator that wraps a
    // parenthesized abstract_pointer_declarator. The pointers OUTSIDE
    // the parenthesised group belong to the return type.)
    if let Some(d) = child_declarator(func_decl) {
        return_pointer_depth = return_pointer_depth_of(d);
    }

    let mut return_token = canonicalise_base(base_tokens, typedef_chain);
    if return_token.is_empty() {
        return None;
    }
    for _ in 0..return_pointer_depth {
        return_token.push('*');
    }

    Some(build_signature_string(&return_token, &params))
}

/// Count pointer-declarator levels that occur **before** the first
/// parenthesised group inside `node`. Those are return-type pointers.
///
/// `int (*)(int)`: the function_declarator's `declarator` field is an
/// `abstract_pointer_declarator`, but that pointer is INSIDE the
/// parenthesised group, so its depth is 0 from this perspective.
///
/// `int* (*)(int)`: the abstract_function_declarator's declarator
/// chain is `abstract_pointer_declarator(abstract_parenthesized_declarator(abstract_pointer_declarator(...)))`
/// — the outer pointer is at top level (return-type) and the inner is
/// inside the parens (function-pointer indirection). We count only the
/// outer.
fn return_pointer_depth_of(node: Node) -> u8 {
    let mut depth: u8 = 0;
    let mut current = node;
    loop {
        match current.kind() {
            "pointer_declarator" | "abstract_pointer_declarator" => {
                depth = depth.saturating_add(1);
                match child_declarator(current) {
                    Some(next) => current = next,
                    None => break,
                }
            }
            "parenthesized_declarator" | "abstract_parenthesized_declarator" => {
                // Stop — pointers inside the parens are part of the
                // function-pointer indirection, not the return type.
                break;
            }
            _ => break,
        }
    }
    depth
}

// ---------------------------------------------------------------------------
// Tests (TEST:c-icall-precision-019)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod signature_builder_tests {
    //! TEST:c-icall-precision-019 — eight per-shape assertions per the
    //! prompt's acceptance matrix (declarator alphabet rows, typedef
    //! depth-2 chase, width-alias normalisation, depth-8 cycle cap,
    //! nested function-pointer parameter, parenthesised declarator).

    use super::*;
    use tree_sitter::Tree;

    /// Parse a snippet of C into a tree-sitter `Tree`.
    fn parse(code: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .expect("set C language");
        parser.parse(code, None).expect("parse C")
    }

    /// Find the first node of `kind` in tree-order, recursively.
    fn find_first<'a>(root: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
        if root.kind() == kind {
            return Some(root);
        }
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if let Some(found) = find_first(child, kind) {
                return Some(found);
            }
        }
        None
    }

    /// Build a signature directly from the `type_definition` node of a
    /// `typedef <return> (*<name>)(<params>);` snippet.
    fn signature_from_typedef(code: &str, chain: &TypedefChain) -> Option<String> {
        let tree = parse(code);
        let td = find_first(tree.root_node(), "type_definition")?;
        build_function_signature(td, code.as_bytes(), chain)
    }

    // ----- Test 1: int (*)(int) → "int(int)" -----

    #[test]
    fn signature_int_int() {
        let code = "typedef int (*FnA)(int);";
        let chain = TypedefChain::new();
        let sig = signature_from_typedef(code, &chain).expect("build_function_signature");
        assert_eq!(sig, "int(int)");
    }

    // ----- Test 2: void (*)(char**) → "void(char**)" -----

    #[test]
    fn signature_void_charpp() {
        let code = "typedef void (*FnB)(char**);";
        let chain = TypedefChain::new();
        let sig = signature_from_typedef(code, &chain).expect("build_function_signature");
        assert_eq!(sig, "void(char**)");
    }

    // ----- Test 3: int (*)(int[10]) → "int(int*)" (array decay) -----

    #[test]
    fn signature_array_decay() {
        let code = "typedef int (*FnC)(int x[10]);";
        let chain = TypedefChain::new();
        let sig = signature_from_typedef(code, &chain).expect("build_function_signature");
        assert_eq!(sig, "int(int*)");
    }

    // ----- Test 4: void (*)(int (*)(int)) → "void(fnptr)" (Phase A opacity) -----

    #[test]
    fn signature_nested_function_pointer_param() {
        // DESIGN §3.4 Phase A opacity: a function-pointer parameter
        // is rendered as the literal token `"fnptr"`; the inner
        // signature (`int(*)(int)`) is NOT recursively examined. The
        // binding plane (DESIGN §7) almost always disambiguates these
        // in practice, and Phase B may upgrade to recursive nested
        // signatures. Named inner so tree-sitter unambiguously parses
        // it.
        let code = "typedef void (*FnD)(int (*cb)(int));";
        let chain = TypedefChain::new();
        let sig = signature_from_typedef(code, &chain).expect("build_function_signature");
        assert_eq!(sig, "void(fnptr)");
    }

    // ----- Test 5: typedef chase depth 2: MyInt → int32_t → int -----

    #[test]
    fn signature_typedef_chain_depth_two() {
        // Exercise the production `TypedefChain::resolve` path
        // through `build_function_signature` → `canonicalise_base`.
        // `MyInt` aliases `int32_t`, `int32_t` aliases `int`. The
        // signature builder sees `MyInt` as the parameter / return
        // type and must produce a signature that uses `"int"` after
        // chasing both hops + the width-alias normalisation.
        let mut chain = TypedefChain::new();
        chain.insert("MyInt".to_string(), "int32_t".to_string());
        chain.insert("int32_t".to_string(), "int".to_string());

        // Sanity-check the resolver in isolation.
        assert_eq!(chain.resolve("MyInt"), "int");
        // And the width-alias table independently maps `int` → `int`
        // (idempotent for already-canonical machine types).
        assert_eq!(normalize_width_alias("int"), "int");

        // End-to-end: build a signature with the chain. The function
        // takes a `MyInt` parameter and returns `MyInt`.
        let code = "typedef MyInt (*FnE)(MyInt);";
        let sig = signature_from_typedef(code, &chain).expect("build_function_signature");
        assert_eq!(sig, "int(int)");
    }

    // ----- Test 6: size_t (*)(uint64_t) → "long(long)" (width-alias) -----

    #[test]
    fn signature_width_alias_size_t_uint64() {
        // No typedef-chain entries — pure width-alias path.
        let code = "typedef size_t (*FnF)(uint64_t);";
        let chain = TypedefChain::new();
        let sig = signature_from_typedef(code, &chain).expect("build_function_signature");
        assert_eq!(sig, "long(long)");
    }

    // ----- Test 7: depth-8 cap: A0→A1→…→A9; resolve(A0) = A8 -----

    #[test]
    fn typedef_chain_resolve_depth_eight_cap() {
        // Build aliases A0→A1, A1→A2, …, A8→A9 (9 hops total
        // possible). After exactly TYPEDEF_DEPTH_LIMIT hops (=8), the
        // resolver bails and returns whatever it reached, which is
        // A8. (Hops: A0→A1, A1→A2, A2→A3, A3→A4, A4→A5, A5→A6,
        // A6→A7, A7→A8 — that's the 8th hop, then we stop.)
        //
        // Exercises the production `String`-keyed resolve path.
        let mut chain = TypedefChain::new();
        for i in 0u32..9 {
            chain.insert(format!("A{i}"), format!("A{}", i + 1));
        }
        let resolved = chain.resolve("A0");
        assert_eq!(resolved, "A8");
    }

    // ----- Regression: from_type_of_edges must filter by source NodeKind -----

    /// Regression for the codex iter-2 HIGH finding: the C plugin emits
    /// `EdgeKind::TypeOf { context: Some(Variable), .. }` for both
    /// typedef aliases (`graph_builder.rs:2131-2143`, source =
    /// `NodeKind::Type` via `helper.add_type`) AND ordinary variables
    /// (`graph_builder.rs:1735-1752`, source = `NodeKind::Variable` via
    /// `helper.add_variable`). Without filtering, an ordinary `int x;`
    /// would land in the typedef-chain alias map as `"x" -> "int"` and
    /// later cause `canonicalise_base` to rewrite unrelated base tokens
    /// matching variable names.
    ///
    /// This test programmatically constructs the two edge shapes the C
    /// plugin produces and asserts that `from_type_of_edges` records
    /// the typedef alias but ignores the variable's type edge.
    #[test]
    fn from_type_of_edges_ignores_variable_typeof_edges() {
        use sqry_core::graph::node::Language;
        use sqry_core::graph::unified::concurrent::CodeGraph;
        use sqry_core::graph::unified::edge::kind::EdgeKind;
        use sqry_core::graph::unified::node::kind::NodeKind;
        use sqry_core::graph::unified::storage::arena::NodeEntry;

        let mut graph = CodeGraph::new();

        // Register a fake C source file.
        let file_id = graph
            .files_mut()
            .register_with_language(
                std::path::Path::new("/fixture/typedef_filter.c"),
                Some(Language::C),
            )
            .expect("register file");

        // String interns.
        let int_name = graph.strings_mut().intern("int").expect("intern int");
        let myint_name = graph.strings_mut().intern("MyInt").expect("intern MyInt");
        let x_name = graph.strings_mut().intern("x").expect("intern x");

        // Shared target node: `int` (NodeKind::Type, mirrors helper.add_type).
        let int_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Type, int_name, file_id))
            .expect("alloc int type node");

        // Typedef alias node: `MyInt` (NodeKind::Type, exactly as
        // process_single_typedef_declarator emits at gb.rs:2132).
        let myint_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Type, myint_name, file_id))
            .expect("alloc MyInt type node");

        // Ordinary variable node: `x` (NodeKind::Variable, exactly as
        // process_variable_node emits at gb.rs:1735).
        let x_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Variable, x_name, file_id))
            .expect("alloc x variable node");

        // Both shapes use `TypeOf { context: Some(Variable), .. }` — the
        // collision the codex finding flagged.
        let typeof_kind = EdgeKind::TypeOf {
            context: Some(TypeOfContext::Variable),
            index: None,
            name: None,
        };

        // Typedef edge: MyInt(Type) -> int(Type). MUST land in chain.
        graph
            .edges_mut()
            .add_edge(myint_id, int_id, typeof_kind.clone(), file_id);

        // Variable edge: x(Variable) -> int(Type). MUST be filtered out.
        graph
            .edges_mut()
            .add_edge(x_id, int_id, typeof_kind, file_id);

        let chain = TypedefChain::from_type_of_edges(&graph);

        // Typedef alias present: resolves to its target.
        assert_eq!(
            chain.resolve("MyInt"),
            "int",
            "typedef alias MyInt -> int must be recorded"
        );
        // Variable NOT inserted: resolve falls through to the input.
        assert_eq!(
            chain.resolve("x"),
            "x",
            "ordinary variable x must NOT pollute the typedef chain"
        );
    }

    // ----- Test 8: parenthesized: int (*(*)(int))(double) -----

    #[test]
    fn signature_parenthesized_well_formed() {
        // `int (*(*FnG)(int))(double);` — FnG is a function pointer
        // taking (int) and returning a pointer to a function taking
        // (double) returning int. Phase A renders this in a
        // deterministic, well-formed way: the OUTER signature is
        // `int(int)` (return type of the outer function pointer is a
        // pointer-to-function which we render as the base token
        // `int` *plus* the outer `*`s collected — at this depth the
        // signature is well-formed in the sense of "non-empty,
        // balanced parentheses, ends with `)`").
        let code = "typedef int (*(*FnG)(int))(double);";
        let chain = TypedefChain::new();
        let sig = signature_from_typedef(code, &chain).expect("build_function_signature");
        assert!(!sig.is_empty(), "expected non-empty signature, got {sig:?}");
        assert!(
            sig.starts_with("int") && sig.contains('(') && sig.ends_with(')'),
            "expected well-formed signature shape, got {sig:?}"
        );
        // Per DESIGN §3.1's spiral-reading rule, the
        // **innermost / deepest** function_declarator names the
        // declared entity itself — for `int (*(*FnG)(int))(double)`
        // that's the declarator with parameter list `(int)` (FnG
        // takes an `int`). The outer `(double)` parameter list
        // belongs to the *return-type's* function-pointer chain and
        // is not the parameter list of `FnG` itself. The renderer
        // therefore selects the innermost function declarator and
        // produces `int(int)`.
        assert_eq!(sig, "int(int)");
    }
}
