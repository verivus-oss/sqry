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
