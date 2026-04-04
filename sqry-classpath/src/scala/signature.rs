//! Binary format reader for the Scala 2 signature format.
//!
//! The `@ScalaSignature` annotation embeds a binary-encoded table of symbol
//! entries containing Scala-specific type information. This module parses the
//! entry table from the raw bytes and provides accessors for individual entries.
//!
//! # Format overview
//!
//! The Scala 2 signature format (version 5.x) consists of:
//! 1. A 2-byte header: major version (1 byte) + minor version (1 byte)
//! 2. A `Nat`-encoded entry count
//! 3. A sequence of tagged entries: `tag (1 byte) | length (Nat) | data[length]`
//!
//! `Nat` is a variable-length encoding where each byte contributes 7 bits and
//! the high bit indicates continuation (similar to LEB128 unsigned).

use log::warn;

// ---------------------------------------------------------------------------
// Entry tags (Scala 2 signature format)
// ---------------------------------------------------------------------------

/// Name entry containing UTF-8 bytes (term-level name).
pub const TAG_TERM_NAME: u8 = 1;
/// Name entry containing UTF-8 bytes (type-level name).
pub const TAG_TYPE_NAME: u8 = 2;
/// Empty symbol (no data).
pub const TAG_NONE_SYM: u8 = 3;
/// Type symbol.
pub const TAG_TYPE_SYM: u8 = 4;
/// Type alias symbol.
pub const TAG_ALIAS_SYM: u8 = 5;
/// Class symbol.
pub const TAG_CLASS_SYM: u8 = 6;
/// Module (object) symbol.
pub const TAG_MODULE_SYM: u8 = 7;
/// Value symbol.
pub const TAG_VAL_SYM: u8 = 8;
/// External reference (name + optional owner).
pub const TAG_EXT_REF: u8 = 9;
/// External module-class reference (name + optional owner).
pub const TAG_EXT_MOD_CLASS_REF: u8 = 10;

// ---------------------------------------------------------------------------
// Symbol flags (relevant bits for Tier 1)
// ---------------------------------------------------------------------------

/// Bit 2: `private` visibility.
pub const FLAG_PRIVATE: u64 = 1 << 2;
/// Bit 3: `protected` visibility.
pub const FLAG_PROTECTED: u64 = 1 << 3;
/// Bit 5: `sealed` modifier.
pub const FLAG_SEALED: u64 = 1 << 5;
/// Bit 7: `case` modifier (case class / case object).
pub const FLAG_CASE: u64 = 1 << 7;
/// Bit 8: `abstract` modifier.
pub const FLAG_ABSTRACT: u64 = 1 << 8;
/// Bit 11: module (object) flag.
pub const FLAG_MODULE: u64 = 1 << 11;
/// Bit 13: interface flag (trait from JVM perspective).
pub const FLAG_INTERFACE: u64 = 1 << 13;
/// Bit 36: actual trait flag.
pub const FLAG_TRAIT: u64 = 1 << 36;

// ---------------------------------------------------------------------------
// SignatureEntry
// ---------------------------------------------------------------------------

/// A single entry in the Scala signature entry table.
#[derive(Debug, Clone)]
pub struct SignatureEntry {
    /// Tag byte identifying the entry kind.
    pub tag: u8,
    /// Raw data bytes for this entry (excluding tag and length).
    pub data: Vec<u8>,
}

// ---------------------------------------------------------------------------
// ScalaSymbolInfo
// ---------------------------------------------------------------------------

/// Decoded symbol info from a CLASSsym or MODULEsym entry.
#[derive(Debug, Clone)]
pub struct ScalaSymbolInfo {
    /// Index of the name entry in the entry table.
    pub name_index: usize,
    /// Index of the owning symbol entry.
    pub owner_index: usize,
    /// Symbol flags (visibility, modifiers, etc.).
    pub flags: u64,
    /// Index of the type-info entry.
    pub info_index: usize,
}

// ---------------------------------------------------------------------------
// ScalaSignatureReader
// ---------------------------------------------------------------------------

/// Reader for the Scala 2 signature binary format.
///
/// Parses the binary entry table from raw `@ScalaSignature` bytes and provides
/// accessors for individual entries, name resolution, and symbol info extraction.
#[derive(Debug)]
pub struct ScalaSignatureReader {
    /// Parsed entry table.
    entries: Vec<SignatureEntry>,
}

impl ScalaSignatureReader {
    /// Parse a Scala signature from raw bytes.
    ///
    /// Returns `None` if the bytes are too short, the version is unsupported,
    /// or the entry table is malformed.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        // Need at least 2 bytes for the version header.
        if bytes.len() < 2 {
            warn!("scala signature too short ({} bytes)", bytes.len());
            return None;
        }

        let major = bytes[0];
        let minor = bytes[1];

        // We support major version 5 (Scala 2.10+).
        if major != 5 {
            warn!("unsupported Scala signature version {major}.{minor} (expected 5.x)");
            return None;
        }

        let mut pos = 2;

        // Read the entry count.
        let entry_count = read_nat(bytes, &mut pos)? as usize;

        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let entry = read_entry(bytes, &mut pos)?;
            entries.push(entry);
        }

        Some(Self { entries })
    }

    /// Return the number of entries in the table.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Get entry at the given index.
    #[must_use]
    pub fn entry(&self, index: usize) -> Option<&SignatureEntry> {
        self.entries.get(index)
    }

    /// Read a name from a TERMname or TYPEname entry.
    ///
    /// Returns `None` if the index is out of bounds or the entry is not a name.
    #[must_use]
    pub fn read_name(&self, index: usize) -> Option<String> {
        let entry = self.entry(index)?;
        if entry.tag != TAG_TERM_NAME && entry.tag != TAG_TYPE_NAME {
            return None;
        }
        String::from_utf8(entry.data.clone()).ok()
    }

    /// Read symbol info from a CLASSsym or MODULEsym entry.
    ///
    /// The symbol info format is:
    /// ```text
    /// name_ref (Nat) | owner_ref (Nat) | flags (LongNat) | [private_within_ref (Nat)] | info_ref (Nat)
    /// ```
    ///
    /// Returns `None` if the entry data is malformed.
    #[must_use]
    pub fn read_symbol_info(&self, entry: &SignatureEntry) -> Option<ScalaSymbolInfo> {
        if entry.tag != TAG_CLASS_SYM && entry.tag != TAG_MODULE_SYM {
            return None;
        }
        parse_symbol_info(&entry.data)
    }

    /// Resolve the fully qualified name of a symbol by walking the owner chain.
    ///
    /// Returns `None` if the chain cannot be resolved (e.g., missing entries).
    #[must_use]
    pub fn resolve_qualified_name(&self, sym_index: usize) -> Option<String> {
        let entry = self.entry(sym_index)?;
        let info = self.read_symbol_info(entry)?;
        let name = self.read_name(info.name_index)?;

        // Walk the owner chain to build segments.
        let mut segments = vec![name];
        let mut current_owner = info.owner_index;

        // Limit depth to prevent infinite loops on malformed data.
        const MAX_DEPTH: usize = 128;
        for _ in 0..MAX_DEPTH {
            let owner_entry = match self.entry(current_owner) {
                Some(e) => e,
                None => break,
            };

            match owner_entry.tag {
                TAG_CLASS_SYM | TAG_MODULE_SYM => {
                    if let Some(owner_info) = self.read_symbol_info(owner_entry) {
                        if let Some(owner_name) = self.read_name(owner_info.name_index) {
                            segments.push(owner_name);
                            current_owner = owner_info.owner_index;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                TAG_EXT_REF | TAG_EXT_MOD_CLASS_REF => {
                    if let Some(ext_name) = self.read_ext_ref_name(owner_entry) {
                        // Skip the root `<empty>` package marker.
                        if ext_name != "<empty>" {
                            segments.push(ext_name);
                        }
                    }
                    break;
                }
                TAG_NONE_SYM => break,
                _ => break,
            }
        }

        segments.reverse();
        Some(segments.join("."))
    }

    /// Read the name from an EXTref or EXTMODCLASSref entry.
    ///
    /// Format: `name_ref (Nat) [owner_ref (Nat)]`
    #[must_use]
    fn read_ext_ref_name(&self, entry: &SignatureEntry) -> Option<String> {
        if entry.tag != TAG_EXT_REF && entry.tag != TAG_EXT_MOD_CLASS_REF {
            return None;
        }
        let mut pos = 0;
        let name_index = read_nat(&entry.data, &mut pos)? as usize;
        self.read_name(name_index)
    }

    /// Read the owner index from an EXTref or EXTMODCLASSref entry.
    ///
    /// Returns `None` if there is no owner reference (only a name reference)
    /// or the entry is not an external reference.
    #[must_use]
    pub fn read_ext_ref_owner(&self, entry: &SignatureEntry) -> Option<usize> {
        if entry.tag != TAG_EXT_REF && entry.tag != TAG_EXT_MOD_CLASS_REF {
            return None;
        }
        let mut pos = 0;
        let _name_index = read_nat(&entry.data, &mut pos)?;
        // Owner is optional — only present if there are remaining bytes.
        if pos < entry.data.len() {
            Some(read_nat(&entry.data, &mut pos)? as usize)
        } else {
            None
        }
    }

    /// Find all CLASSsym and MODULEsym entries and their indices.
    pub fn class_and_module_symbols(&self) -> Vec<(usize, &SignatureEntry)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.tag == TAG_CLASS_SYM || e.tag == TAG_MODULE_SYM)
            .collect()
    }

    /// Find all EXTref and EXTMODCLASSref entries and their indices.
    pub fn ext_refs(&self) -> Vec<(usize, &SignatureEntry)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.tag == TAG_EXT_REF || e.tag == TAG_EXT_MOD_CLASS_REF)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Nat / LongNat encoding
// ---------------------------------------------------------------------------

/// Read a `Nat` (variable-length natural number) from a byte slice.
///
/// Each byte contributes 7 bits of value. The high bit (0x80) indicates that
/// more bytes follow. Returns `None` if the data is truncated.
pub fn read_nat(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;

    loop {
        if *pos >= data.len() {
            return None;
        }
        let byte = data[*pos];
        *pos += 1;

        // Accumulate the low 7 bits.
        let value = u64::from(byte & 0x7F);

        // Guard against overflow (Nat should fit in u64 with up to 10 bytes).
        result = result.checked_add(value.checked_shl(shift)?)?;
        shift += 7;

        // If the high bit is clear, we're done.
        if byte & 0x80 == 0 {
            return Some(result);
        }

        // Safety: 10 bytes can encode at most 70 bits; u64 is 64 bits.
        if shift > 63 {
            return None;
        }
    }
}

/// Read a `LongNat` (variable-length long natural) from a byte slice.
///
/// This is identical to `read_nat` in wire format but semantically represents
/// a `Long`-sized value. The encoding is the same.
pub fn read_long_nat(data: &[u8], pos: &mut usize) -> Option<u64> {
    read_nat(data, pos)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Read a single entry from the byte stream.
fn read_entry(data: &[u8], pos: &mut usize) -> Option<SignatureEntry> {
    if *pos >= data.len() {
        return None;
    }
    let tag = data[*pos];
    *pos += 1;

    let length = read_nat(data, pos)? as usize;

    // Bounds check.
    if *pos + length > data.len() {
        return None;
    }

    let entry_data = data[*pos..*pos + length].to_vec();
    *pos += length;

    Some(SignatureEntry {
        tag,
        data: entry_data,
    })
}

/// Parse symbol info fields from raw entry data.
///
/// Format: `name_ref (Nat) | owner_ref (Nat) | flags (LongNat) | [private_within_ref (Nat)] | info_ref (Nat)`
///
/// The `private_within_ref` field is present when the PRIVATE or PROTECTED flag
/// has a qualifier (e.g., `private[pkg]`). We detect its presence by reading
/// all remaining Nats after flags and taking the last one as `info_ref`.
fn parse_symbol_info(data: &[u8]) -> Option<ScalaSymbolInfo> {
    let mut pos = 0;
    let name_index = read_nat(data, &mut pos)? as usize;
    let owner_index = read_nat(data, &mut pos)? as usize;
    let flags = read_long_nat(data, &mut pos)?;

    // After flags, the remaining data contains either:
    //   info_ref                        (no private_within qualifier)
    //   private_within_ref | info_ref   (with qualifier)
    //
    // We read all remaining Nats and pick the last one as info_ref.
    let mut remaining_nats = Vec::new();
    while pos < data.len() {
        match read_nat(data, &mut pos) {
            Some(v) => remaining_nats.push(v as usize),
            None => break,
        }
    }

    // At minimum we need info_ref.
    let info_index = remaining_nats.pop().unwrap_or(0);

    Some(ScalaSymbolInfo {
        name_index,
        owner_index,
        flags,
        info_index,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Nat encoding helpers -----------------------------------------------

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

    // -- Nat encoding/decoding tests ----------------------------------------

    #[test]
    fn nat_single_byte() {
        let data = [42];
        let mut pos = 0;
        assert_eq!(read_nat(&data, &mut pos), Some(42));
        assert_eq!(pos, 1);
    }

    #[test]
    fn nat_zero() {
        let data = [0];
        let mut pos = 0;
        assert_eq!(read_nat(&data, &mut pos), Some(0));
        assert_eq!(pos, 1);
    }

    #[test]
    fn nat_max_single_byte() {
        let data = [127];
        let mut pos = 0;
        assert_eq!(read_nat(&data, &mut pos), Some(127));
        assert_eq!(pos, 1);
    }

    #[test]
    fn nat_two_bytes() {
        // 128 = 0x80 → encoded as [0x80, 0x01]
        // byte 0: 0x80 → low 7 bits = 0, continuation
        // byte 1: 0x01 → 1 << 7 = 128
        let data = [0x80, 0x01];
        let mut pos = 0;
        assert_eq!(read_nat(&data, &mut pos), Some(128));
        assert_eq!(pos, 2);
    }

    #[test]
    fn nat_multi_byte_300() {
        // 300 = 0x12C → low 7 bits = 0x2C (44), remaining = 2
        // encoded as [0xAC, 0x02]
        let data = [0xAC, 0x02];
        let mut pos = 0;
        assert_eq!(read_nat(&data, &mut pos), Some(300));
        assert_eq!(pos, 2);
    }

    #[test]
    fn nat_round_trip() {
        for value in [0, 1, 127, 128, 255, 300, 16383, 16384, 65535, 1_000_000] {
            let encoded = encode_nat(value);
            let mut pos = 0;
            assert_eq!(
                read_nat(&encoded, &mut pos),
                Some(value),
                "round-trip failed for {value}"
            );
            assert_eq!(pos, encoded.len());
        }
    }

    #[test]
    fn nat_truncated_returns_none() {
        // Continuation byte with no follow-up.
        let data = [0x80];
        let mut pos = 0;
        assert_eq!(read_nat(&data, &mut pos), None);
    }

    #[test]
    fn nat_empty_returns_none() {
        let data: [u8; 0] = [];
        let mut pos = 0;
        assert_eq!(read_nat(&data, &mut pos), None);
    }

    // -- Entry table parsing tests ------------------------------------------

    #[test]
    fn parse_empty_signature() {
        let sig = build_signature(vec![]);
        let reader = ScalaSignatureReader::parse(&sig).unwrap();
        assert_eq!(reader.entry_count(), 0);
    }

    #[test]
    fn parse_name_entries() {
        let name_data = b"MyClass".to_vec();
        let name_entry = build_entry(TAG_TYPE_NAME, &name_data);

        let sig = build_signature(vec![name_entry]);
        let reader = ScalaSignatureReader::parse(&sig).unwrap();

        assert_eq!(reader.entry_count(), 1);
        assert_eq!(reader.read_name(0), Some("MyClass".to_string()));
    }

    #[test]
    fn parse_term_name() {
        let name_data = b"myVal".to_vec();
        let name_entry = build_entry(TAG_TERM_NAME, &name_data);

        let sig = build_signature(vec![name_entry]);
        let reader = ScalaSignatureReader::parse(&sig).unwrap();

        assert_eq!(reader.read_name(0), Some("myVal".to_string()));
    }

    #[test]
    fn read_name_wrong_tag_returns_none() {
        let entry = build_entry(TAG_CLASS_SYM, b"data");
        let sig = build_signature(vec![entry]);
        let reader = ScalaSignatureReader::parse(&sig).unwrap();

        assert_eq!(reader.read_name(0), None);
    }

    #[test]
    fn read_name_out_of_bounds_returns_none() {
        let sig = build_signature(vec![]);
        let reader = ScalaSignatureReader::parse(&sig).unwrap();
        assert_eq!(reader.read_name(0), None);
    }

    #[test]
    fn parse_class_sym_entry() {
        // Build: name entry at 0, owner (NONEsym) at 1, CLASSsym at 2.
        let name = build_entry(TAG_TYPE_NAME, b"Point");
        let owner = build_entry(TAG_NONE_SYM, &[]);

        // CLASSsym data: name_ref=0, owner_ref=1, flags=CASE(bit7)=128, info_ref=0
        let mut sym_data = Vec::new();
        sym_data.extend(encode_nat(0)); // name_ref
        sym_data.extend(encode_nat(1)); // owner_ref
        sym_data.extend(encode_nat(FLAG_CASE)); // flags
        sym_data.extend(encode_nat(0)); // info_ref
        let class_sym = build_entry(TAG_CLASS_SYM, &sym_data);

        let sig = build_signature(vec![name, owner, class_sym]);
        let reader = ScalaSignatureReader::parse(&sig).unwrap();

        let entry = reader.entry(2).unwrap();
        assert_eq!(entry.tag, TAG_CLASS_SYM);

        let info = reader.read_symbol_info(entry).unwrap();
        assert_eq!(info.name_index, 0);
        assert_eq!(info.owner_index, 1);
        assert_eq!(info.flags & FLAG_CASE, FLAG_CASE);
        assert_eq!(reader.read_name(info.name_index), Some("Point".to_string()));
    }

    #[test]
    fn parse_module_sym_entry() {
        let name = build_entry(TAG_TERM_NAME, b"Config");
        let owner = build_entry(TAG_NONE_SYM, &[]);

        let mut sym_data = Vec::new();
        sym_data.extend(encode_nat(0)); // name_ref
        sym_data.extend(encode_nat(1)); // owner_ref
        sym_data.extend(encode_nat(FLAG_MODULE)); // flags (MODULE)
        sym_data.extend(encode_nat(0)); // info_ref
        let mod_sym = build_entry(TAG_MODULE_SYM, &sym_data);

        let sig = build_signature(vec![name, owner, mod_sym]);
        let reader = ScalaSignatureReader::parse(&sig).unwrap();

        let entry = reader.entry(2).unwrap();
        assert_eq!(entry.tag, TAG_MODULE_SYM);

        let info = reader.read_symbol_info(entry).unwrap();
        assert_eq!(info.flags & FLAG_MODULE, FLAG_MODULE);
    }

    #[test]
    fn class_and_module_symbols_finds_all() {
        let name1 = build_entry(TAG_TYPE_NAME, b"A");
        let name2 = build_entry(TAG_TERM_NAME, b"B");
        let owner = build_entry(TAG_NONE_SYM, &[]);

        let mut cls_data = Vec::new();
        cls_data.extend(encode_nat(0));
        cls_data.extend(encode_nat(2));
        cls_data.extend(encode_nat(0));
        cls_data.extend(encode_nat(0));
        let cls = build_entry(TAG_CLASS_SYM, &cls_data);

        let mut mod_data = Vec::new();
        mod_data.extend(encode_nat(1));
        mod_data.extend(encode_nat(2));
        mod_data.extend(encode_nat(0));
        mod_data.extend(encode_nat(0));
        let module = build_entry(TAG_MODULE_SYM, &mod_data);

        let sig = build_signature(vec![name1, name2, owner, cls, module]);
        let reader = ScalaSignatureReader::parse(&sig).unwrap();

        let symbols = reader.class_and_module_symbols();
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].0, 3); // CLASSsym
        assert_eq!(symbols[1].0, 4); // MODULEsym
    }

    #[test]
    fn ext_ref_name_resolution() {
        let name = build_entry(TAG_TERM_NAME, b"scala");
        let mut ext_data = Vec::new();
        ext_data.extend(encode_nat(0)); // name_ref
        let ext = build_entry(TAG_EXT_REF, &ext_data);

        let sig = build_signature(vec![name, ext]);
        let reader = ScalaSignatureReader::parse(&sig).unwrap();

        let ext_entry = reader.entry(1).unwrap();
        assert_eq!(
            reader.read_ext_ref_name(ext_entry),
            Some("scala".to_string())
        );
    }

    #[test]
    fn ext_ref_with_owner() {
        let name = build_entry(TAG_TERM_NAME, b"Option");
        let owner_name = build_entry(TAG_TERM_NAME, b"scala");
        let mut owner_ext_data = Vec::new();
        owner_ext_data.extend(encode_nat(1)); // name_ref → "scala"
        let owner_ext = build_entry(TAG_EXT_MOD_CLASS_REF, &owner_ext_data);

        let mut ext_data = Vec::new();
        ext_data.extend(encode_nat(0)); // name_ref → "Option"
        ext_data.extend(encode_nat(2)); // owner_ref → ext at index 2
        let ext = build_entry(TAG_EXT_REF, &ext_data);

        let sig = build_signature(vec![name, owner_name, owner_ext, ext]);
        let reader = ScalaSignatureReader::parse(&sig).unwrap();

        let entry = reader.entry(3).unwrap();
        assert_eq!(reader.read_ext_ref_owner(entry), Some(2));
    }

    // -- Version handling ---------------------------------------------------

    #[test]
    fn unsupported_major_version_returns_none() {
        let mut sig = build_signature(vec![]);
        sig[0] = 4; // wrong major version
        assert!(ScalaSignatureReader::parse(&sig).is_none());
    }

    #[test]
    fn too_short_returns_none() {
        assert!(ScalaSignatureReader::parse(&[5]).is_none());
        assert!(ScalaSignatureReader::parse(&[]).is_none());
    }

    // -- Malformed data handling --------------------------------------------

    #[test]
    fn truncated_entry_returns_none() {
        // Header + entry count = 1, but the entry data is truncated.
        let mut data = vec![5, 0]; // version
        data.extend(encode_nat(1)); // 1 entry
        data.push(TAG_TYPE_NAME); // tag
        data.extend(encode_nat(100)); // length = 100 (but no data follows)
        assert!(ScalaSignatureReader::parse(&data).is_none());
    }

    #[test]
    fn symbol_info_from_non_symbol_returns_none() {
        let name = build_entry(TAG_TYPE_NAME, b"Foo");
        let sig = build_signature(vec![name]);
        let reader = ScalaSignatureReader::parse(&sig).unwrap();

        let entry = reader.entry(0).unwrap();
        assert!(reader.read_symbol_info(entry).is_none());
    }

    // -- Flags tests --------------------------------------------------------

    #[test]
    fn trait_flag_detection() {
        let name = build_entry(TAG_TYPE_NAME, b"Functor");
        let owner = build_entry(TAG_NONE_SYM, &[]);

        // Traits have both INTERFACE and TRAIT flags set, plus ABSTRACT.
        let flags = FLAG_TRAIT | FLAG_INTERFACE | FLAG_ABSTRACT;
        let mut sym_data = Vec::new();
        sym_data.extend(encode_nat(0)); // name_ref
        sym_data.extend(encode_nat(1)); // owner_ref
        sym_data.extend(encode_nat(flags)); // flags
        sym_data.extend(encode_nat(0)); // info_ref
        let cls = build_entry(TAG_CLASS_SYM, &sym_data);

        let sig = build_signature(vec![name, owner, cls]);
        let reader = ScalaSignatureReader::parse(&sig).unwrap();

        let entry = reader.entry(2).unwrap();
        let info = reader.read_symbol_info(entry).unwrap();
        assert_ne!(info.flags & FLAG_TRAIT, 0);
        assert_ne!(info.flags & FLAG_INTERFACE, 0);
        assert_ne!(info.flags & FLAG_ABSTRACT, 0);
    }

    #[test]
    fn sealed_flag_detection() {
        let name = build_entry(TAG_TYPE_NAME, b"Expr");
        let owner = build_entry(TAG_NONE_SYM, &[]);

        let flags = FLAG_SEALED | FLAG_ABSTRACT | FLAG_TRAIT | FLAG_INTERFACE;
        let mut sym_data = Vec::new();
        sym_data.extend(encode_nat(0));
        sym_data.extend(encode_nat(1));
        sym_data.extend(encode_nat(flags));
        sym_data.extend(encode_nat(0));
        let cls = build_entry(TAG_CLASS_SYM, &sym_data);

        let sig = build_signature(vec![name, owner, cls]);
        let reader = ScalaSignatureReader::parse(&sig).unwrap();

        let entry = reader.entry(2).unwrap();
        let info = reader.read_symbol_info(entry).unwrap();
        assert_ne!(info.flags & FLAG_SEALED, 0);
    }

    #[test]
    fn private_and_protected_flags() {
        // Private class.
        let name = build_entry(TAG_TYPE_NAME, b"Inner");
        let owner = build_entry(TAG_NONE_SYM, &[]);

        let mut sym_data = Vec::new();
        sym_data.extend(encode_nat(0));
        sym_data.extend(encode_nat(1));
        sym_data.extend(encode_nat(FLAG_PRIVATE));
        sym_data.extend(encode_nat(0));
        let cls = build_entry(TAG_CLASS_SYM, &sym_data);

        let sig = build_signature(vec![name, owner, cls]);
        let reader = ScalaSignatureReader::parse(&sig).unwrap();
        let entry = reader.entry(2).unwrap();
        let info = reader.read_symbol_info(entry).unwrap();
        assert_ne!(info.flags & FLAG_PRIVATE, 0);
        assert_eq!(info.flags & FLAG_PROTECTED, 0);
    }

    #[test]
    fn large_nat_flag_value() {
        // Test that FLAG_TRAIT (bit 36) survives encoding/decoding.
        let encoded = encode_nat(FLAG_TRAIT);
        let mut pos = 0;
        let decoded = read_nat(&encoded, &mut pos).unwrap();
        assert_eq!(decoded, FLAG_TRAIT);
        assert_eq!(decoded, 1 << 36);
    }
}
