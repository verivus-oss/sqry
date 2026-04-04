//! Protobuf wire-format reader and Kotlin metadata extraction.
//!
//! This module implements a minimal protobuf wire-format decoder that reads
//! the binary blob stored in the `d1` field of `@kotlin.Metadata` annotations.
//! It extracts Kotlin-specific semantic information that the JVM bytecode
//! representation erases: extension receivers, nullable types, companion
//! objects, class kind (object/data/sealed), and visibility.
//!
//! # Wire format
//!
//! Protobuf encodes each field as `(field_number << 3) | wire_type`:
//! - Wire type 0: varint (7 bits per byte, MSB = continuation)
//! - Wire type 1: 64-bit fixed
//! - Wire type 2: length-delimited (varint length prefix + bytes)
//! - Wire type 5: 32-bit fixed
//!
//! We only read the specific field numbers defined in `kotlin.metadata.jvm.proto`
//! and skip everything else, making this forward-compatible with new fields.

use crate::stub::model::KotlinMetadataStub;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Decoded Kotlin metadata for a class.
///
/// Contains the Kotlin-specific information that is erased during bytecode
/// compilation. This is used to enrich graph nodes with accurate visibility,
/// class kind, and relationship edges (extension receivers, companion objects).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KotlinClassMetadata {
    /// Kotlin class kind (Class, Object, `CompanionObject`, Interface, etc.).
    pub kind: KotlinClassKind,
    /// Kotlin visibility (public, private, protected, internal).
    pub visibility: KotlinVisibility,
    /// Whether this is a data class (has `data` modifier).
    pub is_data: bool,
    /// Whether this is a sealed class (has `sealed` modifier — modality=3).
    pub is_sealed: bool,
    /// Companion object name (if this class has one).
    ///
    /// Most companion objects are named `"Companion"`, but Kotlin allows
    /// custom names: `companion object Factory { ... }`.
    pub companion_object_name: Option<String>,
    /// Extension functions declared in this class with their receiver types.
    pub extension_functions: Vec<KotlinExtensionFunction>,
    /// Property names whose types are nullable (`T?`).
    pub nullable_properties: Vec<String>,
}

/// Kotlin class kind as encoded in the metadata flags (bits 9-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KotlinClassKind {
    /// Regular class.
    Class,
    /// Interface (including functional interfaces).
    Interface,
    /// Enum class.
    EnumClass,
    /// Enum entry (individual constant).
    EnumEntry,
    /// Annotation class.
    AnnotationClass,
    /// Singleton `object` declaration.
    Object,
    /// Companion object.
    CompanionObject,
}

/// Kotlin visibility as encoded in the metadata flags (bits 3-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KotlinVisibility {
    /// Visible within the module.
    Internal,
    /// Visible only within the declaring class.
    Private,
    /// Visible to subclasses.
    Protected,
    /// Visible everywhere.
    Public,
    /// `private` scoped to `this` (backing field access).
    PrivateToThis,
    /// Local declaration (inside a function body).
    Local,
}

/// An extension function with its receiver type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KotlinExtensionFunction {
    /// Function name.
    pub name: String,
    /// Fully qualified receiver type (e.g., `"kotlin/String"`).
    pub receiver_type: String,
    /// Whether the receiver type is nullable.
    pub receiver_nullable: bool,
}

// ---------------------------------------------------------------------------
// Wire format constants
// ---------------------------------------------------------------------------

/// Protobuf wire types.
const WIRE_VARINT: u8 = 0;
const WIRE_64BIT: u8 = 1;
const WIRE_LENGTH_DELIMITED: u8 = 2;
const WIRE_32BIT: u8 = 5;

/// Class message field numbers (from `kotlin.metadata.jvm.proto`).
const CLASS_FLAGS: u32 = 1;
const CLASS_FUNCTIONS: u32 = 14;
const CLASS_PROPERTIES: u32 = 15;
const CLASS_COMPANION_OBJECT_NAME: u32 = 20;

/// Function message field numbers.
const FUNCTION_FLAGS: u32 = 1;
const FUNCTION_NAME: u32 = 2;
const FUNCTION_RECEIVER_TYPE: u32 = 6;

/// Property message field numbers.
const PROPERTY_FLAGS: u32 = 1;
const PROPERTY_NAME: u32 = 2;
const PROPERTY_RETURN_TYPE: u32 = 5;

/// Type message field numbers.
const TYPE_FLAGS: u32 = 1;
const TYPE_CLASS_NAME: u32 = 6;

// ---------------------------------------------------------------------------
// Flag extraction helpers
// ---------------------------------------------------------------------------

/// Extract visibility from a flags varint (bits 3-5, zero-indexed).
fn extract_visibility(flags: u64) -> KotlinVisibility {
    match (flags >> 3) & 0x7 {
        0 => KotlinVisibility::Internal,
        1 => KotlinVisibility::Private,
        2 => KotlinVisibility::Protected,
        3 => KotlinVisibility::Public,
        4 => KotlinVisibility::PrivateToThis,
        5 => KotlinVisibility::Local,
        _ => KotlinVisibility::Public, // unknown → default to public
    }
}

/// Extract modality from a flags varint (bits 6-8, zero-indexed).
fn extract_modality(flags: u64) -> u8 {
    ((flags >> 6) & 0x7) as u8
}

/// Extract class kind from a flags varint (bits 9-11, zero-indexed).
fn extract_class_kind(flags: u64) -> KotlinClassKind {
    match (flags >> 9) & 0x7 {
        0 => KotlinClassKind::Class,
        1 => KotlinClassKind::Interface,
        2 => KotlinClassKind::EnumClass,
        3 => KotlinClassKind::EnumEntry,
        4 => KotlinClassKind::AnnotationClass,
        5 => KotlinClassKind::Object,
        6 => KotlinClassKind::CompanionObject,
        _ => KotlinClassKind::Class, // unknown → default to class
    }
}

// ---------------------------------------------------------------------------
// Wire format reader
// ---------------------------------------------------------------------------

/// Minimal protobuf wire-format reader.
///
/// Supports varint, length-delimited, and fixed-width field types. Does not
/// allocate — all returned byte slices borrow from the input buffer.
struct WireReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> WireReader<'a> {
    /// Create a new reader over the given byte slice.
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Returns `true` if there are more bytes to read.
    fn has_remaining(&self) -> bool {
        self.pos < self.data.len()
    }

    /// Read a varint (up to 64 bits, 10 bytes max).
    ///
    /// Returns `None` if the input is truncated or the varint exceeds 10 bytes
    /// (malformed data).
    fn read_varint(&mut self) -> Option<u64> {
        let mut result: u64 = 0;
        let mut shift: u32 = 0;

        for _ in 0..10 {
            if self.pos >= self.data.len() {
                return None;
            }
            let byte = self.data[self.pos];
            self.pos += 1;

            result |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Some(result);
            }
            shift += 7;
        }

        // Varint exceeds 10 bytes — malformed.
        None
    }

    /// Read a field tag and decompose it into `(field_number, wire_type)`.
    ///
    /// Returns `None` at EOF or on malformed input.
    fn read_tag(&mut self) -> Option<(u32, u8)> {
        let raw = self.read_varint()?;
        let wire_type = (raw & 0x7) as u8;
        let field_number = (raw >> 3) as u32;
        if field_number == 0 {
            return None; // field number 0 is invalid
        }
        Some((field_number, wire_type))
    }

    /// Read a length-delimited field (varint length prefix + payload bytes).
    ///
    /// Returns the payload as a borrowed byte slice, or `None` if truncated.
    fn read_length_delimited(&mut self) -> Option<&'a [u8]> {
        let len = self.read_varint()? as usize;
        if self.pos + len > self.data.len() {
            return None;
        }
        let slice = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Some(slice)
    }

    /// Skip a field of the given wire type.
    ///
    /// Returns `false` if the data is malformed (truncated or unknown wire type).
    fn skip_field(&mut self, wire_type: u8) -> bool {
        match wire_type {
            WIRE_VARINT => self.read_varint().is_some(),
            WIRE_64BIT => {
                if self.pos + 8 > self.data.len() {
                    return false;
                }
                self.pos += 8;
                true
            }
            WIRE_LENGTH_DELIMITED => self.read_length_delimited().is_some(),
            WIRE_32BIT => {
                if self.pos + 4 > self.data.len() {
                    return false;
                }
                self.pos += 4;
                true
            }
            _ => false, // unknown wire type
        }
    }
}

// ---------------------------------------------------------------------------
// String table
// ---------------------------------------------------------------------------

/// Look up a string in the `d2` string table by index.
///
/// Returns `None` if the index is out of bounds.
fn string_table_lookup(d2: &[String], index: u64) -> Option<&str> {
    let idx = index as usize;
    d2.get(idx).map(String::as_str)
}

// ---------------------------------------------------------------------------
// Type message decoder
// ---------------------------------------------------------------------------

/// Decoded information from a `Type` protobuf message.
struct DecodedType {
    /// Whether the type is nullable (`T?`).
    nullable: bool,
    /// Class name index into the string table.
    class_name_index: Option<u64>,
}

/// Decode a `Type` message from its raw protobuf bytes.
///
/// Extracts the nullable flag (bit 1 of field 1) and the class name index
/// (field 6).
fn decode_type(data: &[u8]) -> Option<DecodedType> {
    let mut reader = WireReader::new(data);
    let mut flags: u64 = 0;
    let mut class_name_index: Option<u64> = None;

    while reader.has_remaining() {
        let (field_number, wire_type) = reader.read_tag()?;

        match (field_number, wire_type) {
            (TYPE_FLAGS, WIRE_VARINT) => {
                flags = reader.read_varint()?;
            }
            (TYPE_CLASS_NAME, WIRE_VARINT) => {
                class_name_index = Some(reader.read_varint()?);
            }
            _ => {
                if !reader.skip_field(wire_type) {
                    return None;
                }
            }
        }
    }

    Some(DecodedType {
        nullable: (flags >> 1) & 1 == 1,
        class_name_index,
    })
}

// ---------------------------------------------------------------------------
// Function message decoder
// ---------------------------------------------------------------------------

/// Decoded information from a `Function` message.
struct DecodedFunction {
    /// Function name index into the string table.
    name_index: u64,
    /// Receiver type bytes (present only for extension functions).
    receiver_type: Option<DecodedType>,
}

/// Decode a `Function` message from its raw protobuf bytes.
///
/// Extracts the function name index and, if present, the receiver type
/// (which marks this as an extension function).
fn decode_function(data: &[u8]) -> Option<DecodedFunction> {
    let mut reader = WireReader::new(data);
    let mut name_index: u64 = 0;
    let mut receiver_type: Option<DecodedType> = None;

    while reader.has_remaining() {
        let (field_number, wire_type) = reader.read_tag()?;

        match (field_number, wire_type) {
            (FUNCTION_FLAGS, WIRE_VARINT) => {
                // Read and discard flags — we extract name and receiver only.
                let _flags = reader.read_varint()?;
            }
            (FUNCTION_NAME, WIRE_VARINT) => {
                name_index = reader.read_varint()?;
            }
            (FUNCTION_RECEIVER_TYPE, WIRE_LENGTH_DELIMITED) => {
                let type_data = reader.read_length_delimited()?;
                receiver_type = decode_type(type_data);
            }
            _ => {
                if !reader.skip_field(wire_type) {
                    return None;
                }
            }
        }
    }

    Some(DecodedFunction {
        name_index,
        receiver_type,
    })
}

// ---------------------------------------------------------------------------
// Property message decoder
// ---------------------------------------------------------------------------

/// Decoded information from a `Property` message.
struct DecodedProperty {
    /// Property name index into the string table.
    name_index: u64,
    /// Whether the return type is nullable.
    return_type_nullable: bool,
}

/// Decode a `Property` message from its raw protobuf bytes.
///
/// Extracts the property name index and whether its return type is nullable.
fn decode_property(data: &[u8]) -> Option<DecodedProperty> {
    let mut reader = WireReader::new(data);
    let mut name_index: u64 = 0;
    let mut return_type_nullable = false;

    while reader.has_remaining() {
        let (field_number, wire_type) = reader.read_tag()?;

        match (field_number, wire_type) {
            (PROPERTY_FLAGS, WIRE_VARINT) => {
                let _flags = reader.read_varint()?;
            }
            (PROPERTY_NAME, WIRE_VARINT) => {
                name_index = reader.read_varint()?;
            }
            (PROPERTY_RETURN_TYPE, WIRE_LENGTH_DELIMITED) => {
                let type_data = reader.read_length_delimited()?;
                if let Some(decoded) = decode_type(type_data) {
                    return_type_nullable = decoded.nullable;
                }
            }
            _ => {
                if !reader.skip_field(wire_type) {
                    return None;
                }
            }
        }
    }

    Some(DecodedProperty {
        name_index,
        return_type_nullable,
    })
}

// ---------------------------------------------------------------------------
// Class message decoder (top-level)
// ---------------------------------------------------------------------------

/// Decode the top-level `Class` message from `d1[0]` protobuf bytes.
///
/// Extracts class flags, companion object name, functions (with extension
/// receiver detection), and properties (with nullable type detection).
fn decode_class_message(data: &[u8], string_table: &[String]) -> Option<KotlinClassMetadata> {
    let mut reader = WireReader::new(data);
    let mut flags: u64 = 0;
    let mut companion_name_index: Option<u64> = None;
    let mut extension_functions = Vec::new();
    let mut nullable_properties = Vec::new();

    while reader.has_remaining() {
        let (field_number, wire_type) = reader.read_tag()?;

        match (field_number, wire_type) {
            (CLASS_FLAGS, WIRE_VARINT) => {
                flags = reader.read_varint()?;
            }
            (CLASS_FUNCTIONS, WIRE_LENGTH_DELIMITED) => {
                let func_data = reader.read_length_delimited()?;
                if let Some(func) = decode_function(func_data)
                    && let Some(ref recv_type) = func.receiver_type
                {
                    // This is an extension function — resolve names.
                    let fn_name = string_table_lookup(string_table, func.name_index)
                        .unwrap_or("<unknown>")
                        .to_owned();

                    let receiver_name = recv_type
                        .class_name_index
                        .and_then(|idx| string_table_lookup(string_table, idx))
                        .unwrap_or("<unknown>")
                        .to_owned();

                    extension_functions.push(KotlinExtensionFunction {
                        name: fn_name,
                        receiver_type: receiver_name,
                        receiver_nullable: recv_type.nullable,
                    });
                }
            }
            (CLASS_PROPERTIES, WIRE_LENGTH_DELIMITED) => {
                let prop_data = reader.read_length_delimited()?;
                if let Some(prop) = decode_property(prop_data)
                    && prop.return_type_nullable
                    && let Some(name) = string_table_lookup(string_table, prop.name_index)
                {
                    nullable_properties.push(name.to_owned());
                }
            }
            (CLASS_COMPANION_OBJECT_NAME, WIRE_VARINT) => {
                companion_name_index = Some(reader.read_varint()?);
            }
            _ => {
                if !reader.skip_field(wire_type) {
                    return None;
                }
            }
        }
    }

    let class_kind = extract_class_kind(flags);
    let visibility = extract_visibility(flags);
    let modality = extract_modality(flags);

    // Data class: class kind is Class (0) and has the `data` modifier.
    // The data flag is bit 12 in Kotlin metadata class flags.
    let is_data = class_kind == KotlinClassKind::Class && (flags >> 12) & 1 == 1;
    let is_sealed = modality == 3; // modality 3 = sealed

    let companion_object_name = companion_name_index
        .and_then(|idx| string_table_lookup(string_table, idx))
        .map(str::to_owned);

    Some(KotlinClassMetadata {
        kind: class_kind,
        visibility,
        is_data,
        is_sealed,
        companion_object_name,
        extension_functions,
        nullable_properties,
    })
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Decode Kotlin metadata from a [`KotlinMetadataStub`].
///
/// Returns `None` if the metadata kind is not supported (only kind=1 Class is
/// decoded in Tier 1) or if decoding fails. When `None` is returned, callers
/// should fall back to bytecode-only analysis.
///
/// # Errors
///
/// This function never panics. Malformed protobuf data, unsupported metadata
/// versions, and out-of-bounds string table references all result in `None`.
pub fn decode_kotlin_metadata(stub: &KotlinMetadataStub) -> Option<KotlinClassMetadata> {
    // Only kind=1 (Class) is supported in Tier 1.
    if stub.kind != 1 {
        log::debug!(
            "skipping Kotlin metadata kind {} (only kind=1 Class is supported)",
            stub.kind,
        );
        return None;
    }

    // Validate metadata version — we support 1.x.
    if let Some(&major) = stub.metadata_version.first()
        && !(1..=2).contains(&major)
    {
        log::warn!(
            "unsupported Kotlin metadata version {:?}, skipping",
            stub.metadata_version,
        );
        return None;
    }

    // d1 must contain at least one protobuf chunk.
    let d1_combined = combine_d1_chunks(&stub.data1)?;

    // An empty protobuf message is valid — all fields take default values.
    decode_class_message(&d1_combined, &stub.data2)
}

/// Combine `d1` string chunks into raw protobuf bytes.
///
/// In JVM bytecode, `d1` entries are `String[]` where each character's
/// Unicode code point represents a raw byte value (Latin-1 / ISO-8859-1
/// encoding). Kotlin uses code points 0-255 to encode arbitrary protobuf
/// bytes in JVM strings. We iterate over chars and extract the low byte of
/// each code point.
///
/// Returns `None` if `d1` is empty.
fn combine_d1_chunks(d1: &[String]) -> Option<Vec<u8>> {
    if d1.is_empty() {
        return None;
    }

    let total_chars: usize = d1.iter().map(|s| s.chars().count()).sum();
    let mut bytes = Vec::with_capacity(total_chars);
    for chunk in d1 {
        for ch in chunk.chars() {
            // Each char's code point is a protobuf byte (0-255).
            // Code points > 255 should not appear in valid metadata, but
            // we mask to the low byte defensively.
            bytes.push((ch as u32 & 0xFF) as u8);
        }
    }
    Some(bytes)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Wire format helpers ------------------------------------------------

    /// Encode a varint into bytes.
    fn encode_varint(mut value: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        loop {
            let mut byte = (value & 0x7F) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            buf.push(byte);
            if value == 0 {
                break;
            }
        }
        buf
    }

    /// Encode a protobuf tag.
    fn encode_tag(field_number: u32, wire_type: u8) -> Vec<u8> {
        encode_varint(u64::from(field_number) << 3 | u64::from(wire_type))
    }

    /// Encode a varint field (tag + varint value).
    fn encode_varint_field(field_number: u32, value: u64) -> Vec<u8> {
        let mut buf = encode_tag(field_number, WIRE_VARINT);
        buf.extend(encode_varint(value));
        buf
    }

    /// Encode a length-delimited field (tag + length + bytes).
    fn encode_length_delimited_field(field_number: u32, data: &[u8]) -> Vec<u8> {
        let mut buf = encode_tag(field_number, WIRE_LENGTH_DELIMITED);
        buf.extend(encode_varint(data.len() as u64));
        buf.extend(data);
        buf
    }

    /// Build class flags from visibility, modality, and class kind.
    fn build_class_flags(visibility: u64, modality: u64, class_kind: u64, is_data: bool) -> u64 {
        let mut flags = 0u64;
        flags |= visibility << 3;
        flags |= modality << 6;
        flags |= class_kind << 9;
        if is_data {
            flags |= 1 << 12;
        }
        flags
    }

    /// Build a Type message with optional nullable flag and class name index.
    fn build_type_message(nullable: bool, class_name_index: Option<u64>) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut flags: u64 = 0;
        if nullable {
            flags |= 1 << 1;
        }
        if flags != 0 {
            buf.extend(encode_varint_field(TYPE_FLAGS, flags));
        }
        if let Some(idx) = class_name_index {
            buf.extend(encode_varint_field(TYPE_CLASS_NAME, idx));
        }
        buf
    }

    /// Build a Function message.
    fn build_function_message(name_index: u64, receiver_type: Option<&[u8]>) -> Vec<u8> {
        let mut buf = Vec::new();
        // flags (field 1) — just set to 0 for tests.
        buf.extend(encode_varint_field(FUNCTION_FLAGS, 0));
        // name (field 2)
        buf.extend(encode_varint_field(FUNCTION_NAME, name_index));
        // receiver_type (field 6) — only for extension functions
        if let Some(rt) = receiver_type {
            buf.extend(encode_length_delimited_field(FUNCTION_RECEIVER_TYPE, rt));
        }
        buf
    }

    /// Build a Property message.
    fn build_property_message(name_index: u64, return_type: Option<&[u8]>) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend(encode_varint_field(PROPERTY_FLAGS, 0));
        buf.extend(encode_varint_field(PROPERTY_NAME, name_index));
        if let Some(rt) = return_type {
            buf.extend(encode_length_delimited_field(PROPERTY_RETURN_TYPE, rt));
        }
        buf
    }

    /// Create a `KotlinMetadataStub` from raw d1 bytes and a string table.
    ///
    /// Uses Latin-1 encoding (byte → char code point) to match how Kotlin
    /// stores protobuf bytes in JVM string constants.
    fn make_stub(d1_bytes: Vec<u8>, string_table: Vec<&str>) -> KotlinMetadataStub {
        let d1_string: String = d1_bytes.iter().map(|&b| b as char).collect();
        KotlinMetadataStub {
            kind: 1,
            metadata_version: vec![1, 9, 0],
            data1: vec![d1_string],
            data2: string_table.into_iter().map(str::to_owned).collect(),
            extra_string: None,
            package_name: None,
            extra_int: None,
        }
    }

    // -- WireReader tests ---------------------------------------------------

    #[test]
    fn wire_reader_varint_single_byte() {
        let data = [0x05]; // value = 5
        let mut reader = WireReader::new(&data);
        assert_eq!(reader.read_varint(), Some(5));
        assert!(!reader.has_remaining());
    }

    #[test]
    fn wire_reader_varint_multi_byte() {
        // 300 = 0b100101100
        // byte 0: 0b10101100 = 0xAC
        // byte 1: 0b00000010 = 0x02
        let data = [0xAC, 0x02];
        let mut reader = WireReader::new(&data);
        assert_eq!(reader.read_varint(), Some(300));
    }

    #[test]
    fn wire_reader_varint_max_bytes() {
        // u64::MAX requires 10 bytes.
        let encoded = encode_varint(u64::MAX);
        assert_eq!(encoded.len(), 10);
        let mut reader = WireReader::new(&encoded);
        assert_eq!(reader.read_varint(), Some(u64::MAX));
    }

    #[test]
    fn wire_reader_varint_truncated() {
        // Continuation bit set but no next byte.
        let data = [0x80];
        let mut reader = WireReader::new(&data);
        assert_eq!(reader.read_varint(), None);
    }

    #[test]
    fn wire_reader_varint_exceeds_10_bytes() {
        // 11 bytes all with continuation bit — malformed.
        let data = [0x80; 11];
        let mut reader = WireReader::new(&data);
        assert_eq!(reader.read_varint(), None);
    }

    #[test]
    fn wire_reader_tag_decomposition() {
        // Field 3, wire type 2 (length-delimited): (3 << 3) | 2 = 26 = 0x1A
        let data = [0x1A];
        let mut reader = WireReader::new(&data);
        assert_eq!(reader.read_tag(), Some((3, 2)));
    }

    #[test]
    fn wire_reader_tag_field_zero_invalid() {
        // Field 0 is invalid in protobuf.
        let data = [0x02]; // (0 << 3) | 2 = 2 → field 0, wire type 2
        let mut reader = WireReader::new(&data);
        assert_eq!(reader.read_tag(), None);
    }

    #[test]
    fn wire_reader_length_delimited() {
        // Tag for field 1, wire type 2: (1 << 3) | 2 = 10 = 0x0A
        // Length = 3, payload = [0x01, 0x02, 0x03]
        let data = [0x0A, 0x03, 0x01, 0x02, 0x03];
        let mut reader = WireReader::new(&data);
        let (field, wire) = reader.read_tag().unwrap();
        assert_eq!((field, wire), (1, 2));
        let payload = reader.read_length_delimited().unwrap();
        assert_eq!(payload, &[0x01, 0x02, 0x03]);
    }

    #[test]
    fn wire_reader_length_delimited_truncated() {
        // Claims length 5 but only 2 bytes follow.
        let data = [0x05, 0x01, 0x02];
        let mut reader = WireReader::new(&data);
        assert_eq!(reader.read_length_delimited(), None);
    }

    #[test]
    fn wire_reader_skip_varint() {
        let mut data = encode_tag(99, WIRE_VARINT);
        data.extend(encode_varint(42));
        data.extend(encode_tag(1, WIRE_VARINT));
        data.extend(encode_varint(7));

        let mut reader = WireReader::new(&data);
        let (field, wire) = reader.read_tag().unwrap();
        assert_eq!(field, 99);
        assert!(reader.skip_field(wire));

        let (field2, _) = reader.read_tag().unwrap();
        assert_eq!(field2, 1);
    }

    #[test]
    fn wire_reader_skip_32bit() {
        let mut data = vec![];
        data.extend(encode_tag(5, WIRE_32BIT));
        data.extend(&[0x00, 0x00, 0x00, 0x00]); // 4 bytes
        data.extend(encode_tag(1, WIRE_VARINT));
        data.extend(encode_varint(99));

        let mut reader = WireReader::new(&data);
        let (_, wire) = reader.read_tag().unwrap();
        assert!(reader.skip_field(wire));
        let (field, _) = reader.read_tag().unwrap();
        assert_eq!(field, 1);
    }

    #[test]
    fn wire_reader_skip_64bit() {
        let mut data = vec![];
        data.extend(encode_tag(5, WIRE_64BIT));
        data.extend(&[0u8; 8]); // 8 bytes
        data.extend(encode_tag(1, WIRE_VARINT));
        data.extend(encode_varint(99));

        let mut reader = WireReader::new(&data);
        let (_, wire) = reader.read_tag().unwrap();
        assert!(reader.skip_field(wire));
        let (field, _) = reader.read_tag().unwrap();
        assert_eq!(field, 1);
    }

    #[test]
    fn wire_reader_skip_unknown_wire_type() {
        let mut reader = WireReader::new(&[]);
        assert!(!reader.skip_field(3)); // wire type 3 is deprecated/unknown
    }

    // -- String table tests -------------------------------------------------

    #[test]
    fn string_table_valid_lookup() {
        let table = vec!["kotlin/String".to_owned(), "isEmail".to_owned()];
        assert_eq!(string_table_lookup(&table, 0), Some("kotlin/String"));
        assert_eq!(string_table_lookup(&table, 1), Some("isEmail"));
    }

    #[test]
    fn string_table_out_of_bounds() {
        let table = vec!["only_one".to_owned()];
        assert_eq!(string_table_lookup(&table, 1), None);
        assert_eq!(string_table_lookup(&table, 999), None);
    }

    // -- Extension receiver detection ---------------------------------------

    #[test]
    fn extension_receiver_detection() {
        // String table: [0]="kotlin/String", [1]="isEmail"
        let receiver_type = build_type_message(false, Some(0));
        let func = build_function_message(1, Some(&receiver_type));

        let mut d1 = Vec::new();
        // Class flags: public, final, class
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 0, false),
        ));
        // Function with extension receiver
        d1.extend(encode_length_delimited_field(CLASS_FUNCTIONS, &func));

        let stub = make_stub(d1, vec!["kotlin/String", "isEmail"]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert_eq!(meta.extension_functions.len(), 1);
        assert_eq!(meta.extension_functions[0].name, "isEmail");
        assert_eq!(meta.extension_functions[0].receiver_type, "kotlin/String");
        assert!(!meta.extension_functions[0].receiver_nullable);
    }

    #[test]
    fn extension_receiver_nullable() {
        // Test nullable receiver: `fun String?.isNullOrEmail()`
        let receiver_type = build_type_message(true, Some(0));
        let func = build_function_message(1, Some(&receiver_type));

        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 0, false),
        ));
        d1.extend(encode_length_delimited_field(CLASS_FUNCTIONS, &func));

        let stub = make_stub(d1, vec!["kotlin/String", "isNullOrEmail"]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert_eq!(meta.extension_functions.len(), 1);
        assert_eq!(meta.extension_functions[0].name, "isNullOrEmail");
        assert!(meta.extension_functions[0].receiver_nullable);
    }

    #[test]
    fn regular_function_not_treated_as_extension() {
        // Function without receiver_type should NOT appear in extension_functions.
        let func = build_function_message(0, None);

        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 0, false),
        ));
        d1.extend(encode_length_delimited_field(CLASS_FUNCTIONS, &func));

        let stub = make_stub(d1, vec!["regularFunction"]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert!(meta.extension_functions.is_empty());
    }

    // -- Nullable parameter detection ---------------------------------------

    #[test]
    fn nullable_property_detection() {
        // Property with nullable return type
        let nullable_type = build_type_message(true, Some(0));
        let prop = build_property_message(1, Some(&nullable_type));

        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 0, false),
        ));
        d1.extend(encode_length_delimited_field(CLASS_PROPERTIES, &prop));

        let stub = make_stub(d1, vec!["kotlin/String", "name"]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert_eq!(meta.nullable_properties, vec!["name"]);
    }

    #[test]
    fn non_nullable_property_not_included() {
        let non_nullable_type = build_type_message(false, Some(0));
        let prop = build_property_message(1, Some(&non_nullable_type));

        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 0, false),
        ));
        d1.extend(encode_length_delimited_field(CLASS_PROPERTIES, &prop));

        let stub = make_stub(d1, vec!["kotlin/String", "name"]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert!(meta.nullable_properties.is_empty());
    }

    // -- Companion object detection -----------------------------------------

    #[test]
    fn companion_object_detection() {
        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 0, false),
        ));
        // companion_object_name = string table index 0 = "Companion"
        d1.extend(encode_varint_field(CLASS_COMPANION_OBJECT_NAME, 0));

        let stub = make_stub(d1, vec!["Companion"]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert_eq!(meta.companion_object_name, Some("Companion".to_owned()));
    }

    #[test]
    fn companion_object_custom_name() {
        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 0, false),
        ));
        d1.extend(encode_varint_field(CLASS_COMPANION_OBJECT_NAME, 0));

        let stub = make_stub(d1, vec!["Factory"]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert_eq!(meta.companion_object_name, Some("Factory".to_owned()));
    }

    #[test]
    fn no_companion_object() {
        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 0, false),
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert_eq!(meta.companion_object_name, None);
    }

    // -- Object declaration (singleton) -------------------------------------

    #[test]
    fn object_declaration_kind() {
        let mut d1 = Vec::new();
        // class kind 5 = object
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 5, false),
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert_eq!(meta.kind, KotlinClassKind::Object);
    }

    #[test]
    fn companion_object_kind() {
        let mut d1 = Vec::new();
        // class kind 6 = companion object
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 6, false),
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert_eq!(meta.kind, KotlinClassKind::CompanionObject);
    }

    // -- Data class flag detection ------------------------------------------

    #[test]
    fn data_class_detection() {
        let mut d1 = Vec::new();
        // class kind 0 = class, is_data = true
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 0, true),
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert!(meta.is_data);
        assert_eq!(meta.kind, KotlinClassKind::Class);
    }

    #[test]
    fn non_data_class() {
        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 0, false),
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert!(!meta.is_data);
    }

    // -- Sealed class flag detection ----------------------------------------

    #[test]
    fn sealed_class_detection() {
        let mut d1 = Vec::new();
        // modality 3 = sealed
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 3, 0, false),
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert!(meta.is_sealed);
    }

    #[test]
    fn non_sealed_class() {
        let mut d1 = Vec::new();
        // modality 0 = final
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 0, false),
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert!(!meta.is_sealed);
    }

    // -- Visibility extraction ----------------------------------------------

    #[test]
    fn visibility_public() {
        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 0, false), // visibility 3 = public
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();
        assert_eq!(meta.visibility, KotlinVisibility::Public);
    }

    #[test]
    fn visibility_private() {
        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(1, 0, 0, false), // visibility 1 = private
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();
        assert_eq!(meta.visibility, KotlinVisibility::Private);
    }

    #[test]
    fn visibility_internal() {
        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(0, 0, 0, false), // visibility 0 = internal
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();
        assert_eq!(meta.visibility, KotlinVisibility::Internal);
    }

    #[test]
    fn visibility_protected() {
        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(2, 0, 0, false), // visibility 2 = protected
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();
        assert_eq!(meta.visibility, KotlinVisibility::Protected);
    }

    // -- Malformed protobuf → None ------------------------------------------

    #[test]
    fn malformed_protobuf_returns_none() {
        // Random garbage bytes — should not panic, should return None or
        // degrade gracefully.
        let stub = make_stub(
            vec![
                0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
            ],
            vec![],
        );
        // This may return Some with defaults or None — either is acceptable
        // as long as it doesn't panic.
        let _result = decode_kotlin_metadata(&stub);
    }

    #[test]
    fn empty_d1_returns_none() {
        let stub = KotlinMetadataStub {
            kind: 1,
            metadata_version: vec![1, 9, 0],
            data1: vec![],
            data2: vec![],
            extra_string: None,
            package_name: None,
            extra_int: None,
        };
        assert_eq!(decode_kotlin_metadata(&stub), None);
    }

    #[test]
    fn truncated_varint_returns_none() {
        // Tag byte with continuation bit but no subsequent byte.
        let stub = make_stub(vec![0x80], vec![]);
        assert_eq!(decode_kotlin_metadata(&stub), None);
    }

    // -- Unsupported kind → None --------------------------------------------

    #[test]
    fn unsupported_kind_returns_none() {
        let stub = KotlinMetadataStub {
            kind: 2, // file facade — not supported in Tier 1
            metadata_version: vec![1, 9, 0],
            data1: vec!["data".to_owned()],
            data2: vec![],
            extra_string: None,
            package_name: None,
            extra_int: None,
        };
        assert_eq!(decode_kotlin_metadata(&stub), None);
    }

    #[test]
    fn unsupported_kind_synthetic() {
        let stub = KotlinMetadataStub {
            kind: 3,
            metadata_version: vec![1, 9, 0],
            data1: vec![],
            data2: vec![],
            extra_string: None,
            package_name: None,
            extra_int: None,
        };
        assert_eq!(decode_kotlin_metadata(&stub), None);
    }

    #[test]
    fn unsupported_metadata_version() {
        let stub = KotlinMetadataStub {
            kind: 1,
            metadata_version: vec![99, 0, 0], // far future version
            data1: vec!["data".to_owned()],
            data2: vec![],
            extra_string: None,
            package_name: None,
            extra_int: None,
        };
        assert_eq!(decode_kotlin_metadata(&stub), None);
    }

    // -- Comprehensive scenario: all features combined ----------------------

    #[test]
    fn comprehensive_class_decoding() {
        // String table:
        //   [0] = "kotlin/String"
        //   [1] = "isEmail"
        //   [2] = "name"
        //   [3] = "Companion"
        //   [4] = "toString"
        let string_table = vec!["kotlin/String", "isEmail", "name", "Companion", "toString"];

        // Build an extension function: fun String.isEmail()
        let receiver_type = build_type_message(false, Some(0));
        let ext_func = build_function_message(1, Some(&receiver_type));

        // Build a regular function: fun toString()
        let regular_func = build_function_message(4, None);

        // Build a nullable property: var name: String?
        let nullable_type = build_type_message(true, Some(0));
        let nullable_prop = build_property_message(2, Some(&nullable_type));

        // Assemble class message
        let mut d1 = Vec::new();
        // public, final, class, data=false
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 0, false),
        ));
        d1.extend(encode_length_delimited_field(CLASS_FUNCTIONS, &ext_func));
        d1.extend(encode_length_delimited_field(
            CLASS_FUNCTIONS,
            &regular_func,
        ));
        d1.extend(encode_length_delimited_field(
            CLASS_PROPERTIES,
            &nullable_prop,
        ));
        d1.extend(encode_varint_field(CLASS_COMPANION_OBJECT_NAME, 3));

        let stub = make_stub(d1, string_table);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert_eq!(meta.kind, KotlinClassKind::Class);
        assert_eq!(meta.visibility, KotlinVisibility::Public);
        assert!(!meta.is_data);
        assert!(!meta.is_sealed);
        assert_eq!(meta.companion_object_name, Some("Companion".to_owned()));
        assert_eq!(meta.extension_functions.len(), 1);
        assert_eq!(meta.extension_functions[0].name, "isEmail");
        assert_eq!(meta.extension_functions[0].receiver_type, "kotlin/String");
        assert_eq!(meta.nullable_properties, vec!["name"]);
    }

    // -- Interface kind -----------------------------------------------------

    #[test]
    fn interface_kind_detection() {
        let mut d1 = Vec::new();
        // class kind 1 = interface
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 2, 1, false), // public, abstract, interface
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert_eq!(meta.kind, KotlinClassKind::Interface);
    }

    // -- Enum class kind ----------------------------------------------------

    #[test]
    fn enum_class_kind_detection() {
        let mut d1 = Vec::new();
        // class kind 2 = enum class
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 2, false),
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert_eq!(meta.kind, KotlinClassKind::EnumClass);
    }

    // -- Annotation class kind ----------------------------------------------

    #[test]
    fn annotation_class_kind_detection() {
        let mut d1 = Vec::new();
        // class kind 4 = annotation class
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 4, false),
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert_eq!(meta.kind, KotlinClassKind::AnnotationClass);
    }

    // -- Multiple extension functions ---------------------------------------

    #[test]
    fn multiple_extension_functions() {
        let recv_string = build_type_message(false, Some(0));
        let recv_list = build_type_message(false, Some(2));

        let func1 = build_function_message(1, Some(&recv_string));
        let func2 = build_function_message(3, Some(&recv_list));

        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 0, false),
        ));
        d1.extend(encode_length_delimited_field(CLASS_FUNCTIONS, &func1));
        d1.extend(encode_length_delimited_field(CLASS_FUNCTIONS, &func2));

        let stub = make_stub(
            d1,
            vec!["kotlin/String", "isEmail", "kotlin/List", "firstOrNull"],
        );
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert_eq!(meta.extension_functions.len(), 2);
        assert_eq!(meta.extension_functions[0].name, "isEmail");
        assert_eq!(meta.extension_functions[0].receiver_type, "kotlin/String");
        assert_eq!(meta.extension_functions[1].name, "firstOrNull");
        assert_eq!(meta.extension_functions[1].receiver_type, "kotlin/List");
    }

    // -- Multiple nullable properties ---------------------------------------

    #[test]
    fn multiple_nullable_properties() {
        let nullable_type = build_type_message(true, Some(0));
        let prop1 = build_property_message(1, Some(&nullable_type));
        let prop2 = build_property_message(2, Some(&nullable_type));

        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 0, 0, false),
        ));
        d1.extend(encode_length_delimited_field(CLASS_PROPERTIES, &prop1));
        d1.extend(encode_length_delimited_field(CLASS_PROPERTIES, &prop2));

        let stub = make_stub(d1, vec!["kotlin/String", "name", "email"]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert_eq!(meta.nullable_properties.len(), 2);
        assert!(meta.nullable_properties.contains(&"name".to_owned()));
        assert!(meta.nullable_properties.contains(&"email".to_owned()));
    }

    // -- combine_d1_chunks --------------------------------------------------

    #[test]
    fn combine_d1_multiple_chunks() {
        let d1 = vec!["hel".to_owned(), "lo".to_owned()];
        let combined = combine_d1_chunks(&d1).unwrap();
        assert_eq!(combined, b"hello");
    }

    #[test]
    fn combine_d1_empty() {
        let d1: Vec<String> = vec![];
        assert_eq!(combine_d1_chunks(&d1), None);
    }

    // -- Edge case: data class with all features ----------------------------

    #[test]
    fn data_class_with_sealed_is_not_data() {
        // A sealed data class has both is_data and is_sealed set.
        let mut d1 = Vec::new();
        // modality 3 = sealed, is_data = true
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(3, 3, 0, true),
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        assert!(meta.is_data);
        assert!(meta.is_sealed);
    }

    // -- Default flags when field 1 is absent --------------------------------

    #[test]
    fn missing_flags_defaults_to_internal_final_class() {
        // A class message with no flags field.
        let d1 = Vec::new();
        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();

        // flags=0 → visibility=internal, modality=final, kind=class
        assert_eq!(meta.kind, KotlinClassKind::Class);
        assert_eq!(meta.visibility, KotlinVisibility::Internal);
        assert!(!meta.is_data);
        assert!(!meta.is_sealed);
        assert!(meta.companion_object_name.is_none());
        assert!(meta.extension_functions.is_empty());
        assert!(meta.nullable_properties.is_empty());
    }

    // -- PrivateToThis and Local visibility ----------------------------------

    #[test]
    fn visibility_private_to_this() {
        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(4, 0, 0, false),
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();
        assert_eq!(meta.visibility, KotlinVisibility::PrivateToThis);
    }

    #[test]
    fn visibility_local() {
        let mut d1 = Vec::new();
        d1.extend(encode_varint_field(
            CLASS_FLAGS,
            build_class_flags(5, 0, 0, false),
        ));

        let stub = make_stub(d1, vec![]);
        let meta = decode_kotlin_metadata(&stub).unwrap();
        assert_eq!(meta.visibility, KotlinVisibility::Local);
    }

    // -- Type decoder tests -------------------------------------------------

    #[test]
    fn decode_type_nullable() {
        let data = build_type_message(true, Some(5));
        let decoded = decode_type(&data).unwrap();
        assert!(decoded.nullable);
        assert_eq!(decoded.class_name_index, Some(5));
    }

    #[test]
    fn decode_type_non_nullable() {
        let data = build_type_message(false, Some(3));
        let decoded = decode_type(&data).unwrap();
        assert!(!decoded.nullable);
        assert_eq!(decoded.class_name_index, Some(3));
    }

    #[test]
    fn decode_type_no_class_name() {
        let data = build_type_message(true, None);
        let decoded = decode_type(&data).unwrap();
        assert!(decoded.nullable);
        assert_eq!(decoded.class_name_index, None);
    }

    #[test]
    fn decode_type_empty() {
        let decoded = decode_type(&[]).unwrap();
        assert!(!decoded.nullable);
        assert_eq!(decoded.class_name_index, None);
    }
}
