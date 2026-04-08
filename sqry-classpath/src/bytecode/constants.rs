//! JVM constant pool helper utilities.
//!
//! Provides extraction functions for working with parsed `cafebabe` class file
//! data, converting constant pool items and descriptors into our stub model types.

use cafebabe::attributes::{AttributeData, AttributeInfo};
use cafebabe::constant_pool::LiteralConstant;
use cafebabe::descriptors::{FieldDescriptor, FieldType, MethodDescriptor, ReturnDescriptor};

use crate::stub::model::{BaseType, ConstantValue, OrderedFloat, TypeSignature};

// ---------------------------------------------------------------------------
// Descriptor → TypeSignature conversion
// ---------------------------------------------------------------------------

/// Convert a `cafebabe` [`FieldDescriptor`] into our [`TypeSignature`].
///
/// Handles primitive types, object types (converting `/` to `.` in FQNs),
/// and array types (recursively wrapping in `TypeSignature::Array`).
pub(crate) fn field_descriptor_to_type(desc: &FieldDescriptor<'_>) -> TypeSignature {
    let base = field_type_to_signature(&desc.field_type);
    wrap_in_arrays(base, desc.dimensions)
}

/// Convert a `cafebabe` [`ReturnDescriptor`] into our [`TypeSignature`].
///
/// Maps `void` to `TypeSignature::Base(BaseType::Void)` and delegates field
/// descriptors to [`field_descriptor_to_type`].
pub(crate) fn return_descriptor_to_type(desc: &ReturnDescriptor<'_>) -> TypeSignature {
    match desc {
        ReturnDescriptor::Void => TypeSignature::Base(BaseType::Void),
        ReturnDescriptor::Return(fd) => field_descriptor_to_type(fd),
    }
}

/// Convert a `cafebabe` [`MethodDescriptor`] into parameter types and return type.
pub(crate) fn method_descriptor_to_types(
    desc: &MethodDescriptor<'_>,
) -> (Vec<TypeSignature>, TypeSignature) {
    let param_types = desc
        .parameters
        .iter()
        .map(field_descriptor_to_type)
        .collect();
    let return_type = return_descriptor_to_type(&desc.return_type);
    (param_types, return_type)
}

/// Convert a single `cafebabe` [`FieldType`] into a [`TypeSignature`]
/// (without array wrapping).
fn field_type_to_signature(ft: &FieldType<'_>) -> TypeSignature {
    match ft {
        FieldType::Byte => TypeSignature::Base(BaseType::Byte),
        FieldType::Char => TypeSignature::Base(BaseType::Char),
        FieldType::Double => TypeSignature::Base(BaseType::Double),
        FieldType::Float => TypeSignature::Base(BaseType::Float),
        FieldType::Integer => TypeSignature::Base(BaseType::Int),
        FieldType::Long => TypeSignature::Base(BaseType::Long),
        FieldType::Short => TypeSignature::Base(BaseType::Short),
        FieldType::Boolean => TypeSignature::Base(BaseType::Boolean),
        FieldType::Object(class_name) => TypeSignature::Class {
            fqn: class_name_to_fqn(class_name),
            type_arguments: vec![],
        },
    }
}

/// Wrap a [`TypeSignature`] in `n` layers of `TypeSignature::Array`.
fn wrap_in_arrays(inner: TypeSignature, dimensions: u8) -> TypeSignature {
    let mut sig = inner;
    for _ in 0..dimensions {
        sig = TypeSignature::Array(Box::new(sig));
    }
    sig
}

// ---------------------------------------------------------------------------
// Class name helpers
// ---------------------------------------------------------------------------

/// Convert a JVM internal class name (with `/` separators) to a fully qualified
/// name (with `.` separators).
///
/// For example, `"java/util/HashMap"` becomes `"java.util.HashMap"`.
pub(crate) fn class_name_to_fqn(name: &str) -> String {
    name.replace('/', ".")
}

// ---------------------------------------------------------------------------
// Constant value extraction
// ---------------------------------------------------------------------------

/// Convert a `cafebabe` [`LiteralConstant`] to our [`ConstantValue`].
pub(crate) fn literal_to_constant_value(lit: &LiteralConstant<'_>) -> ConstantValue {
    match lit {
        LiteralConstant::Integer(v) => ConstantValue::Int(*v),
        LiteralConstant::Long(v) => ConstantValue::Long(*v),
        LiteralConstant::Float(v) => ConstantValue::Float(OrderedFloat(*v)),
        LiteralConstant::Double(v) => ConstantValue::Double(OrderedFloat(*v)),
        LiteralConstant::String(s) => ConstantValue::String(s.to_string()),
        LiteralConstant::StringBytes(bytes) => {
            // Best-effort: try UTF-8, fall back to lossy conversion.
            ConstantValue::String(String::from_utf8_lossy(bytes).into_owned())
        }
    }
}

// ---------------------------------------------------------------------------
// Attribute search helpers
// ---------------------------------------------------------------------------

/// Find the first attribute with the given name from a slice of attributes.
#[allow(dead_code)]
pub(crate) fn find_attribute<'a, 'b>(
    attrs: &'a [AttributeInfo<'b>],
    name: &str,
) -> Option<&'a AttributeInfo<'b>> {
    attrs.iter().find(|a| a.name == name)
}

/// Extract the `SourceFile` attribute value from a list of attributes.
pub(crate) fn extract_source_file(attrs: &[AttributeInfo<'_>]) -> Option<String> {
    attrs.iter().find_map(|a| match &a.data {
        AttributeData::SourceFile(s) => Some(s.to_string()),
        _ => None,
    })
}

/// Extract method parameter names from the `MethodParameters` attribute.
pub(crate) fn extract_method_parameter_names(attrs: &[AttributeInfo<'_>]) -> Vec<String> {
    for attr in attrs {
        if let AttributeData::MethodParameters(params) = &attr.data {
            return params
                .iter()
                .filter_map(|p| p.name.as_ref().map(std::string::ToString::to_string))
                .collect();
        }
    }
    vec![]
}

/// Extract the constant value from a field's `ConstantValue` attribute.
pub(crate) fn extract_constant_value(attrs: &[AttributeInfo<'_>]) -> Option<ConstantValue> {
    attrs.iter().find_map(|a| match &a.data {
        AttributeData::ConstantValue(lit) => Some(literal_to_constant_value(lit)),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// Descriptor string parsing (from raw strings, independent of cafebabe)
// ---------------------------------------------------------------------------

/// Parse a raw JVM field descriptor string into a [`TypeSignature`].
///
/// This provides descriptor parsing from raw strings (e.g., when we have a
/// descriptor as a `&str` rather than a parsed `cafebabe` type).
///
/// # Examples
/// - `"I"` → `Base(Int)`
/// - `"Ljava/lang/String;"` → `Class { fqn: "java.lang.String", .. }`
/// - `"[I"` → `Array(Base(Int))`
/// - `"[[Ljava/lang/String;"` → `Array(Array(Class { .. }))`
#[allow(dead_code)]
pub(crate) fn parse_field_descriptor_str(desc: &str) -> Option<TypeSignature> {
    let (sig, rest) = parse_field_type_from_str(desc)?;
    if rest.is_empty() { Some(sig) } else { None }
}

/// Parse a raw JVM method descriptor string, returning parameter types and
/// return type.
///
/// # Examples
/// - `"()V"` → `(vec![], Base(Void))`
/// - `"(ILjava/lang/String;)V"` → `(vec![Base(Int), Class{..}], Base(Void))`
#[allow(dead_code)]
pub(crate) fn parse_method_descriptor_str(
    desc: &str,
) -> Option<(Vec<TypeSignature>, TypeSignature)> {
    let bytes = desc.as_bytes();
    if bytes.is_empty() || bytes[0] != b'(' {
        return None;
    }
    let mut pos = 1;
    let mut params = Vec::new();
    while pos < bytes.len() && bytes[pos] != b')' {
        let (sig, rest) = parse_field_type_from_str(&desc[pos..])?;
        let consumed = desc[pos..].len() - rest.len();
        pos += consumed;
        params.push(sig);
    }
    if pos >= bytes.len() || bytes[pos] != b')' {
        return None;
    }
    pos += 1;
    let ret_str = &desc[pos..];
    let return_type = if ret_str == "V" {
        TypeSignature::Base(BaseType::Void)
    } else {
        let (sig, rest) = parse_field_type_from_str(ret_str)?;
        if !rest.is_empty() {
            return None;
        }
        sig
    };
    Some((params, return_type))
}

/// Parse one field type from the start of a descriptor string, returning the
/// parsed type and the remaining unparsed portion.
fn parse_field_type_from_str(desc: &str) -> Option<(TypeSignature, &str)> {
    let bytes = desc.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    match bytes[0] {
        b'B' => Some((TypeSignature::Base(BaseType::Byte), &desc[1..])),
        b'C' => Some((TypeSignature::Base(BaseType::Char), &desc[1..])),
        b'D' => Some((TypeSignature::Base(BaseType::Double), &desc[1..])),
        b'F' => Some((TypeSignature::Base(BaseType::Float), &desc[1..])),
        b'I' => Some((TypeSignature::Base(BaseType::Int), &desc[1..])),
        b'J' => Some((TypeSignature::Base(BaseType::Long), &desc[1..])),
        b'S' => Some((TypeSignature::Base(BaseType::Short), &desc[1..])),
        b'Z' => Some((TypeSignature::Base(BaseType::Boolean), &desc[1..])),
        b'V' => Some((TypeSignature::Base(BaseType::Void), &desc[1..])),
        b'L' => {
            // Find the terminating `;`
            let semi_pos = desc[1..].find(';')?;
            let class_name = &desc[1..=semi_pos];
            let fqn = class_name.replace('/', ".");
            let sig = TypeSignature::Class {
                fqn,
                type_arguments: vec![],
            };
            Some((sig, &desc[2 + semi_pos..]))
        }
        b'[' => {
            let (inner, rest) = parse_field_type_from_str(&desc[1..])?;
            Some((TypeSignature::Array(Box::new(inner)), rest))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_class_name_to_fqn() {
        assert_eq!(class_name_to_fqn("java/lang/String"), "java.lang.String");
        assert_eq!(class_name_to_fqn("com/example/Foo"), "com.example.Foo");
        assert_eq!(class_name_to_fqn("SimpleClass"), "SimpleClass");
    }

    #[test]
    fn test_parse_primitive_descriptors() {
        assert!(matches!(
            parse_field_descriptor_str("B"),
            Some(TypeSignature::Base(BaseType::Byte))
        ));
        assert!(matches!(
            parse_field_descriptor_str("C"),
            Some(TypeSignature::Base(BaseType::Char))
        ));
        assert!(matches!(
            parse_field_descriptor_str("D"),
            Some(TypeSignature::Base(BaseType::Double))
        ));
        assert!(matches!(
            parse_field_descriptor_str("F"),
            Some(TypeSignature::Base(BaseType::Float))
        ));
        assert!(matches!(
            parse_field_descriptor_str("I"),
            Some(TypeSignature::Base(BaseType::Int))
        ));
        assert!(matches!(
            parse_field_descriptor_str("J"),
            Some(TypeSignature::Base(BaseType::Long))
        ));
        assert!(matches!(
            parse_field_descriptor_str("S"),
            Some(TypeSignature::Base(BaseType::Short))
        ));
        assert!(matches!(
            parse_field_descriptor_str("Z"),
            Some(TypeSignature::Base(BaseType::Boolean))
        ));
        assert!(matches!(
            parse_field_descriptor_str("V"),
            Some(TypeSignature::Base(BaseType::Void))
        ));
    }

    #[test]
    fn test_parse_class_descriptor() {
        let sig = parse_field_descriptor_str("Ljava/lang/String;").unwrap();
        match sig {
            TypeSignature::Class {
                fqn,
                type_arguments,
            } => {
                assert_eq!(fqn, "java.lang.String");
                assert!(type_arguments.is_empty());
            }
            other => panic!("Expected Class, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_array_descriptor() {
        let sig = parse_field_descriptor_str("[I").unwrap();
        match sig {
            TypeSignature::Array(inner) => {
                assert!(matches!(*inner, TypeSignature::Base(BaseType::Int)));
            }
            other => panic!("Expected Array, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_nested_array_descriptor() {
        let sig = parse_field_descriptor_str("[[Ljava/lang/String;").unwrap();
        match sig {
            TypeSignature::Array(outer) => match *outer {
                TypeSignature::Array(inner) => match *inner {
                    TypeSignature::Class { ref fqn, .. } => {
                        assert_eq!(fqn, "java.lang.String");
                    }
                    other => panic!("Expected Class, got {other:?}"),
                },
                other => panic!("Expected Array, got {other:?}"),
            },
            other => panic!("Expected Array, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_method_descriptor_void_void() {
        let (params, ret) = parse_method_descriptor_str("()V").unwrap();
        assert!(params.is_empty());
        assert!(matches!(ret, TypeSignature::Base(BaseType::Void)));
    }

    #[test]
    fn test_parse_method_descriptor_with_params() {
        let (params, ret) = parse_method_descriptor_str("(ILjava/lang/String;)V").unwrap();
        assert_eq!(params.len(), 2);
        assert!(matches!(params[0], TypeSignature::Base(BaseType::Int)));
        match &params[1] {
            TypeSignature::Class { fqn, .. } => assert_eq!(fqn, "java.lang.String"),
            other => panic!("Expected Class, got {other:?}"),
        }
        assert!(matches!(ret, TypeSignature::Base(BaseType::Void)));
    }

    #[test]
    fn test_parse_method_descriptor_with_return() {
        let (params, ret) = parse_method_descriptor_str("()Ljava/lang/String;").unwrap();
        assert!(params.is_empty());
        match &ret {
            TypeSignature::Class { fqn, .. } => assert_eq!(fqn, "java.lang.String"),
            other => panic!("Expected Class, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_method_descriptor_complex() {
        let (params, ret) =
            parse_method_descriptor_str("(BJ[Ljava/lang/Object;D)Ljava/util/List;").unwrap();
        assert_eq!(params.len(), 4);
        assert!(matches!(params[0], TypeSignature::Base(BaseType::Byte)));
        assert!(matches!(params[1], TypeSignature::Base(BaseType::Long)));
        match &params[2] {
            TypeSignature::Array(inner) => match inner.as_ref() {
                TypeSignature::Class { fqn, .. } => assert_eq!(fqn, "java.lang.Object"),
                other => panic!("Expected Class, got {other:?}"),
            },
            other => panic!("Expected Array, got {other:?}"),
        }
        assert!(matches!(params[3], TypeSignature::Base(BaseType::Double)));
        match &ret {
            TypeSignature::Class { fqn, .. } => assert_eq!(fqn, "java.util.List"),
            other => panic!("Expected Class, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_invalid_descriptor() {
        assert!(parse_field_descriptor_str("").is_none());
        assert!(parse_field_descriptor_str("X").is_none());
        assert!(parse_field_descriptor_str("Ljava/lang/String").is_none()); // missing semicolon
        assert!(parse_field_descriptor_str("II").is_none()); // trailing chars
    }

    #[test]
    fn test_parse_invalid_method_descriptor() {
        assert!(parse_method_descriptor_str("").is_none());
        assert!(parse_method_descriptor_str("V").is_none()); // no parens
        assert!(parse_method_descriptor_str("(").is_none()); // unclosed
        assert!(parse_method_descriptor_str("()").is_none()); // no return type
    }

    #[test]
    fn test_literal_to_constant_value() {
        assert_eq!(
            literal_to_constant_value(&LiteralConstant::Integer(42)),
            ConstantValue::Int(42)
        );
        assert_eq!(
            literal_to_constant_value(&LiteralConstant::Long(123_456_789)),
            ConstantValue::Long(123_456_789)
        );
        assert_eq!(
            literal_to_constant_value(&LiteralConstant::Float(std::f32::consts::PI)),
            ConstantValue::Float(OrderedFloat(std::f32::consts::PI))
        );
        assert_eq!(
            literal_to_constant_value(&LiteralConstant::Double(std::f64::consts::E)),
            ConstantValue::Double(OrderedFloat(std::f64::consts::E))
        );
        assert_eq!(
            literal_to_constant_value(&LiteralConstant::String("hello".into())),
            ConstantValue::String("hello".to_owned())
        );
    }
}
