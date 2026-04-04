//! Annotation attribute parser for JVM bytecode.
//!
//! Converts `cafebabe`'s parsed annotation structures into the sqry stub model
//! types ([`AnnotationStub`], [`AnnotationElement`], [`AnnotationElementValue`]).
//!
//! This module handles the four annotation attribute types defined in JVMS 4.7.16-4.7.19:
//! - `RuntimeVisibleAnnotations` (`is_runtime_visible = true`)
//! - `RuntimeInvisibleAnnotations` (`is_runtime_visible = false`)
//! - `RuntimeVisibleParameterAnnotations` (`is_runtime_visible = true`)
//! - `RuntimeInvisibleParameterAnnotations` (`is_runtime_visible = false`)
//!
//! ## Type Descriptor Conversion
//!
//! Annotation type descriptors in JVM bytecode use the internal form
//! (e.g., `Lorg/springframework/web/bind/annotation/RequestMapping;`).
//! These are converted to FQN dot-notation
//! (e.g., `org.springframework.web.bind.annotation.RequestMapping`).

use cafebabe::attributes::{
    Annotation, AnnotationElement as CafeAnnotationElement,
    AnnotationElementValue as CafeAnnotationElementValue, AttributeData, ParameterAnnotation,
};
use cafebabe::descriptors::FieldType;

use crate::ClasspathError;
use crate::stub::model::{
    AnnotationElement, AnnotationElementValue, AnnotationStub, ConstantValue, OrderedFloat,
};

/// Convert a JVM internal class name (using `/` separators) to a fully qualified
/// dot-separated name.
///
/// For annotation type descriptors, `cafebabe` parses `Lorg/example/Foo;` into a
/// `FieldDescriptor` with `FieldType::Object(ClassName("org/example/Foo"))`. The
/// `ClassName` derefs to a `&str` with `/` separators, which we replace with `.`.
fn internal_name_to_fqn(internal: &str) -> String {
    internal.replace('/', ".")
}

/// Extract the fully qualified name from a cafebabe `FieldType`.
///
/// Annotation types are always `FieldType::Object(ClassName)`. For non-object
/// field types (which should never appear as annotation types), we fall back to
/// a string representation of the primitive type.
fn field_type_to_fqn(field_type: &FieldType<'_>) -> String {
    match field_type {
        FieldType::Object(class_name) => internal_name_to_fqn(class_name),
        FieldType::Byte => "byte".to_owned(),
        FieldType::Char => "char".to_owned(),
        FieldType::Double => "double".to_owned(),
        FieldType::Float => "float".to_owned(),
        FieldType::Integer => "int".to_owned(),
        FieldType::Long => "long".to_owned(),
        FieldType::Short => "short".to_owned(),
        FieldType::Boolean => "boolean".to_owned(),
    }
}

/// Convert a single cafebabe `AnnotationElementValue` to the sqry model
/// [`AnnotationElementValue`].
///
/// The JVM constant pool types `B` (byte), `C` (char), `S` (short), `Z` (boolean)
/// are all stored as `i32` in cafebabe; we map them to [`ConstantValue::Int`] since
/// the JVM representation is the same.
fn convert_element_value(
    value: &CafeAnnotationElementValue<'_>,
    is_runtime_visible: bool,
) -> AnnotationElementValue {
    match value {
        CafeAnnotationElementValue::ByteConstant(v)
        | CafeAnnotationElementValue::CharConstant(v)
        | CafeAnnotationElementValue::IntConstant(v)
        | CafeAnnotationElementValue::ShortConstant(v)
        | CafeAnnotationElementValue::BooleanConstant(v) => {
            AnnotationElementValue::Const(ConstantValue::Int(*v))
        }
        CafeAnnotationElementValue::LongConstant(v) => {
            AnnotationElementValue::Const(ConstantValue::Long(*v))
        }
        CafeAnnotationElementValue::FloatConstant(v) => {
            AnnotationElementValue::Const(ConstantValue::Float(OrderedFloat(*v)))
        }
        CafeAnnotationElementValue::DoubleConstant(v) => {
            AnnotationElementValue::Const(ConstantValue::Double(OrderedFloat(*v)))
        }
        CafeAnnotationElementValue::StringConstant(v) => {
            AnnotationElementValue::Const(ConstantValue::String(v.to_string()))
        }
        CafeAnnotationElementValue::EnumConstant {
            type_name,
            const_name,
        } => AnnotationElementValue::EnumConst {
            type_fqn: field_type_to_fqn(&type_name.field_type),
            const_name: const_name.to_string(),
        },
        CafeAnnotationElementValue::ClassLiteral { class_name } => {
            // class_name is a descriptor like "Ljava/lang/String;" or a primitive.
            // We store the FQN form (dots, no L/; wrapper).
            let fqn = class_name.replace('/', ".");
            // Strip the `L` prefix and `;` suffix if present (object type descriptor).
            let fqn = fqn
                .strip_prefix('L')
                .and_then(|s| s.strip_suffix(';'))
                .map_or_else(|| fqn.clone(), str::to_owned);
            AnnotationElementValue::ClassInfo(fqn)
        }
        CafeAnnotationElementValue::AnnotationValue(nested) => AnnotationElementValue::Annotation(
            Box::new(convert_annotation(nested, is_runtime_visible)),
        ),
        CafeAnnotationElementValue::ArrayValue(elements) => AnnotationElementValue::Array(
            elements
                .iter()
                .map(|e| convert_element_value(e, is_runtime_visible))
                .collect(),
        ),
    }
}

/// Convert a single cafebabe [`Annotation`] to an [`AnnotationStub`].
fn convert_annotation(annotation: &Annotation<'_>, is_runtime_visible: bool) -> AnnotationStub {
    let type_fqn = field_type_to_fqn(&annotation.type_descriptor.field_type);

    let elements = annotation
        .elements
        .iter()
        .map(|e| convert_element(e, is_runtime_visible))
        .collect();

    AnnotationStub {
        type_fqn,
        elements,
        is_runtime_visible,
    }
}

/// Convert a single cafebabe [`CafeAnnotationElement`] to an [`AnnotationElement`].
fn convert_element(
    element: &CafeAnnotationElement<'_>,
    is_runtime_visible: bool,
) -> AnnotationElement {
    AnnotationElement {
        name: element.name.to_string(),
        value: convert_element_value(&element.value, is_runtime_visible),
    }
}

/// Convert a list of cafebabe [`Annotation`] values into [`AnnotationStub`] records.
///
/// This is used for both `RuntimeVisibleAnnotations` and
/// `RuntimeInvisibleAnnotations` attributes. The caller controls which
/// variant by passing `is_runtime_visible`.
///
/// # Arguments
///
/// * `annotations` - Parsed annotation list from cafebabe.
/// * `is_runtime_visible` - Whether these annotations come from a
///   `RuntimeVisibleAnnotations` attribute.
#[must_use]
pub fn convert_annotations(
    annotations: &[Annotation<'_>],
    is_runtime_visible: bool,
) -> Vec<AnnotationStub> {
    annotations
        .iter()
        .map(|a| convert_annotation(a, is_runtime_visible))
        .collect()
}

/// Convert a list of cafebabe [`ParameterAnnotation`] values into nested
/// `Vec<Vec<AnnotationStub>>` records.
///
/// The outer vector is indexed by parameter position; the inner vector
/// contains the annotations for that parameter.
///
/// # Arguments
///
/// * `param_annotations` - Parsed parameter annotation list from cafebabe.
/// * `is_runtime_visible` - Whether these annotations come from a
///   `RuntimeVisibleParameterAnnotations` attribute.
#[must_use]
pub fn convert_parameter_annotations(
    param_annotations: &[ParameterAnnotation<'_>],
    is_runtime_visible: bool,
) -> Vec<Vec<AnnotationStub>> {
    param_annotations
        .iter()
        .map(|pa| convert_annotations(&pa.annotations, is_runtime_visible))
        .collect()
}

/// Extract all annotations from a cafebabe [`AttributeData`] value.
///
/// Returns `Ok(Some(stubs))` if the attribute is an annotation attribute,
/// `Ok(None)` if it is a different attribute type. Returns `Err` only on
/// internal conversion failures (currently infallible, but the signature
/// allows for future extension).
///
/// This handles:
/// - `RuntimeVisibleAnnotations` → `is_runtime_visible = true`
/// - `RuntimeInvisibleAnnotations` → `is_runtime_visible = false`
pub fn extract_annotations_from_attribute(
    attr: &AttributeData<'_>,
) -> Result<Option<Vec<AnnotationStub>>, ClasspathError> {
    match attr {
        AttributeData::RuntimeVisibleAnnotations(annotations) => {
            Ok(Some(convert_annotations(annotations, true)))
        }
        AttributeData::RuntimeInvisibleAnnotations(annotations) => {
            Ok(Some(convert_annotations(annotations, false)))
        }
        _ => Ok(None),
    }
}

/// Extract parameter annotations from a cafebabe [`AttributeData`] value.
///
/// Returns `Ok(Some(stubs))` if the attribute is a parameter annotation attribute,
/// `Ok(None)` if it is a different attribute type.
///
/// This handles:
/// - `RuntimeVisibleParameterAnnotations` → `is_runtime_visible = true`
/// - `RuntimeInvisibleParameterAnnotations` → `is_runtime_visible = false`
pub fn extract_parameter_annotations_from_attribute(
    attr: &AttributeData<'_>,
) -> Result<Option<Vec<Vec<AnnotationStub>>>, ClasspathError> {
    match attr {
        AttributeData::RuntimeVisibleParameterAnnotations(param_annotations) => {
            Ok(Some(convert_parameter_annotations(param_annotations, true)))
        }
        AttributeData::RuntimeInvisibleParameterAnnotations(param_annotations) => Ok(Some(
            convert_parameter_annotations(param_annotations, false),
        )),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cafebabe::attributes::{
        Annotation, AnnotationElement as CafeAnnotationElement,
        AnnotationElementValue as CafeAnnotationElementValue, AttributeData, ParameterAnnotation,
    };
    use cafebabe::descriptors::{ClassName, FieldDescriptor, FieldType};
    use std::borrow::Cow;

    /// Helper: build a `FieldDescriptor` for an object type from an internal name.
    fn object_descriptor(internal_name: &str) -> FieldDescriptor<'_> {
        FieldDescriptor {
            dimensions: 0,
            field_type: FieldType::Object(
                ClassName::try_from(Cow::Borrowed(internal_name)).expect("valid class name"),
            ),
        }
    }

    /// Helper: build a cafebabe `Annotation` with no elements.
    fn marker_annotation(internal_name: &str) -> Annotation<'_> {
        Annotation {
            type_descriptor: object_descriptor(internal_name),
            elements: vec![],
        }
    }

    // -- Test 1: Simple marker annotation (no elements) ---------------------

    #[test]
    fn simple_marker_annotation() {
        let cafe_ann = marker_annotation("java/lang/Override");
        let stubs = convert_annotations(&[cafe_ann], true);

        assert_eq!(stubs.len(), 1);
        assert_eq!(stubs[0].type_fqn, "java.lang.Override");
        assert!(stubs[0].elements.is_empty());
        assert!(stubs[0].is_runtime_visible);
    }

    // -- Test 2: Annotation with string value --------------------------------

    #[test]
    fn annotation_with_string_value() {
        let cafe_ann = Annotation {
            type_descriptor: object_descriptor(
                "org/springframework/web/bind/annotation/RequestMapping",
            ),
            elements: vec![CafeAnnotationElement {
                name: Cow::Borrowed("value"),
                value: CafeAnnotationElementValue::StringConstant(Cow::Borrowed("/api")),
            }],
        };

        let stubs = convert_annotations(&[cafe_ann], true);

        assert_eq!(stubs.len(), 1);
        assert_eq!(
            stubs[0].type_fqn,
            "org.springframework.web.bind.annotation.RequestMapping"
        );
        assert_eq!(stubs[0].elements.len(), 1);
        assert_eq!(stubs[0].elements[0].name, "value");
        assert!(matches!(
            &stubs[0].elements[0].value,
            AnnotationElementValue::Const(ConstantValue::String(s)) if s == "/api"
        ));
    }

    // -- Test 3: Annotation with enum constant --------------------------------

    #[test]
    fn annotation_with_enum_constant() {
        let cafe_ann = Annotation {
            type_descriptor: object_descriptor(
                "org/springframework/web/bind/annotation/RequestMapping",
            ),
            elements: vec![CafeAnnotationElement {
                name: Cow::Borrowed("method"),
                value: CafeAnnotationElementValue::EnumConstant {
                    type_name: object_descriptor(
                        "org/springframework/web/bind/annotation/RequestMethod",
                    ),
                    const_name: Cow::Borrowed("GET"),
                },
            }],
        };

        let stubs = convert_annotations(&[cafe_ann], true);

        assert_eq!(stubs.len(), 1);
        assert_eq!(stubs[0].elements.len(), 1);
        assert!(matches!(
            &stubs[0].elements[0].value,
            AnnotationElementValue::EnumConst { type_fqn, const_name }
                if type_fqn == "org.springframework.web.bind.annotation.RequestMethod"
                && const_name == "GET"
        ));
    }

    // -- Test 4: Annotation with array value ----------------------------------

    #[test]
    fn annotation_with_array_value() {
        let cafe_ann = Annotation {
            type_descriptor: object_descriptor(
                "org/springframework/context/annotation/ComponentScan",
            ),
            elements: vec![CafeAnnotationElement {
                name: Cow::Borrowed("basePackages"),
                value: CafeAnnotationElementValue::ArrayValue(vec![
                    CafeAnnotationElementValue::StringConstant(Cow::Borrowed("com.example.a")),
                    CafeAnnotationElementValue::StringConstant(Cow::Borrowed("com.example.b")),
                ]),
            }],
        };

        let stubs = convert_annotations(&[cafe_ann], false);

        assert_eq!(stubs.len(), 1);
        assert!(!stubs[0].is_runtime_visible);
        assert_eq!(stubs[0].elements[0].name, "basePackages");
        match &stubs[0].elements[0].value {
            AnnotationElementValue::Array(items) => {
                assert_eq!(items.len(), 2);
                assert!(matches!(
                    &items[0],
                    AnnotationElementValue::Const(ConstantValue::String(s)) if s == "com.example.a"
                ));
                assert!(matches!(
                    &items[1],
                    AnnotationElementValue::Const(ConstantValue::String(s)) if s == "com.example.b"
                ));
            }
            other => panic!("expected Array, got {other:?}"),
        }
    }

    // -- Test 5: Annotation with class literal --------------------------------

    #[test]
    fn annotation_with_class_literal() {
        let cafe_ann = Annotation {
            type_descriptor: object_descriptor("javax/persistence/Type"),
            elements: vec![CafeAnnotationElement {
                name: Cow::Borrowed("value"),
                value: CafeAnnotationElementValue::ClassLiteral {
                    class_name: Cow::Borrowed("Ljava/lang/String;"),
                },
            }],
        };

        let stubs = convert_annotations(&[cafe_ann], true);

        assert_eq!(stubs[0].elements[0].name, "value");
        assert!(matches!(
            &stubs[0].elements[0].value,
            AnnotationElementValue::ClassInfo(fqn) if fqn == "java.lang.String"
        ));
    }

    // -- Test 6: Nested annotation -------------------------------------------

    #[test]
    fn nested_annotation() {
        let inner = Annotation {
            type_descriptor: object_descriptor("javax/validation/constraints/Size"),
            elements: vec![
                CafeAnnotationElement {
                    name: Cow::Borrowed("min"),
                    value: CafeAnnotationElementValue::IntConstant(1),
                },
                CafeAnnotationElement {
                    name: Cow::Borrowed("max"),
                    value: CafeAnnotationElementValue::IntConstant(100),
                },
            ],
        };

        let outer = Annotation {
            type_descriptor: object_descriptor("javax/validation/Valid"),
            elements: vec![CafeAnnotationElement {
                name: Cow::Borrowed("payload"),
                value: CafeAnnotationElementValue::AnnotationValue(inner),
            }],
        };

        let stubs = convert_annotations(&[outer], true);

        assert_eq!(stubs.len(), 1);
        assert_eq!(stubs[0].type_fqn, "javax.validation.Valid");
        match &stubs[0].elements[0].value {
            AnnotationElementValue::Annotation(nested) => {
                assert_eq!(nested.type_fqn, "javax.validation.constraints.Size");
                assert!(nested.is_runtime_visible);
                assert_eq!(nested.elements.len(), 2);
                assert_eq!(nested.elements[0].name, "min");
                assert!(matches!(
                    &nested.elements[0].value,
                    AnnotationElementValue::Const(ConstantValue::Int(1))
                ));
                assert_eq!(nested.elements[1].name, "max");
                assert!(matches!(
                    &nested.elements[1].value,
                    AnnotationElementValue::Const(ConstantValue::Int(100))
                ));
            }
            other => panic!("expected Annotation, got {other:?}"),
        }
    }

    // -- Test 7: Parameter annotations ----------------------------------------

    #[test]
    fn parameter_annotations() {
        let param0_annotations = ParameterAnnotation {
            annotations: vec![Annotation {
                type_descriptor: object_descriptor("javax/annotation/Nonnull"),
                elements: vec![],
            }],
        };
        let param1_annotations = ParameterAnnotation {
            annotations: vec![
                Annotation {
                    type_descriptor: object_descriptor("javax/validation/constraints/NotNull"),
                    elements: vec![],
                },
                Annotation {
                    type_descriptor: object_descriptor("javax/validation/constraints/Size"),
                    elements: vec![CafeAnnotationElement {
                        name: Cow::Borrowed("max"),
                        value: CafeAnnotationElementValue::IntConstant(255),
                    }],
                },
            ],
        };
        let param2_no_annotations = ParameterAnnotation {
            annotations: vec![],
        };

        let result = convert_parameter_annotations(
            &[
                param0_annotations,
                param1_annotations,
                param2_no_annotations,
            ],
            true,
        );

        assert_eq!(result.len(), 3);
        // Parameter 0: one annotation
        assert_eq!(result[0].len(), 1);
        assert_eq!(result[0][0].type_fqn, "javax.annotation.Nonnull");
        assert!(result[0][0].is_runtime_visible);

        // Parameter 1: two annotations
        assert_eq!(result[1].len(), 2);
        assert_eq!(
            result[1][0].type_fqn,
            "javax.validation.constraints.NotNull"
        );
        assert_eq!(result[1][1].type_fqn, "javax.validation.constraints.Size");
        assert_eq!(result[1][1].elements.len(), 1);
        assert_eq!(result[1][1].elements[0].name, "max");

        // Parameter 2: no annotations
        assert!(result[2].is_empty());
    }

    // -- Test 8: Extract from AttributeData -----------------------------------

    #[test]
    fn extract_visible_annotations_from_attribute_data() {
        let annotations = vec![marker_annotation("java/lang/Deprecated")];
        let attr = AttributeData::RuntimeVisibleAnnotations(annotations);

        let result = extract_annotations_from_attribute(&attr).unwrap();
        let stubs = result.expect("should return Some for annotation attribute");
        assert_eq!(stubs.len(), 1);
        assert_eq!(stubs[0].type_fqn, "java.lang.Deprecated");
        assert!(stubs[0].is_runtime_visible);
    }

    #[test]
    fn extract_invisible_annotations_from_attribute_data() {
        let annotations = vec![marker_annotation("javax/annotation/Generated")];
        let attr = AttributeData::RuntimeInvisibleAnnotations(annotations);

        let result = extract_annotations_from_attribute(&attr).unwrap();
        let stubs = result.expect("should return Some");
        assert_eq!(stubs.len(), 1);
        assert!(!stubs[0].is_runtime_visible);
    }

    #[test]
    fn extract_returns_none_for_non_annotation_attribute() {
        let attr = AttributeData::Deprecated;
        let result = extract_annotations_from_attribute(&attr).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn extract_visible_parameter_annotations() {
        let param_annotations = vec![ParameterAnnotation {
            annotations: vec![marker_annotation("javax/annotation/Nullable")],
        }];
        let attr = AttributeData::RuntimeVisibleParameterAnnotations(param_annotations);

        let result = extract_parameter_annotations_from_attribute(&attr).unwrap();
        let stubs = result.expect("should return Some");
        assert_eq!(stubs.len(), 1);
        assert_eq!(stubs[0].len(), 1);
        assert_eq!(stubs[0][0].type_fqn, "javax.annotation.Nullable");
        assert!(stubs[0][0].is_runtime_visible);
    }

    #[test]
    fn extract_invisible_parameter_annotations() {
        let param_annotations = vec![ParameterAnnotation {
            annotations: vec![marker_annotation("javax/annotation/Nonnull")],
        }];
        let attr = AttributeData::RuntimeInvisibleParameterAnnotations(param_annotations);

        let result = extract_parameter_annotations_from_attribute(&attr).unwrap();
        let stubs = result.expect("should return Some");
        assert_eq!(stubs[0][0].type_fqn, "javax.annotation.Nonnull");
        assert!(!stubs[0][0].is_runtime_visible);
    }

    // -- Test: Numeric constant types -----------------------------------------

    #[test]
    fn byte_char_short_boolean_constants_map_to_int() {
        let values = vec![
            CafeAnnotationElementValue::ByteConstant(42),
            CafeAnnotationElementValue::CharConstant(65),
            CafeAnnotationElementValue::ShortConstant(1000),
            CafeAnnotationElementValue::BooleanConstant(1),
        ];

        for v in &values {
            let result = convert_element_value(v, true);
            assert!(
                matches!(result, AnnotationElementValue::Const(ConstantValue::Int(_))),
                "expected Int variant for {v:?}"
            );
        }
    }

    #[test]
    fn long_constant() {
        let v = CafeAnnotationElementValue::LongConstant(i64::MAX);
        let result = convert_element_value(&v, true);
        assert!(matches!(
            result,
            AnnotationElementValue::Const(ConstantValue::Long(i64::MAX))
        ));
    }

    #[test]
    fn float_constant() {
        let v = CafeAnnotationElementValue::FloatConstant(std::f32::consts::PI);
        let result = convert_element_value(&v, true);
        match result {
            AnnotationElementValue::Const(ConstantValue::Float(f)) => {
                assert!((f.0 - std::f32::consts::PI).abs() < f32::EPSILON);
            }
            other => panic!("expected Float, got {other:?}"),
        }
    }

    #[test]
    fn double_constant() {
        let v = CafeAnnotationElementValue::DoubleConstant(std::f64::consts::E);
        let result = convert_element_value(&v, true);
        match result {
            AnnotationElementValue::Const(ConstantValue::Double(d)) => {
                assert!((d.0 - std::f64::consts::E).abs() < f64::EPSILON);
            }
            other => panic!("expected Double, got {other:?}"),
        }
    }

    // -- Test: Internal name conversion ---------------------------------------

    #[test]
    fn internal_name_conversion() {
        assert_eq!(
            internal_name_to_fqn("org/springframework/web/bind/annotation/RequestMapping"),
            "org.springframework.web.bind.annotation.RequestMapping"
        );
        assert_eq!(internal_name_to_fqn("java/lang/Object"), "java.lang.Object");
        assert_eq!(internal_name_to_fqn("Foo"), "Foo");
    }

    // -- Test: Class literal with primitive descriptor -----------------------

    #[test]
    fn class_literal_primitive_descriptor() {
        // Primitive class literals like `int.class` use descriptor "I" etc.
        let v = CafeAnnotationElementValue::ClassLiteral {
            class_name: Cow::Borrowed("I"),
        };
        let result = convert_element_value(&v, true);
        assert!(matches!(
            result,
            AnnotationElementValue::ClassInfo(ref s) if s == "I"
        ));
    }

    // -- Test: Multiple annotations in one attribute -------------------------

    #[test]
    fn multiple_annotations() {
        let annotations = vec![
            marker_annotation("java/lang/Override"),
            marker_annotation("java/lang/Deprecated"),
            marker_annotation("java/lang/SuppressWarnings"),
        ];

        let stubs = convert_annotations(&annotations, false);
        assert_eq!(stubs.len(), 3);
        assert_eq!(stubs[0].type_fqn, "java.lang.Override");
        assert_eq!(stubs[1].type_fqn, "java.lang.Deprecated");
        assert_eq!(stubs[2].type_fqn, "java.lang.SuppressWarnings");
        for stub in &stubs {
            assert!(!stub.is_runtime_visible);
        }
    }

    // -- Test: Empty annotation list -----------------------------------------

    #[test]
    fn empty_annotation_list() {
        let stubs = convert_annotations(&[], true);
        assert!(stubs.is_empty());
    }

    // -- Test: Empty parameter annotation list --------------------------------

    #[test]
    fn empty_parameter_annotation_list() {
        let stubs = convert_parameter_annotations(&[], true);
        assert!(stubs.is_empty());
    }

    // -- Test: Complex annotation with mixed element types --------------------

    #[test]
    fn complex_annotation_with_mixed_elements() {
        let ann = Annotation {
            type_descriptor: object_descriptor(
                "org/springframework/web/bind/annotation/RequestMapping",
            ),
            elements: vec![
                CafeAnnotationElement {
                    name: Cow::Borrowed("value"),
                    value: CafeAnnotationElementValue::ArrayValue(vec![
                        CafeAnnotationElementValue::StringConstant(Cow::Borrowed("/api/users")),
                    ]),
                },
                CafeAnnotationElement {
                    name: Cow::Borrowed("method"),
                    value: CafeAnnotationElementValue::EnumConstant {
                        type_name: object_descriptor(
                            "org/springframework/web/bind/annotation/RequestMethod",
                        ),
                        const_name: Cow::Borrowed("GET"),
                    },
                },
                CafeAnnotationElement {
                    name: Cow::Borrowed("produces"),
                    value: CafeAnnotationElementValue::ClassLiteral {
                        class_name: Cow::Borrowed("Ljava/lang/String;"),
                    },
                },
                CafeAnnotationElement {
                    name: Cow::Borrowed("timeout"),
                    value: CafeAnnotationElementValue::IntConstant(30),
                },
            ],
        };

        let stubs = convert_annotations(&[ann], true);
        assert_eq!(stubs.len(), 1);
        let stub = &stubs[0];
        assert_eq!(stub.elements.len(), 4);

        // Array element
        assert!(matches!(
            &stub.elements[0].value,
            AnnotationElementValue::Array(items) if items.len() == 1
        ));
        // Enum element
        assert!(matches!(
            &stub.elements[1].value,
            AnnotationElementValue::EnumConst { const_name, .. } if const_name == "GET"
        ));
        // Class element
        assert!(matches!(
            &stub.elements[2].value,
            AnnotationElementValue::ClassInfo(fqn) if fqn == "java.lang.String"
        ));
        // Int element
        assert!(matches!(
            &stub.elements[3].value,
            AnnotationElementValue::Const(ConstantValue::Int(30))
        ));
    }
}
