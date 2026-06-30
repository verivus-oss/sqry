//! Shared identifier-blind shape walker (WS2).
//!
//! This module owns the ONE routine that turns an already-parsed function/method
//! subtree into a [`ShapeDescriptor`]: the [`ShapeMapping`] per-plugin protocol,
//! the frozen [`CfBucket`] control-flow schema, and [`compute_shape_descriptor`],
//! which produces the AST shingle, the `Weisfeiler-Lehman` relabel, the `MinHash`,
//! and the renaming-invariant `shape_hash`.
//!
//! The data model it populates lives in
//! [`crate::graph::unified::storage::shape`] (WS1); the build seam that calls this
//! routine (`build/staging.rs`) and the `NodeId`-keyed side table that stores the
//! result are wired in later units. Nothing here touches `body_hash`: the two
//! hashes are deliberately separate (AC-5).
//!
//! # Identifier-blindness (AC-2)
//!
//! Two independent properties keep the descriptor blind to names and literal
//! values:
//!
//! 1. tree-sitter `kind_id`s are grammar-symbol ids, not source text, so every
//!    `identifier` already shares one id regardless of the name it spells. The
//!    text never enters any hash because only `kind_id`s are walked.
//! 2. On top of that, every NAMED leaf (an `identifier`, `field_identifier`, or a
//!    `string`/`number` literal token: a named node with no children) is
//!    canonicalized to a single [`LEAF_TOKEN`], so two functions that differ only
//!    in a literal's *type* (an integer vs a string in the same slot) still hash
//!    identically. Anonymous tokens (operators, keywords, punctuation) keep their
//!    `kind_id` because they ARE the structural shape, not user-chosen content.
//!
//! # Whitespace/comment invariance (AC-3)
//!
//! Comment and whitespace nodes are tree-sitter "extras"; they are skipped in the
//! walk, so reformatting a body or injecting comments leaves the descriptor
//! byte-identical (while `body_hash` still differs, by design).
//!
//! # Determinism (AC-1)
//!
//! No wall-clock and no RNG. Every hash uses the fixed `SHAPE_SEED_*` seeds from
//! [`crate::graph::body_hash`]; multisets are sorted before hashing and the
//! `MinHash` is order-independent by construction, so the same input tree yields
//! the same descriptor across processes. Changing the seeds, the [`CfBucket`]
//! order, the canonicalization rule, or the lane derivation changes the bytes of
//! every descriptor and MUST bump `SHAPE_SCHEMA_VERSION`.

use crate::graph::body_hash::{SHAPE_SEED_0, SHAPE_SEED_1};
use crate::graph::unified::storage::shape::{
    CF_BUCKET_COUNT, CalleeShape, MIN_HASHABLE_TOKENS, MINHASH_LANES, ShapeDescriptor, ShapeFlags,
    ShapeHash128, SignatureShape,
};
use xxhash_rust::xxh64::xxh64;

/// Canonical token id for every NAMED leaf (identifiers and literal tokens).
///
/// `kind_id` is a `u16`, so `u32::MAX` cannot collide with any real grammar
/// symbol. Collapsing named leaves to one token is what makes the structural
/// fingerprint blind to identifier names AND literal types (see the module docs).
const LEAF_TOKEN: u32 = u32::MAX;

/// Default cap on the number of subtree nodes the walker visits for one body.
///
/// Mirrors the bounded-budget convention the build pipeline already uses for
/// pathological inputs (C indirect-call fan-out, the Go method-set hop budget):
/// a body larger than this is fingerprinted from its first `budget` nodes and
/// marked [`ShapeFlags::TRUNCATED`] rather than walked unbounded. Raising it does
/// not change the bytes of in-budget descriptors, so it is NOT a schema-affecting
/// constant; only descriptors that actually cross the budget shift.
pub const DEFAULT_SHAPE_NODE_BUDGET: u32 = 4096;

/// Golden-ratio odd multiplier used to derive the per-lane `MinHash` seeds from
/// [`SHAPE_SEED_0`]. Changing it reshuffles every lane, so it is gated by
/// `SHAPE_SCHEMA_VERSION` like the seeds themselves.
const MINHASH_LANE_MIX: u64 = 0x9E37_79B9_7F4A_7C15;

/// Canonical, language-neutral control-flow buckets.
///
/// Frozen `#[repr(u8)]`, `Branch = 0` ..= `Comprehension = 14`. The order is the
/// cross-language contract (every plugin's [`ShapeMapping::cf_bucket`] maps its
/// grammar's node kinds into this set) and is baked into every serialized
/// `cf_histogram`. Changes are additive-only: append a variant, never reorder or
/// remove one, matching the discriminant-stability discipline of `FrameworkId`
/// and `ResolvedVia`. Any change here bumps `SHAPE_SCHEMA_VERSION`.
///
/// The discriminant doubles as the index into the
/// `[u16; CF_BUCKET_COUNT]` histogram (see [`CfBucket::index`]).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CfBucket {
    /// `if` / `elif` / ternary / guard.
    Branch = 0,
    /// `for` / `while` / `do` / `loop` / comprehension-loop.
    Loop = 1,
    /// `switch` / `match` / `case`.
    Match = 2,
    /// `try`.
    Try = 3,
    /// `except` / `catch` / `rescue`.
    Catch = 4,
    /// `raise` / `throw`.
    Throw = 5,
    /// `with` / `using` / `defer` / try-with-resources.
    Resource = 6,
    /// `return`.
    Return = 7,
    /// `yield` / `yield from` / generator.
    Yield = 8,
    /// `await` / async-suspend.
    Await = 9,
    /// `break` / `continue`.
    BreakContinue = 10,
    /// Call expression.
    Call = 11,
    /// Assignment / binding.
    Assign = 12,
    /// Lambda / closure / nested function.
    Closure = 13,
    /// List / dict / set / generator comprehension head.
    Comprehension = 14,
}

impl CfBucket {
    /// Number of canonical buckets; kept in lockstep with
    /// [`crate::graph::unified::storage::shape::CF_BUCKET_COUNT`] by
    /// [`CfBucket::ALL`] and a frozen-count test.
    pub const COUNT: usize = CF_BUCKET_COUNT;

    /// Every bucket in discriminant order. Doubles as the freeze witness: the
    /// length and each element's [`index`](CfBucket::index) are asserted in tests.
    pub const ALL: [CfBucket; CF_BUCKET_COUNT] = [
        CfBucket::Branch,
        CfBucket::Loop,
        CfBucket::Match,
        CfBucket::Try,
        CfBucket::Catch,
        CfBucket::Throw,
        CfBucket::Resource,
        CfBucket::Return,
        CfBucket::Yield,
        CfBucket::Await,
        CfBucket::BreakContinue,
        CfBucket::Call,
        CfBucket::Assign,
        CfBucket::Closure,
        CfBucket::Comprehension,
    ];

    /// Histogram index for this bucket (the `#[repr(u8)]` discriminant).
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Per-plugin shape protocol: the ONLY language-specific surface of the feature.
///
/// Everything else (subtree walk, histogram counting, shingling, WL relabel,
/// `MinHash`, `shape_hash`) is the one shared [`compute_shape_descriptor`]
/// routine. A plugin supplies two small things: the mapping from its grammar's
/// node-kind ids to canonical [`CfBucket`]s, and the structural signature shape
/// read from its parameter list. Both must be deterministic and identifier-blind
/// (they may inspect `src` for structural facts like "has a default value", never
/// to hash a name).
pub trait ShapeMapping: Send + Sync {
    /// Map one tree-sitter node-kind id to its canonical control-flow bucket, or
    /// `None` if the kind is not a control-flow construct (it still contributes to
    /// the structural shingle, just not the histogram).
    fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket>;

    /// Read the structural [`SignatureShape`] (arities + flags) directly from the
    /// function's parameter list, independent of the return-type-only `signature`
    /// slot on `NodeEntry`.
    fn signature_shape(&self, fn_node: tree_sitter::Node, src: &[u8]) -> SignatureShape;
}

/// Visited-node budget for one body walk (design §9).
#[derive(Debug, Clone, Copy)]
pub struct ShapeBudget {
    /// Maximum number of non-extra subtree nodes to visit before truncating.
    pub max_visited_nodes: u32,
}

impl Default for ShapeBudget {
    fn default() -> Self {
        Self {
            max_visited_nodes: DEFAULT_SHAPE_NODE_BUDGET,
        }
    }
}

/// Canonical token for a node: [`LEAF_TOKEN`] for a named leaf (identifier or
/// literal), otherwise the grammar `kind_id`. This is the single point where
/// identifier/literal blindness is enforced.
fn canonical_token(node: tree_sitter::Node) -> u32 {
    if node.is_named() && node.child_count() == 0 {
        LEAF_TOKEN
    } else {
        u32::from(node.kind_id())
    }
}

/// Derive the seed for `MinHash` lane `i` from [`SHAPE_SEED_0`]. Distinct,
/// well-spread seeds give `MINHASH_LANES` near-independent hash functions without
/// registering one ASCII seed per lane.
fn lane_seed(i: usize) -> u64 {
    SHAPE_SEED_0 ^ (i as u64).wrapping_mul(MINHASH_LANE_MIX)
}

/// One round of `Weisfeiler-Lehman` relabel for a single node: hash its own
/// canonical token together with the SORTED multiset of its children's canonical
/// tokens. Sorting makes the label invariant to child order (design §4).
fn wl_label(own: u32, child_tokens: &mut [u32]) -> u64 {
    child_tokens.sort_unstable();
    let mut buf = Vec::with_capacity(4 + child_tokens.len() * 4);
    buf.extend_from_slice(&own.to_le_bytes());
    for t in child_tokens.iter() {
        buf.extend_from_slice(&t.to_le_bytes());
    }
    xxh64(&buf, SHAPE_SEED_0)
}

/// Resolve the body subtree of a function node.
///
/// Most tree-sitter grammars expose the body as a `body` field; when absent we
/// fall back to the whole function node (params are named leaves, so they
/// canonicalize to `LEAF` and do not leak identifiers either way).
fn body_subtree(fn_node: tree_sitter::Node) -> tree_sitter::Node {
    fn_node.child_by_field_name("body").unwrap_or(fn_node)
}

/// Accumulator for the single pre-order walk.
struct WalkState {
    token_count: u32,
    histogram: [u16; CF_BUCKET_COUNT],
    shingles: Vec<(u32, u32)>,
    wl_labels: Vec<u64>,
    truncated: bool,
}

/// Walk the body subtree once (iteratively, so deeply-nested bodies cannot blow
/// the stack), filling [`WalkState`].
fn walk_body(
    body: tree_sitter::Node,
    mapping: &dyn ShapeMapping,
    budget: ShapeBudget,
) -> WalkState {
    let mut state = WalkState {
        token_count: 0,
        histogram: [0; CF_BUCKET_COUNT],
        shingles: Vec::new(),
        wl_labels: Vec::new(),
        truncated: false,
    };
    let mut stack = vec![body];
    let mut visited: u32 = 0;

    while let Some(node) = stack.pop() {
        if node.is_extra() {
            continue;
        }
        visited += 1;
        if visited > budget.max_visited_nodes {
            state.truncated = true;
            break;
        }

        if let Some(bucket) = mapping.cf_bucket(node.kind_id()) {
            let slot = &mut state.histogram[bucket.index()];
            *slot = slot.saturating_add(1);
        }

        let own = canonical_token(node);
        let child_count = node.child_count();
        if child_count == 0 {
            state.token_count = state.token_count.saturating_add(1);
        }

        let mut child_tokens: Vec<u32> = Vec::new();
        for i in 0..u32::try_from(child_count).unwrap_or(u32::MAX) {
            let Some(child) = node.child(i) else { continue };
            if child.is_extra() {
                continue;
            }
            let child_token = canonical_token(child);
            child_tokens.push(child_token);
            state.shingles.push((own, child_token));
            stack.push(child);
        }

        state.wl_labels.push(wl_label(own, &mut child_tokens));
    }

    state
}

/// Dual-seeded xxh64 over the sorted shingle multiset: the exact structural
/// identity hash (renaming-invariant by construction).
fn shape_hash_of(shingles: &mut [(u32, u32)]) -> ShapeHash128 {
    shingles.sort_unstable();
    let mut buf = Vec::with_capacity(shingles.len() * 8);
    for (a, b) in shingles.iter() {
        buf.extend_from_slice(&a.to_le_bytes());
        buf.extend_from_slice(&b.to_le_bytes());
    }
    ShapeHash128 {
        high: xxh64(&buf, SHAPE_SEED_0),
        low: xxh64(&buf, SHAPE_SEED_1),
    }
}

/// `MinHash` signature over the WL-label multiset: lane `i` is the minimum of
/// `hash_i(label)` over all labels, giving an LSH-bandable approximate-Jaccard
/// sketch.
fn minhash_of(wl_labels: &[u64]) -> [u32; MINHASH_LANES] {
    let mut sketch = [u32::MAX; MINHASH_LANES];
    for &label in wl_labels {
        let bytes = label.to_le_bytes();
        for (i, lane) in sketch.iter_mut().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let h = xxh64(&bytes, lane_seed(i)) as u32;
            if h < *lane {
                *lane = h;
            }
        }
    }
    sketch
}

/// Compute the identifier-blind [`ShapeDescriptor`] for one function/method
/// subtree.
///
/// Always returns a descriptor, never `None` (AC-1): a body below
/// [`MIN_HASHABLE_TOKENS`] yields an explicit
/// [`ShapeDescriptor::unhashable`] marker (which still carries the meaningful
/// `signature_shape`) rather than a silently-zeroed fingerprint. Whether to call
/// this at all is the seam's decision: the build seam gates on body-span validity
/// (the same `has_valid_body_span` contract as `body_hash`), so suppressed-span
/// nodes get no descriptor, matching `body_hash`.
///
/// `fn_node` is the whole function/method node; the histogram, shingle, WL, and
/// `MinHash` is computed over its [`body_subtree`], while `signature_shape` reads
/// the parameter list off `fn_node` directly.
#[must_use]
pub fn compute_shape_descriptor(
    fn_node: tree_sitter::Node,
    src: &[u8],
    mapping: &dyn ShapeMapping,
    budget: &ShapeBudget,
) -> ShapeDescriptor {
    let signature_shape = mapping.signature_shape(fn_node, src);
    let body = body_subtree(fn_node);
    let mut state = walk_body(body, mapping, *budget);

    // A genuinely tiny body (below the token floor and NOT cut short by the
    // budget) gets the honest unhashable marker. A truncated body is large by
    // definition, so its low in-budget token count never demotes it to
    // unhashable: it keeps a partial descriptor with the truncated flag set.
    if !state.truncated && state.token_count < u32::from(MIN_HASHABLE_TOKENS) {
        return ShapeDescriptor::unhashable(signature_shape);
    }

    let shape_hash = shape_hash_of(&mut state.shingles);
    let minhash = minhash_of(&state.wl_labels);

    let mut flags = ShapeFlags::empty();
    if state.truncated {
        flags.set_truncated();
    }

    ShapeDescriptor {
        cf_histogram: state.histogram,
        signature_shape,
        callee_shape: CalleeShape::Unresolved,
        shape_hash,
        minhash,
        flags,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal Rust-only [`ShapeMapping`] for exercising the shared walker.
    /// The real per-plugin impls land in later units; this only needs to map a
    /// handful of Rust control-flow kinds and count parameters so the walker has
    /// something deterministic to drive.
    struct TestRustMapping;

    impl ShapeMapping for TestRustMapping {
        fn cf_bucket(&self, ts_node_kind_id: u16) -> Option<CfBucket> {
            let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
            let name = lang.node_kind_for_id(ts_node_kind_id)?;
            let bucket = match name {
                "if_expression" => CfBucket::Branch,
                "for_expression" | "while_expression" | "loop_expression" => CfBucket::Loop,
                "match_expression" => CfBucket::Match,
                "return_expression" => CfBucket::Return,
                "await_expression" => CfBucket::Await,
                "break_expression" | "continue_expression" => CfBucket::BreakContinue,
                "call_expression" | "macro_invocation" => CfBucket::Call,
                "let_declaration" | "assignment_expression" => CfBucket::Assign,
                "closure_expression" => CfBucket::Closure,
                _ => return None,
            };
            Some(bucket)
        }

        fn signature_shape(&self, fn_node: tree_sitter::Node, _src: &[u8]) -> SignatureShape {
            let mut shape = SignatureShape::default();
            if let Some(params) = fn_node.child_by_field_name("parameters") {
                let mut cursor = params.walk();
                for child in params.named_children(&mut cursor) {
                    if child.kind() == "parameter" || child.kind() == "self_parameter" {
                        shape.arity_positional = shape.arity_positional.saturating_add(1);
                    }
                }
            }
            shape.has_return_annotation = fn_node.child_by_field_name("return_type").is_some();
            shape
        }
    }

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&lang).expect("load rust grammar");
        parser.parse(src, None).expect("parse")
    }

    /// Resolve the first `function_item` node in a parsed source file.
    fn first_function<'t>(tree: &'t tree_sitter::Tree) -> tree_sitter::Node<'t> {
        let root = tree.root_node();
        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            if child.kind() == "function_item" {
                return child;
            }
        }
        panic!("no function_item in source");
    }

    fn descriptor_of(src: &str) -> ShapeDescriptor {
        let tree = parse(src);
        let func = first_function(&tree);
        compute_shape_descriptor(
            func,
            src.as_bytes(),
            &TestRustMapping,
            &ShapeBudget::default(),
        )
    }

    #[test]
    fn cf_bucket_discriminants_are_frozen() {
        assert_eq!(CfBucket::COUNT, CF_BUCKET_COUNT);
        assert_eq!(CfBucket::ALL.len(), CF_BUCKET_COUNT);
        for (i, bucket) in CfBucket::ALL.iter().enumerate() {
            assert_eq!(bucket.index(), i, "discriminant must equal histogram index");
        }
        // Spot-check the two endpoints of the frozen contract.
        assert_eq!(CfBucket::Branch.index(), 0);
        assert_eq!(CfBucket::Comprehension.index(), CF_BUCKET_COUNT - 1);
    }

    #[test]
    fn ac2_rename_invariance_identifiers_and_literals() {
        // Same structure, every identifier and literal renamed/retyped.
        let a = r#"
            fn original(input: i32) -> i32 {
                let total = input + 42;
                if total > 100 {
                    return total;
                }
                helper(total)
            }
        "#;
        let b = r#"
            fn renamed(arg: i32) -> i32 {
                let sum = arg + 7;
                if sum > 999 {
                    return sum;
                }
                other(sum)
            }
        "#;
        let da = descriptor_of(a);
        let db = descriptor_of(b);
        assert_eq!(
            da.cf_histogram, db.cf_histogram,
            "histogram must be rename-invariant"
        );
        assert_eq!(
            da.shape_hash, db.shape_hash,
            "shape_hash must be rename-invariant"
        );
        assert_eq!(da.minhash, db.minhash, "minhash must be rename-invariant");
        assert!(!da.is_unhashable());
    }

    #[test]
    fn ac3_whitespace_and_comment_invariance() {
        let plain = "fn f(x: i32) -> i32 { let y = x + 1; if y > 0 { return y; } y }";
        let formatted = r#"
            fn f(x: i32) -> i32 {
                // leading comment
                let y = x + 1; // trailing comment

                if y > 0 {
                    /* block comment */
                    return y;
                }
                y
            }
        "#;
        let dp = descriptor_of(plain);
        let df = descriptor_of(formatted);
        assert_eq!(dp.cf_histogram, df.cf_histogram);
        assert_eq!(
            dp.shape_hash, df.shape_hash,
            "comments/whitespace must not change shape_hash"
        );
        assert_eq!(dp.minhash, df.minhash);
    }

    #[test]
    fn different_structure_changes_shape_hash() {
        let loops = descriptor_of("fn a(n: i32) { for i in 0..n { sink(i); } }");
        let branch = descriptor_of("fn a(n: i32) { if n > 0 { sink(n); } }");
        assert_ne!(loops.shape_hash, branch.shape_hash);
        assert_ne!(loops.cf_histogram, branch.cf_histogram);
    }

    #[test]
    fn histogram_counts_control_flow_kinds() {
        let d = descriptor_of(
            "fn a(n: i32) -> i32 { if n > 0 { return n; } for i in 0..n { f(i); } n }",
        );
        assert_eq!(d.cf_histogram[CfBucket::Branch.index()], 1);
        assert_eq!(d.cf_histogram[CfBucket::Loop.index()], 1);
        assert_eq!(d.cf_histogram[CfBucket::Return.index()], 1);
        assert!(d.cf_histogram[CfBucket::Call.index()] >= 1);
    }

    #[test]
    fn signature_shape_reads_parameters_and_return() {
        let d = descriptor_of("fn a(x: i32, y: i32) -> i32 { x + y + 1 }");
        assert_eq!(d.signature_shape.arity_positional, 2);
        assert!(d.signature_shape.has_return_annotation);
    }

    #[test]
    fn sub_four_token_body_is_unhashable_not_none() {
        // An empty body `{ }` is two tokens (`{`, `}`).
        let d = descriptor_of("fn tiny() {}");
        assert!(
            d.is_unhashable(),
            "tiny body must carry the honest unhashable marker"
        );
        assert!(d.shape_hash.is_zero());
        assert_eq!(d.cf_histogram, [0; CF_BUCKET_COUNT]);
    }

    #[test]
    fn determinism_two_computes_match() {
        let src = "fn a(n: i32) -> i32 { let mut s = 0; for i in 0..n { s += g(i); } s }";
        assert_eq!(descriptor_of(src), descriptor_of(src));
    }

    #[test]
    fn over_budget_body_is_truncated() {
        let src = "fn a(n: i32) -> i32 { let y = n + 1; if y > 0 { return y; } y }";
        let tree = parse(src);
        let func = first_function(&tree);
        let tight = ShapeBudget {
            max_visited_nodes: 3,
        };
        let d = compute_shape_descriptor(func, src.as_bytes(), &TestRustMapping, &tight);
        assert!(
            d.is_truncated(),
            "a body past the node budget must be marked truncated"
        );
    }

    #[test]
    fn callee_shape_is_unresolved_this_effort() {
        let d = descriptor_of("fn a(n: i32) -> i32 { if n > 0 { return n; } n }");
        assert_eq!(d.callee_shape, CalleeShape::Unresolved);
    }
}
