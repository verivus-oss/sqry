//! Identifier-blind per-function body-shape descriptor types (V15+).
//!
//! A [`ShapeDescriptor`] is a deterministic, renaming-invariant fingerprint of a
//! function or method body. It is distinct from `body_hash` (the exact
//! copy-paste hash in `crate::graph::body_hash`): `body_hash` is whitespace-,
//! identifier-, and literal-sensitive by design, whereas the shape descriptor
//! erases identifiers and literals and survives reformatting and renaming.
//!
//! This module holds only the data model (WS1). The shared walker that POPULATES
//! these fields (the AST shingle, the `Weisfeiler-Lehman` relabel, the `MinHash`, and
//! the `shape_hash` itself) lives in `crate::graph::unified::build::shape` (WS2),
//! and the `NodeId`-keyed side table that STORES them hangs off
//! [`super::metadata::NodeMetadataStore`] (WS1 side-table step).
//!
//! # Determinism and on-disk stability
//!
//! Everything here is plain data with a fixed field order so postcard
//! serialization is deterministic across processes and runs (AC-1, AC-8). The
//! three constants below ([`SHAPE_SCHEMA_VERSION`], [`CF_BUCKET_COUNT`],
//! [`MINHASH_LANES`]) are on-disk-affecting: the control-flow bucket count and
//! the `MinHash` lane count are baked into every serialized descriptor, and the
//! hashing seeds (registered in `crate::graph::body_hash`) feed `shape_hash`.
//! Changing any of them changes the bytes of every descriptor, so any such
//! change MUST bump [`SHAPE_SCHEMA_VERSION`], which forces a recompute on load
//! mismatch.

use serde::{Deserialize, Serialize};

/// Schema version for the shape descriptor. Bump on ANY change to the canonical
/// bucket set, the `MinHash` lane count, the shingle/WL algorithm, or the hashing
/// seeds. A load-time mismatch forces descriptors to recompute on reindex.
///
/// This is the shape-side sibling of `INDEX_SCHEMA_VERSION` (the body-hash seed
/// discipline in `crate::graph::body_hash`).
pub const SHAPE_SCHEMA_VERSION: u16 = 1;

/// Number of canonical, language-neutral control-flow buckets. Frozen: the order
/// is the cross-language contract (see `CfBucket` in
/// `crate::graph::unified::build::shape`). Changes are additive-only (append,
/// never reorder) and bump [`SHAPE_SCHEMA_VERSION`].
pub const CF_BUCKET_COUNT: usize = 15;

/// Number of `MinHash` lanes per descriptor. Pinned on disk (it sizes the
/// serialized `minhash` array); retuning it is a [`SHAPE_SCHEMA_VERSION`] bump.
pub const MINHASH_LANES: usize = 64;

/// Minimum token count for a body to be hashable. Bodies below this carry
/// [`ShapeFlags::UNHASHABLE`] instead of a meaningless fingerprint (AC-1).
pub const MIN_HASHABLE_TOKENS: u16 = 4;

/// Renaming-invariant 128-bit structural hash of a function/method body.
///
/// Deliberately mirrors `crate::graph::body_hash::BodyHash128`'s `{ high, low }`
/// layout so the two stay visually and structurally distinct in review (one is
/// the exact copy-paste hash, this is the structural hash) and so the 32-char
/// hex `Display` convention is shared. It is NOT `body_hash`: keeping them
/// separate preserves the `find_duplicates` Body ladder unchanged (AC-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct ShapeHash128 {
    /// High 64 bits (computed with the `SQRYSHP0` seed in WS2).
    pub high: u64,
    /// Low 64 bits (computed with the `SQRYSHP1` seed in WS2).
    pub low: u64,
}

impl ShapeHash128 {
    /// Pack into a single `u128` for comparison and compact storage.
    #[must_use]
    pub const fn as_u128(self) -> u128 {
        ((self.high as u128) << 64) | (self.low as u128)
    }

    /// Unpack from a `u128`.
    #[must_use]
    pub const fn from_u128(value: u128) -> Self {
        Self {
            high: (value >> 64) as u64,
            low: (value & 0xFFFF_FFFF_FFFF_FFFF) as u64,
        }
    }

    /// True when both halves are zero (the never-computed sentinel value carried
    /// by an unhashable descriptor).
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.high == 0 && self.low == 0
    }
}

impl std::fmt::Display for ShapeHash128 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}{:016x}", self.high, self.low)
    }
}

/// Structural signature shape: arities and flags read directly from a function's
/// parameter list, independent of the return-type-only `signature` slot on
/// `NodeEntry` (which is frequently null). Language-neutral.
///
/// The four booleans are independent, orthogonal signature properties (defaults,
/// varargs, kwargs, return annotation); collapsing them into an enum or bitset
/// would obscure the data model, so the `struct_excessive_bools` lint is allowed.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct SignatureShape {
    /// Count of positional parameters.
    pub arity_positional: u16,
    /// Count of keyword-only / named-only parameters (0 in languages without them).
    pub arity_keyword_only: u16,
    /// At least one parameter has a default value.
    pub has_defaults: bool,
    /// A variadic positional parameter is present (`*args`, `...`, rest).
    pub has_varargs: bool,
    /// A variadic keyword parameter is present (`**kwargs`).
    pub has_kwargs: bool,
    /// The function declares a return-type annotation.
    pub has_return_annotation: bool,
}

/// Bucketed callee fan-out shape (DEPENDENT EXTENSION, SPEC §3).
///
/// Always present on a descriptor. It carries [`CalleeShape::Unresolved`] for
/// this effort and is never silently zeroed; a `Resolved` value is populated only
/// once the externally-filed `call-resolution-completeness` gap lands. The
/// `Resolved` arm exists so the on-disk shape is stable when that work arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum CalleeShape {
    /// Call resolution is not complete; the fan-out shape is unknown (not zero).
    #[default]
    Unresolved,
    /// Resolved fan-out: total call-site count plus a small degree histogram.
    Resolved {
        /// Number of outgoing call sites.
        count: u16,
        /// Degree buckets (e.g. by callee fan-in tier); shape pinned for stability.
        degree_buckets: [u16; 4],
    },
}

/// Explicit per-descriptor state markers. A bit set means the descriptor is an
/// honest marker rather than a silently-zeroed fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct ShapeFlags(pub u8);

impl ShapeFlags {
    /// Body had fewer than [`MIN_HASHABLE_TOKENS`] tokens; the fingerprint is not
    /// meaningful (AC-1).
    pub const UNHASHABLE: u8 = 0b0000_0001;
    /// Body exceeded the walker node-count budget; the descriptor is partial
    /// (design §9 over-budget marker).
    pub const TRUNCATED: u8 = 0b0000_0010;

    /// An empty flag set (the common case for a normal, fully-computed descriptor).
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// True when the unhashable marker is set.
    #[must_use]
    pub const fn is_unhashable(self) -> bool {
        self.0 & Self::UNHASHABLE != 0
    }

    /// True when the truncated marker is set.
    #[must_use]
    pub const fn is_truncated(self) -> bool {
        self.0 & Self::TRUNCATED != 0
    }

    /// Set the unhashable marker.
    pub const fn set_unhashable(&mut self) {
        self.0 |= Self::UNHASHABLE;
    }

    /// Set the truncated marker.
    pub const fn set_truncated(&mut self) {
        self.0 |= Self::TRUNCATED;
    }
}

/// Identifier-blind structural fingerprint of one function/method body.
///
/// Deterministic function of the source AST (never the source text); versioned
/// via [`SHAPE_SCHEMA_VERSION`]. Stored in a `NodeId`-keyed side table on
/// `NodeMetadataStore`, kept off the hot-path `NodeEntry` because descriptors are
/// larger and not needed for most traversals.
///
/// Not `Copy`: it carries a [`MINHASH_LANES`]-wide array, so it is cloned (not
/// memcpy'd by value) into snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShapeDescriptor {
    /// Canonical control-flow bucket counts, fixed order (see `CfBucket`).
    pub cf_histogram: [u16; CF_BUCKET_COUNT],
    /// Structural signature shape (arities + flags).
    pub signature_shape: SignatureShape,
    /// Bucketed callee fan-out (dependent extension; `Unresolved` this effort).
    pub callee_shape: CalleeShape,
    /// Renaming-invariant structural hash (NOT `body_hash`).
    pub shape_hash: ShapeHash128,
    /// `MinHash` signature over identifier-erased WL labels, for LSH banding.
    ///
    /// Serialized via [`minhash_serde`] because serde only derives array
    /// `Deserialize` up to length 32; the fixed-tuple encoding is byte-identical
    /// to a native `[u32; MINHASH_LANES]` in postcard.
    #[serde(with = "minhash_serde")]
    pub minhash: [u32; MINHASH_LANES],
    /// Explicit state markers (unhashable / truncated).
    pub flags: ShapeFlags,
}

impl Default for ShapeDescriptor {
    fn default() -> Self {
        // `[u32; MINHASH_LANES]` does not derive `Default` (std only derives it
        // for arrays up to length 32), so construct the whole value by hand.
        Self {
            cf_histogram: [0; CF_BUCKET_COUNT],
            signature_shape: SignatureShape::default(),
            callee_shape: CalleeShape::default(),
            shape_hash: ShapeHash128::default(),
            minhash: [0; MINHASH_LANES],
            flags: ShapeFlags::empty(),
        }
    }
}

impl ShapeDescriptor {
    /// Construct the explicit "unhashable" descriptor for a sub-[`MIN_HASHABLE_TOKENS`]
    /// body: empty histogram and `MinHash`, zero `shape_hash`, the unhashable flag
    /// set, and the given signature shape (which is still meaningful for tiny
    /// bodies). This is an honest marker, never a silently-zeroed descriptor (AC-1).
    #[must_use]
    pub fn unhashable(signature_shape: SignatureShape) -> Self {
        let mut flags = ShapeFlags::empty();
        flags.set_unhashable();
        Self {
            signature_shape,
            flags,
            ..Self::default()
        }
    }

    /// True when this descriptor is the unhashable marker.
    #[must_use]
    pub const fn is_unhashable(&self) -> bool {
        self.flags.is_unhashable()
    }

    /// True when this descriptor was truncated by the walker node-count budget.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.flags.is_truncated()
    }
}

/// Serde adapter for the fixed-size `minhash` array.
///
/// serde only derives `Deserialize` for arrays up to length 32, so a
/// [`MINHASH_LANES`]-wide array needs a hand-written adapter. Serializing as a
/// fixed-length tuple (the technique `serde-big-array` uses) keeps the on-disk
/// encoding byte-identical to a native array under postcard (no length prefix)
/// and produces a plain JSON array under `serde_json`.
mod minhash_serde {
    use super::MINHASH_LANES;
    use serde::de::{self, Deserializer, SeqAccess, Visitor};
    use serde::ser::{SerializeTuple, Serializer};
    use std::fmt;

    /// Serialize the array as a fixed-length tuple of `MINHASH_LANES` elements.
    ///
    /// # Errors
    ///
    /// Propagates the serializer's error if an element cannot be encoded.
    pub fn serialize<S>(value: &[u32; MINHASH_LANES], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut tup = serializer.serialize_tuple(MINHASH_LANES)?;
        for lane in value {
            tup.serialize_element(lane)?;
        }
        tup.end()
    }

    /// Deserialize exactly `MINHASH_LANES` elements back into the fixed array.
    ///
    /// # Errors
    ///
    /// Returns an `invalid_length` error if fewer than `MINHASH_LANES` elements
    /// are present, or any element fails to decode.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u32; MINHASH_LANES], D::Error>
    where
        D: Deserializer<'de>,
    {
        struct LanesVisitor;

        impl<'de> Visitor<'de> for LanesVisitor {
            type Value = [u32; MINHASH_LANES];

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "an array of {MINHASH_LANES} u32 `MinHash` lanes")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut lanes = [0u32; MINHASH_LANES];
                for (i, slot) in lanes.iter_mut().enumerate() {
                    *slot = seq
                        .next_element()?
                        .ok_or_else(|| de::Error::invalid_length(i, &self))?;
                }
                Ok(lanes)
            }
        }

        deserializer.deserialize_tuple(MINHASH_LANES, LanesVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_hash_u128_roundtrip() {
        let h = ShapeHash128 {
            high: 0x1234_5678_9abc_def0,
            low: 0x0fed_cba9_8765_4321,
        };
        assert_eq!(ShapeHash128::from_u128(h.as_u128()), h);
    }

    #[test]
    fn shape_hash_display_is_32_hex() {
        let h = ShapeHash128 {
            high: 0x1234_5678_90ab_cdef,
            low: 0xfedc_ba09_8765_4321,
        };
        let s = format!("{h}");
        assert_eq!(s, "1234567890abcdeffedcba0987654321");
        assert_eq!(s.len(), 32);
    }

    #[test]
    fn shape_hash_zero_sentinel() {
        assert!(ShapeHash128::default().is_zero());
        assert!(!ShapeHash128 { high: 0, low: 1 }.is_zero());
    }

    #[test]
    fn callee_shape_defaults_unresolved() {
        assert_eq!(CalleeShape::default(), CalleeShape::Unresolved);
    }

    #[test]
    fn flags_set_and_query() {
        let mut f = ShapeFlags::empty();
        assert!(!f.is_unhashable() && !f.is_truncated());
        f.set_unhashable();
        assert!(f.is_unhashable() && !f.is_truncated());
        f.set_truncated();
        assert!(f.is_unhashable() && f.is_truncated());
    }

    #[test]
    fn default_descriptor_is_empty_but_present() {
        let d = ShapeDescriptor::default();
        assert_eq!(d.cf_histogram, [0; CF_BUCKET_COUNT]);
        assert_eq!(d.minhash, [0; MINHASH_LANES]);
        assert_eq!(d.callee_shape, CalleeShape::Unresolved);
        assert!(d.shape_hash.is_zero());
        assert!(!d.is_unhashable());
        assert!(!d.is_truncated());
    }

    #[test]
    fn unhashable_constructor_sets_marker_and_keeps_signature() {
        let sig = SignatureShape {
            arity_positional: 2,
            has_return_annotation: true,
            ..SignatureShape::default()
        };
        let d = ShapeDescriptor::unhashable(sig);
        assert!(d.is_unhashable());
        assert_eq!(d.signature_shape, sig);
        assert!(d.shape_hash.is_zero());
        assert_eq!(d.cf_histogram, [0; CF_BUCKET_COUNT]);
    }

    #[test]
    fn descriptor_postcard_roundtrip_with_full_minhash() {
        let mut d = ShapeDescriptor::default();
        for (i, slot) in d.minhash.iter_mut().enumerate() {
            *slot = (i as u32).wrapping_mul(2_654_435_761);
        }
        d.cf_histogram[0] = 7;
        d.cf_histogram[CF_BUCKET_COUNT - 1] = 3;
        d.signature_shape.arity_positional = 4;
        d.signature_shape.has_kwargs = true;
        d.callee_shape = CalleeShape::Resolved {
            count: 9,
            degree_buckets: [1, 2, 3, 4],
        };
        d.shape_hash = ShapeHash128 { high: 11, low: 22 };
        d.flags.set_truncated();

        let bytes = postcard::to_allocvec(&d).expect("serialize");
        let back: ShapeDescriptor = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(d, back);
    }

    #[test]
    fn descriptor_json_roundtrip() {
        let d = ShapeDescriptor::default();
        let json = serde_json::to_string(&d).expect("to json");
        let back: ShapeDescriptor = serde_json::from_str(&json).expect("from json");
        assert_eq!(d, back);
    }

    #[test]
    fn constants_are_frozen() {
        // Guards against accidental edits that would silently break on-disk
        // stability without a SHAPE_SCHEMA_VERSION bump.
        assert_eq!(CF_BUCKET_COUNT, 15);
        assert_eq!(MINHASH_LANES, 64);
        assert_eq!(SHAPE_SCHEMA_VERSION, 1);
        assert_eq!(MIN_HASHABLE_TOKENS, 4);
    }
}
