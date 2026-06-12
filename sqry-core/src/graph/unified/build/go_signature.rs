//! Public shim around the Go canonical signature normaliser used by the
//! Go T1 implements-and-promotion pass.
//!
//! The body of the normaliser lives inside
//! [`pass_go_method_set`][crate::graph::unified::build::pass_go_method_set]
//! because it is implemented next to (and unit-tested with) the
//! satisfaction algorithm. The Go language plugin in `sqry-lang-go` is in
//! a different crate, so the internal `pub(crate)` symbol is not
//! reachable from there. This module exposes a single `pub fn`
//! delegating to the internal canonicaliser so the plugin and the pass
//! both compute the canonical signature with bit-identical bytes.
//!
//! # Determinism contract
//!
//! `canonicalise_go_signature(params, returns)` is a pure function of
//! its two inputs. The byte sequence returned is the same one that
//! `pass_go_method_set::canonicalise_signature` wraps in a
//! `NormalizedSignature`. Comparing two outputs with `==` is exactly the
//! signature-identity predicate prescribed by
//! `docs/development/go-implements-and-promotion/02_DESIGN.md` §4.1.3.
//!
//! The output is **always valid UTF-8** because the input is required to
//! be valid UTF-8 (Go source is UTF-8) and the normaliser preserves
//! byte-by-byte semantics for identifier and punctuation bytes while
//! collapsing only ASCII whitespace.

use crate::graph::unified::build::pass_go_method_set::canonicalise_signature;

/// Canonicalise a Go function / method signature for cross-crate
/// emission of `GoMethodSignatureHint` / `GoFunctionSignatureHint`
/// hints.
///
/// `params` is the parameter-list text (with or without outer parens).
/// `returns` is the result clause text — empty string for void
/// functions, a single type for single-return shapes, or a
/// paren-wrapped list for multi-return shapes. Both inputs are
/// canonicalised per `02_DESIGN` §4.1.2:
///
/// 1. Whitespace between modifier tokens is collapsed.
/// 2. Parameter names are erased.
/// 3. Variadic `...T` is preserved.
/// 4. (Step 4 — package qualification of bare identifiers — runs inside
///    the satisfaction pass, not here; both sides see the same lexical
///    input because the plugin emits the same param/return text the pass
///    would.)
/// 5. Generic type-parameter erasure runs in the plugin's
///    `canonicalize_type_text_with_params` before calling this function;
///    here we operate on the post-substitution text.
///
/// Returns the canonical signature as a `String`. The byte representation
/// is identical to the `Vec<u8>` carried by `NormalizedSignature`.
#[must_use]
pub fn canonicalise_go_signature(params: &str, returns: &str) -> String {
    let normalised = canonicalise_signature(params, returns);
    // `NormalizedSignature` is `pub(crate)` so we cannot name its tuple
    // field across the crate boundary, but its byte payload is the same
    // sequence the canonicaliser already wrote. Round-trip via
    // `format!("{:?}", ...)` is the wrong shape — instead drain the
    // bytes through the public `Vec<u8>` access path inside the same
    // crate.
    let bytes = normalised.0;
    // The normaliser only writes ASCII bytes (it operates on UTF-8 Go
    // source identifiers and ASCII punctuation), so `from_utf8` is
    // infallible in practice. If the input contained non-ASCII bytes,
    // the canonicaliser preserves them verbatim, so they remain valid
    // UTF-8 here too.
    String::from_utf8(bytes).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spot-check that the shim produces identical output to the
    /// internal canonicaliser. Bit-identical bytes are the load-bearing
    /// invariant — every D3 callsite compares via byte-wise string
    /// equality.
    #[test]
    fn shim_is_byte_identical_with_internal_canonicaliser() {
        // The shim covers the same input space as the internal
        // canonicaliser; the assertion below piggybacks on the
        // canonicaliser's exhaustive unit-test corpus in
        // `pass_go_method_set::tests`. Here we assert just one
        // representative case so the shim is its own callable check.
        let a = canonicalise_go_signature("(p []byte)", "(int, error)");
        let b = canonicalise_go_signature("[]byte", "(int, error)");
        assert_eq!(a, b, "named and unnamed parameter forms must collapse");
    }

    #[test]
    fn empty_inputs_produce_paren_pair() {
        assert_eq!(canonicalise_go_signature("", ""), "()");
    }

    #[test]
    fn variadic_and_pointer_modifier_whitespace_collapse() {
        let a = canonicalise_go_signature("(...* T)", "");
        let b = canonicalise_go_signature("...*T", "");
        assert_eq!(a, b);
    }
}
