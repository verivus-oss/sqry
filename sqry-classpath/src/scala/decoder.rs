//! High-level decoder that produces structured Scala metadata from raw
//! `@ScalaSignature` bytes.
//!
//! This module bridges the low-level [`ScalaSignatureReader`] (which parses the
//! binary entry table) and the rest of the classpath pipeline by producing a
//! [`ScalaClassMetadata`] with Scala-specific information: class kind (class /
//! trait / object), visibility, case/sealed/abstract modifiers, and basic
//! superclass/trait hierarchy.

use log::warn;

use crate::stub::model::ScalaSignatureStub;

use super::signature::{
    FLAG_ABSTRACT, FLAG_CASE, FLAG_INTERFACE, FLAG_PRIVATE, FLAG_PROTECTED, FLAG_SEALED,
    FLAG_TRAIT, ScalaSignatureReader, TAG_EXT_MOD_CLASS_REF, TAG_EXT_REF, TAG_MODULE_SYM,
};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The kind of a Scala class-level entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScalaClassKind {
    /// A regular `class`.
    Class,
    /// A `trait`.
    Trait,
    /// An `object` (singleton).
    Object,
    /// A package object (`package object foo`).
    PackageObject,
}

/// Visibility of a Scala symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScalaVisibility {
    /// No access modifier (public by default in Scala).
    Public,
    /// `private` or `private[scope]`.
    Private,
    /// `protected` or `protected[scope]`.
    Protected,
}

/// Decoded Scala metadata for a class/trait/object.
///
/// Produced by [`decode_scala_signature`] from a [`ScalaSignatureStub`].
/// When decoding fails or the format is unsupported, the function returns
/// `None` and the caller falls back to bytecode-only analysis.
#[derive(Debug, Clone)]
pub struct ScalaClassMetadata {
    /// Scala class kind (class, trait, object, package object).
    pub kind: ScalaClassKind,
    /// Visibility modifier.
    pub visibility: ScalaVisibility,
    /// Whether this is a `case class` or `case object`.
    pub is_case: bool,
    /// Whether this is `sealed`.
    pub is_sealed: bool,
    /// Whether this is `abstract`.
    pub is_abstract: bool,
    /// Superclass name (if found in the signature).
    pub superclass: Option<String>,
    /// Mixed-in trait names.
    pub traits: Vec<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Decode Scala metadata from a [`ScalaSignatureStub`].
///
/// Returns `None` if the signature format is unsupported or decoding fails.
/// In those cases the caller should fall back to bytecode-only analysis.
///
/// # Errors
///
/// This function never panics. All malformed data is handled gracefully by
/// returning `None`.
pub fn decode_scala_signature(stub: &ScalaSignatureStub) -> Option<ScalaClassMetadata> {
    // Only support major version 5 (Scala 2.10+).
    if stub.major_version != 5 {
        warn!(
            "unsupported Scala signature major version {} (expected 5)",
            stub.major_version
        );
        return None;
    }

    let reader = ScalaSignatureReader::parse(&stub.bytes)?;

    // Find the primary class/module symbol. The "primary" symbol is the one
    // whose name matches the class file (heuristic: not a companion, not a
    // nested anonymous class). In most cases this is the first CLASSsym or
    // MODULEsym entry.
    let symbols = reader.class_and_module_symbols();
    if symbols.is_empty() {
        warn!("no CLASSsym or MODULEsym entries found in Scala signature");
        return None;
    }

    // Find the best candidate: prefer the first symbol that has a
    // non-anonymous, non-empty name.
    let (primary_index, primary_entry) = find_primary_symbol(&reader, &symbols)?;

    let info = reader.read_symbol_info(primary_entry)?;
    let flags = info.flags;

    // Determine kind.
    let kind = determine_kind(primary_entry.tag, flags, &reader, info.name_index);

    // Determine visibility.
    let visibility = determine_visibility(flags);

    // Extract hierarchy from EXTref / EXTMODCLASSref entries.
    let (superclass, traits) = extract_hierarchy(&reader, primary_index);

    Some(ScalaClassMetadata {
        kind,
        visibility,
        is_case: flags & FLAG_CASE != 0,
        is_sealed: flags & FLAG_SEALED != 0,
        is_abstract: flags & FLAG_ABSTRACT != 0,
        superclass,
        traits,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Find the primary symbol among CLASSsym/MODULEsym entries.
///
/// Returns the index and entry reference, or `None` if no suitable symbol is
/// found.
fn find_primary_symbol<'a>(
    reader: &'a ScalaSignatureReader,
    symbols: &[(usize, &'a super::signature::SignatureEntry)],
) -> Option<(usize, &'a super::signature::SignatureEntry)> {
    // Prefer a symbol with a non-empty, non-anonymous name that is not a
    // compiler-generated artifact (names starting with `$`).
    for &(idx, entry) in symbols {
        if let Some(info) = reader.read_symbol_info(entry)
            && let Some(name) = reader.read_name(info.name_index)
            && !name.is_empty()
            && !name.starts_with('$')
            && !name.contains("$anon")
        {
            return Some((idx, entry));
        }
    }

    // Fall back to the very first symbol if all are anonymous/generated.
    symbols.first().map(|&(idx, entry)| (idx, entry))
}

/// Determine the [`ScalaClassKind`] from the entry tag and flags.
fn determine_kind(
    tag: u8,
    flags: u64,
    reader: &ScalaSignatureReader,
    name_index: usize,
) -> ScalaClassKind {
    // MODULEsym entries are objects.
    if tag == TAG_MODULE_SYM {
        // Check for package object: the name is typically "package".
        if let Some(name) = reader.read_name(name_index)
            && name == "package"
        {
            return ScalaClassKind::PackageObject;
        }
        return ScalaClassKind::Object;
    }

    // CLASSsym with TRAIT flag → trait.
    if flags & FLAG_TRAIT != 0 || flags & FLAG_INTERFACE != 0 {
        return ScalaClassKind::Trait;
    }

    ScalaClassKind::Class
}

/// Determine [`ScalaVisibility`] from flags.
fn determine_visibility(flags: u64) -> ScalaVisibility {
    if flags & FLAG_PRIVATE != 0 {
        ScalaVisibility::Private
    } else if flags & FLAG_PROTECTED != 0 {
        ScalaVisibility::Protected
    } else {
        ScalaVisibility::Public
    }
}

/// Extract superclass and trait names from the signature.
///
/// This is a heuristic approach for Tier 1: we look at EXTref and
/// EXTMODCLASSref entries to find well-known JVM superclass and trait
/// references. The actual parent type information is encoded in the type-info
/// entry, but for Tier 1 we use the simpler approach of scanning external
/// references.
///
/// Returns `(superclass, traits)` where `superclass` is the first non-Object
/// superclass found, and `traits` are mixed-in trait names.
fn extract_hierarchy(
    reader: &ScalaSignatureReader,
    _primary_index: usize,
) -> (Option<String>, Vec<String>) {
    let ext_refs = reader.ext_refs();

    let mut superclass: Option<String> = None;
    let mut traits = Vec::new();

    // Known base types to skip.
    const SKIP_NAMES: &[&str] = &[
        "Object",
        "AnyRef",
        "Any",
        "Serializable",
        "Product",
        "Equals",
        "<empty>",
        "java.lang.Object",
        "scala.AnyRef",
    ];

    for &(_idx, entry) in &ext_refs {
        if entry.tag != TAG_EXT_REF && entry.tag != TAG_EXT_MOD_CLASS_REF {
            continue;
        }

        // Resolve the name via the name_ref in the EXTref data.
        let mut pos = 0;
        let Some(name_ref) = super::signature::read_nat(&entry.data, &mut pos) else {
            continue;
        };
        let Some(name) = reader.read_name(name_ref as usize) else {
            continue;
        };

        // Skip well-known base types that don't provide useful hierarchy info.
        if SKIP_NAMES.contains(&name.as_str()) {
            continue;
        }

        // Skip compiler-generated names.
        if name.starts_with('$') || name.contains("$anon") || name.is_empty() {
            continue;
        }

        // Heuristic: EXTMODCLASSref entries tend to be module/object references
        // while EXTref entries are class/trait references. For Tier 1, we
        // collect all unique non-skipped names.
        if entry.tag == TAG_EXT_REF {
            if superclass.is_none() {
                superclass = Some(name);
            } else if !traits.contains(&name) {
                traits.push(name);
            }
        } else if entry.tag == TAG_EXT_MOD_CLASS_REF && !traits.contains(&name) {
            // Module-class references are typically companion objects or
            // well-known modules. Include them as potential trait references
            // only if they look like trait names.
            traits.push(name);
        }
    }

    (superclass, traits)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scala::signature::{
        FLAG_MODULE, TAG_CLASS_SYM, TAG_EXT_REF, TAG_MODULE_SYM, TAG_NONE_SYM, TAG_TERM_NAME,
        TAG_TYPE_NAME,
    };

    // -- Test helpers -------------------------------------------------------

    /// Encode a u64 as a Nat (variable-length).
    fn encode_nat(mut value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let mut byte = (value & 0x7F) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if value == 0 {
                break;
            }
        }
        bytes
    }

    /// Build a minimal entry: tag + Nat-encoded length + data.
    fn build_entry(tag: u8, data: &[u8]) -> Vec<u8> {
        let mut entry = vec![tag];
        entry.extend(encode_nat(data.len() as u64));
        entry.extend_from_slice(data);
        entry
    }

    /// Build a complete signature with version header + entry count + entries.
    fn build_signature(entries: Vec<Vec<u8>>) -> Vec<u8> {
        let mut buf = vec![5, 0]; // version 5.0
        buf.extend(encode_nat(entries.len() as u64));
        for entry in entries {
            buf.extend(entry);
        }
        buf
    }

    /// Build a symbol info data block.
    fn build_sym_data(name_ref: u64, owner_ref: u64, flags: u64, info_ref: u64) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend(encode_nat(name_ref));
        data.extend(encode_nat(owner_ref));
        data.extend(encode_nat(flags));
        data.extend(encode_nat(info_ref));
        data
    }

    /// Create a `ScalaSignatureStub` from raw entries.
    fn stub_from_entries(entries: Vec<Vec<u8>>) -> ScalaSignatureStub {
        ScalaSignatureStub {
            bytes: build_signature(entries),
            major_version: 5,
            minor_version: 0,
        }
    }

    /// Build a minimal stub with a single CLASSsym with the given name and flags.
    fn simple_class_stub(name: &str, flags: u64) -> ScalaSignatureStub {
        let name_entry = build_entry(TAG_TYPE_NAME, name.as_bytes());
        let owner = build_entry(TAG_NONE_SYM, &[]);
        let sym_data = build_sym_data(0, 1, flags, 0);
        let cls = build_entry(TAG_CLASS_SYM, &sym_data);
        stub_from_entries(vec![name_entry, owner, cls])
    }

    /// Build a minimal stub with a single MODULEsym with the given name and flags.
    fn simple_module_stub(name: &str, flags: u64) -> ScalaSignatureStub {
        let name_entry = build_entry(TAG_TERM_NAME, name.as_bytes());
        let owner = build_entry(TAG_NONE_SYM, &[]);
        let sym_data = build_sym_data(0, 1, flags, 0);
        let module = build_entry(TAG_MODULE_SYM, &sym_data);
        stub_from_entries(vec![name_entry, owner, module])
    }

    // -- Kind detection tests -----------------------------------------------

    #[test]
    fn detect_trait() {
        let stub = simple_class_stub("Functor", FLAG_TRAIT | FLAG_INTERFACE | FLAG_ABSTRACT);
        let meta = decode_scala_signature(&stub).unwrap();
        assert_eq!(meta.kind, ScalaClassKind::Trait);
        assert!(meta.is_abstract);
    }

    #[test]
    fn detect_trait_via_interface_flag_only() {
        // Some Scala versions only set INTERFACE without TRAIT.
        let stub = simple_class_stub("Showable", FLAG_INTERFACE | FLAG_ABSTRACT);
        let meta = decode_scala_signature(&stub).unwrap();
        assert_eq!(meta.kind, ScalaClassKind::Trait);
    }

    #[test]
    fn detect_object() {
        let stub = simple_module_stub("Config", FLAG_MODULE);
        let meta = decode_scala_signature(&stub).unwrap();
        assert_eq!(meta.kind, ScalaClassKind::Object);
        assert!(!meta.is_case);
    }

    #[test]
    fn detect_package_object() {
        let stub = simple_module_stub("package", FLAG_MODULE);
        let meta = decode_scala_signature(&stub).unwrap();
        assert_eq!(meta.kind, ScalaClassKind::PackageObject);
    }

    #[test]
    fn detect_regular_class() {
        let stub = simple_class_stub("MyService", 0);
        let meta = decode_scala_signature(&stub).unwrap();
        assert_eq!(meta.kind, ScalaClassKind::Class);
        assert!(!meta.is_case);
        assert!(!meta.is_sealed);
        assert!(!meta.is_abstract);
    }

    // -- Case class / object tests ------------------------------------------

    #[test]
    fn detect_case_class() {
        let stub = simple_class_stub("Point", FLAG_CASE);
        let meta = decode_scala_signature(&stub).unwrap();
        assert_eq!(meta.kind, ScalaClassKind::Class);
        assert!(meta.is_case);
    }

    #[test]
    fn detect_case_object() {
        let stub = simple_module_stub("Nil", FLAG_MODULE | FLAG_CASE);
        let meta = decode_scala_signature(&stub).unwrap();
        assert_eq!(meta.kind, ScalaClassKind::Object);
        assert!(meta.is_case);
    }

    // -- Sealed tests -------------------------------------------------------

    #[test]
    fn detect_sealed_trait() {
        let stub = simple_class_stub(
            "Option",
            FLAG_SEALED | FLAG_ABSTRACT | FLAG_TRAIT | FLAG_INTERFACE,
        );
        let meta = decode_scala_signature(&stub).unwrap();
        assert_eq!(meta.kind, ScalaClassKind::Trait);
        assert!(meta.is_sealed);
        assert!(meta.is_abstract);
    }

    #[test]
    fn detect_sealed_class() {
        let stub = simple_class_stub("Expr", FLAG_SEALED | FLAG_ABSTRACT);
        let meta = decode_scala_signature(&stub).unwrap();
        assert_eq!(meta.kind, ScalaClassKind::Class);
        assert!(meta.is_sealed);
        assert!(meta.is_abstract);
    }

    // -- Visibility tests ---------------------------------------------------

    #[test]
    fn detect_public_visibility() {
        let stub = simple_class_stub("Public", 0);
        let meta = decode_scala_signature(&stub).unwrap();
        assert_eq!(meta.visibility, ScalaVisibility::Public);
    }

    #[test]
    fn detect_private_visibility() {
        let stub = simple_class_stub("Private", FLAG_PRIVATE);
        let meta = decode_scala_signature(&stub).unwrap();
        assert_eq!(meta.visibility, ScalaVisibility::Private);
    }

    #[test]
    fn detect_protected_visibility() {
        let stub = simple_class_stub("Protected", FLAG_PROTECTED);
        let meta = decode_scala_signature(&stub).unwrap();
        assert_eq!(meta.visibility, ScalaVisibility::Protected);
    }

    // -- Abstract class tests -----------------------------------------------

    #[test]
    fn detect_abstract_class() {
        let stub = simple_class_stub("AbstractBase", FLAG_ABSTRACT);
        let meta = decode_scala_signature(&stub).unwrap();
        assert_eq!(meta.kind, ScalaClassKind::Class);
        assert!(meta.is_abstract);
        assert!(!meta.is_sealed);
        assert!(!meta.is_case);
    }

    // -- Hierarchy extraction tests -----------------------------------------

    #[test]
    fn extract_superclass_from_ext_ref() {
        // Build a signature with a class that has an EXTref to a superclass.
        let class_name = build_entry(TAG_TYPE_NAME, b"Child");
        let owner = build_entry(TAG_NONE_SYM, &[]);
        let sym_data = build_sym_data(0, 1, 0, 0);
        let cls = build_entry(TAG_CLASS_SYM, &sym_data);

        // Add EXTref for superclass "Parent".
        let parent_name = build_entry(TAG_TERM_NAME, b"Parent");
        let mut ext_data = Vec::new();
        ext_data.extend(encode_nat(3)); // name_ref → "Parent" at index 3
        let ext = build_entry(TAG_EXT_REF, &ext_data);

        let stub = stub_from_entries(vec![class_name, owner, cls, parent_name, ext]);
        let meta = decode_scala_signature(&stub).unwrap();

        assert_eq!(meta.superclass, Some("Parent".to_string()));
    }

    #[test]
    fn skip_object_and_anyref_in_hierarchy() {
        let class_name = build_entry(TAG_TYPE_NAME, b"Foo");
        let owner = build_entry(TAG_NONE_SYM, &[]);
        let sym_data = build_sym_data(0, 1, 0, 0);
        let cls = build_entry(TAG_CLASS_SYM, &sym_data);

        // EXTrefs for "Object" and "AnyRef" should be skipped.
        let obj_name = build_entry(TAG_TERM_NAME, b"Object");
        let mut ext1_data = Vec::new();
        ext1_data.extend(encode_nat(3));
        let ext1 = build_entry(TAG_EXT_REF, &ext1_data);

        let anyref_name = build_entry(TAG_TERM_NAME, b"AnyRef");
        let mut ext2_data = Vec::new();
        ext2_data.extend(encode_nat(5));
        let ext2 = build_entry(TAG_EXT_REF, &ext2_data);

        let stub = stub_from_entries(vec![
            class_name,
            owner,
            cls,
            obj_name,
            ext1,
            anyref_name,
            ext2,
        ]);
        let meta = decode_scala_signature(&stub).unwrap();

        // No useful superclass found — all are skipped.
        assert_eq!(meta.superclass, None);
    }

    // -- Error handling tests -----------------------------------------------

    #[test]
    fn unsupported_major_version_returns_none() {
        let stub = ScalaSignatureStub {
            bytes: vec![5, 0, 0], // version 5.0, 0 entries
            major_version: 4,     // unsupported
            minor_version: 0,
        };
        assert!(decode_scala_signature(&stub).is_none());
    }

    #[test]
    fn malformed_bytes_returns_none() {
        let stub = ScalaSignatureStub {
            bytes: vec![5, 0, 1], // claims 1 entry but no entry data
            major_version: 5,
            minor_version: 0,
        };
        // The reader should fail to parse the incomplete entry table.
        assert!(decode_scala_signature(&stub).is_none());
    }

    #[test]
    fn empty_bytes_returns_none() {
        let stub = ScalaSignatureStub {
            bytes: vec![],
            major_version: 5,
            minor_version: 0,
        };
        assert!(decode_scala_signature(&stub).is_none());
    }

    #[test]
    fn no_class_symbols_returns_none() {
        // A signature with only name entries and no CLASSsym/MODULEsym.
        let name = build_entry(TAG_TYPE_NAME, b"Foo");
        let stub = stub_from_entries(vec![name]);
        assert!(decode_scala_signature(&stub).is_none());
    }

    #[test]
    fn anonymous_class_skipped_for_primary() {
        // If all symbols are anonymous, we still return Some with the first one.
        let anon_name = build_entry(TAG_TYPE_NAME, b"$anon$1");
        let owner = build_entry(TAG_NONE_SYM, &[]);
        let sym_data = build_sym_data(0, 1, 0, 0);
        let cls = build_entry(TAG_CLASS_SYM, &sym_data);

        let stub = stub_from_entries(vec![anon_name, owner, cls]);
        // Should still decode (falls back to first symbol).
        let meta = decode_scala_signature(&stub);
        assert!(meta.is_some());
    }

    // -- Combined modifier tests --------------------------------------------

    #[test]
    fn sealed_abstract_case_class() {
        // This is unusual but valid: a sealed abstract case class.
        let stub = simple_class_stub("Weird", FLAG_SEALED | FLAG_ABSTRACT | FLAG_CASE);
        let meta = decode_scala_signature(&stub).unwrap();
        assert_eq!(meta.kind, ScalaClassKind::Class);
        assert!(meta.is_sealed);
        assert!(meta.is_abstract);
        assert!(meta.is_case);
    }

    #[test]
    fn private_sealed_trait() {
        let stub = simple_class_stub(
            "Internal",
            FLAG_PRIVATE | FLAG_SEALED | FLAG_TRAIT | FLAG_INTERFACE | FLAG_ABSTRACT,
        );
        let meta = decode_scala_signature(&stub).unwrap();
        assert_eq!(meta.kind, ScalaClassKind::Trait);
        assert_eq!(meta.visibility, ScalaVisibility::Private);
        assert!(meta.is_sealed);
        assert!(meta.is_abstract);
    }
}
