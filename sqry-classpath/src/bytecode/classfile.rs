//! Class file parser: converts `.class` bytes into [`ClassStub`] records.
//!
//! Uses the `cafebabe` crate for low-level JVM bytecode parsing and converts
//! the parsed representation into our stub model types. This module handles
//! the base class parsing: class metadata, methods, fields, superclass,
//! interfaces, inner classes, enum constants, record components, and source
//! file extraction.
//!
//! Generics (U05), annotations (U06), lambdas (U07a), and modules (U07b) are
//! handled by separate enrichment parsers that post-process the stub.

use cafebabe::attributes::AttributeData;
use cafebabe::{ClassAccessFlags, FieldAccessFlags, MethodAccessFlags, ParseOptions};

use crate::stub::model::{
    AccessFlags, ClassKind, ClassStub, FieldStub, InnerClassEntry, MethodStub, RecordComponent,
};
use crate::{ClasspathError, ClasspathResult};

use super::constants::{
    class_name_to_fqn, extract_constant_value, extract_method_parameter_names, extract_source_file,
    method_descriptor_to_types,
};

// ---------------------------------------------------------------------------
// Access flag constants for filtering
// ---------------------------------------------------------------------------

/// ACC_BRIDGE for methods (0x0040).
const METHOD_ACC_BRIDGE: u16 = 0x0040;
/// ACC_SYNTHETIC for methods (0x1000).
const METHOD_ACC_SYNTHETIC: u16 = 0x1000;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a `.class` file's bytes into a [`ClassStub`].
///
/// This function handles the base class parsing: class metadata, methods, fields,
/// superclass, interfaces, inner classes, enum constants, record components, and
/// source file. It does NOT parse generics (U05), annotations (U06), lambdas (U07a),
/// or modules (U07b) — those are handled by separate parsers that enrich the stub.
///
/// # Errors
///
/// Returns [`ClasspathError::BytecodeParseError`] if the bytes cannot be parsed
/// as a valid class file. Individual method/field parse failures are logged as
/// warnings and the member is skipped.
pub fn parse_class(bytes: &[u8]) -> ClasspathResult<ClassStub> {
    // Parse with bytecode parsing disabled — we only need structure, not opcodes.
    let mut opts = ParseOptions::default();
    opts.parse_bytecode(false);

    let class_file = cafebabe::parse_class_with_options(bytes, &opts).map_err(|e| {
        ClasspathError::BytecodeParseError {
            class_name: String::from("<unknown>"),
            reason: e.to_string(),
        }
    })?;

    let this_class_raw = class_file.this_class.to_string();

    // Skip module-info and package-info classes — these are handled by U07b.
    if this_class_raw.ends_with("module-info") || this_class_raw.ends_with("package-info") {
        return Err(ClasspathError::BytecodeParseError {
            class_name: class_name_to_fqn(&this_class_raw),
            reason: "module-info and package-info classes are handled by U07b".to_owned(),
        });
    }

    let fqn = class_name_to_fqn(&this_class_raw);
    let name = extract_simple_name(&fqn);
    let access = convert_class_access_flags(class_file.access_flags);
    let kind = determine_class_kind(class_file.access_flags, &class_file.attributes);

    let superclass = class_file
        .super_class
        .as_ref()
        .map(|sc| class_name_to_fqn(sc));

    let interfaces: Vec<String> = class_file
        .interfaces
        .iter()
        .map(|i| class_name_to_fqn(i))
        .collect();

    // Parse methods (skipping synthetic and bridge methods).
    let methods = parse_methods(&class_file.methods, &fqn);

    // Parse fields.
    let fields = parse_fields(&class_file.fields, &fqn);

    // Extract enum constants (static final fields whose type matches the class).
    let enum_constants = if kind == ClassKind::Enum {
        extract_enum_constants(&class_file.fields, &this_class_raw)
    } else {
        vec![]
    };

    // Extract inner classes from the InnerClasses attribute.
    let inner_classes = extract_inner_classes(&class_file.attributes);

    // Extract record components from the Record attribute.
    let record_components = extract_record_components(&class_file.attributes);

    // Extract source file name.
    let source_file = extract_source_file(&class_file.attributes);

    Ok(ClassStub {
        fqn,
        name,
        kind,
        access,
        superclass,
        interfaces,
        methods,
        fields,
        annotations: vec![],     // Populated by U06 enrichment parser.
        generic_signature: None, // Populated by U05 enrichment parser.
        inner_classes,
        lambda_targets: vec![], // Populated by U07a enrichment parser.
        module: None,           // Populated by U07b enrichment parser.
        record_components,
        enum_constants,
        source_file,
        source_jar: None,      // Set by scan_jar() after parsing.
        kotlin_metadata: None, // Populated by Kotlin metadata decoder.
        scala_signature: None, // Populated by Scala signature decoder.
    })
}

// ---------------------------------------------------------------------------
// Class metadata helpers
// ---------------------------------------------------------------------------

/// Extract the simple name from a fully qualified name.
///
/// For `"java.util.HashMap"` returns `"HashMap"`.
/// For `"java.util.Map.Entry"` returns `"Entry"`.
fn extract_simple_name(fqn: &str) -> String {
    // Inner classes use `$` separator in bytecode but `.` in our FQN.
    // The simple name is the last segment after the last `.`.
    fqn.rsplit('.').next().unwrap_or(fqn).to_owned()
}

/// Convert `cafebabe` class access flags to our [`AccessFlags`].
fn convert_class_access_flags(flags: ClassAccessFlags) -> AccessFlags {
    AccessFlags::new(flags.bits())
}

/// Convert `cafebabe` method access flags to our [`AccessFlags`].
fn convert_method_access_flags(flags: MethodAccessFlags) -> AccessFlags {
    AccessFlags::new(flags.bits())
}

/// Convert `cafebabe` field access flags to our [`AccessFlags`].
fn convert_field_access_flags(flags: FieldAccessFlags) -> AccessFlags {
    AccessFlags::new(flags.bits())
}

/// Determine the [`ClassKind`] from access flags and attributes.
///
/// Order of precedence (per JVMS):
/// 1. `ACC_MODULE` → `Module`
/// 2. `ACC_ANNOTATION` + `ACC_INTERFACE` → `Annotation`
/// 3. `ACC_ENUM` → `Enum`
/// 4. `ACC_INTERFACE` → `Interface`
/// 5. Has `Record` attribute → `Record`
/// 6. Otherwise → `Class`
fn determine_class_kind(
    flags: ClassAccessFlags,
    attrs: &[cafebabe::attributes::AttributeInfo<'_>],
) -> ClassKind {
    if flags.contains(ClassAccessFlags::MODULE) {
        return ClassKind::Module;
    }
    if flags.contains(ClassAccessFlags::ANNOTATION) && flags.contains(ClassAccessFlags::INTERFACE) {
        return ClassKind::Annotation;
    }
    if flags.contains(ClassAccessFlags::ENUM) {
        return ClassKind::Enum;
    }
    if flags.contains(ClassAccessFlags::INTERFACE) {
        return ClassKind::Interface;
    }
    // Check for Record attribute (Java 16+).
    let has_record = attrs
        .iter()
        .any(|a| matches!(&a.data, AttributeData::Record(_)));
    if has_record {
        return ClassKind::Record;
    }
    ClassKind::Class
}

// ---------------------------------------------------------------------------
// Method parsing
// ---------------------------------------------------------------------------

/// Parse all methods from a class file, filtering out synthetic and bridge methods.
fn parse_methods(methods: &[cafebabe::MethodInfo<'_>], class_fqn: &str) -> Vec<MethodStub> {
    let mut result = Vec::with_capacity(methods.len());
    for method in methods {
        let raw_bits = method.access_flags.bits();

        // Skip synthetic and bridge methods.
        if raw_bits & METHOD_ACC_BRIDGE != 0 || raw_bits & METHOD_ACC_SYNTHETIC != 0 {
            continue;
        }

        match parse_single_method(method) {
            Ok(stub) => result.push(stub),
            Err(e) => {
                log::warn!(
                    "Skipping method '{}' in class '{}': {}",
                    method.name,
                    class_fqn,
                    e
                );
            }
        }
    }
    result
}

/// Parse a single method into a [`MethodStub`].
fn parse_single_method(method: &cafebabe::MethodInfo<'_>) -> ClasspathResult<MethodStub> {
    let name = method.name.to_string();
    let access = convert_method_access_flags(method.access_flags);
    let descriptor = method.descriptor.to_string();

    let (parameter_types, return_type) = method_descriptor_to_types(&method.descriptor);

    // Extract parameter names from MethodParameters attribute.
    let parameter_names = extract_method_parameter_names(&method.attributes);

    Ok(MethodStub {
        name,
        access,
        descriptor,
        generic_signature: None,       // Populated by U05 enrichment parser.
        annotations: vec![],           // Populated by U06 enrichment parser.
        parameter_annotations: vec![], // Populated by U06 enrichment parser.
        parameter_names,
        return_type,
        parameter_types,
    })
}

// ---------------------------------------------------------------------------
// Field parsing
// ---------------------------------------------------------------------------

/// Parse all fields from a class file.
fn parse_fields(fields: &[cafebabe::FieldInfo<'_>], class_fqn: &str) -> Vec<FieldStub> {
    let mut result = Vec::with_capacity(fields.len());
    for field in fields {
        match parse_single_field(field) {
            Ok(stub) => result.push(stub),
            Err(e) => {
                log::warn!(
                    "Skipping field '{}' in class '{}': {}",
                    field.name,
                    class_fqn,
                    e
                );
            }
        }
    }
    result
}

/// Parse a single field into a [`FieldStub`].
fn parse_single_field(field: &cafebabe::FieldInfo<'_>) -> ClasspathResult<FieldStub> {
    let name = field.name.to_string();
    let access = convert_field_access_flags(field.access_flags);
    let descriptor = field.descriptor.to_string();

    // Extract constant value for static final fields.
    let constant_value = if access.is_static() && access.is_final() {
        extract_constant_value(&field.attributes)
    } else {
        None
    };

    Ok(FieldStub {
        name,
        access,
        descriptor,
        generic_signature: None, // Populated by U05 enrichment parser.
        annotations: vec![],     // Populated by U06 enrichment parser.
        constant_value,
    })
}

// ---------------------------------------------------------------------------
// Enum constant extraction
// ---------------------------------------------------------------------------

/// Extract enum constant names from fields.
///
/// Enum constants are `static final` fields whose type descriptor matches the
/// containing class (and have `ACC_ENUM` set).
fn extract_enum_constants(
    fields: &[cafebabe::FieldInfo<'_>],
    this_class_internal: &str,
) -> Vec<String> {
    let expected_descriptor = format!("L{this_class_internal};");
    fields
        .iter()
        .filter(|f| {
            let bits = f.access_flags.bits();
            // Must be static, final, and marked as enum constant.
            bits & FieldAccessFlags::STATIC.bits() != 0
                && bits & FieldAccessFlags::FINAL.bits() != 0
                && bits & FieldAccessFlags::ENUM.bits() != 0
                && f.descriptor.to_string() == expected_descriptor
        })
        .map(|f| f.name.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Inner class extraction
// ---------------------------------------------------------------------------

/// Extract inner class entries from the `InnerClasses` attribute.
fn extract_inner_classes(
    attrs: &[cafebabe::attributes::AttributeInfo<'_>],
) -> Vec<InnerClassEntry> {
    let mut result = Vec::new();
    for attr in attrs {
        if let AttributeData::InnerClasses(entries) = &attr.data {
            for entry in entries {
                result.push(InnerClassEntry {
                    inner_fqn: class_name_to_fqn(&entry.inner_class_info),
                    outer_fqn: entry
                        .outer_class_info
                        .as_ref()
                        .map(|o| class_name_to_fqn(o)),
                    inner_name: entry.inner_name.as_ref().map(|n| n.to_string()),
                    access: AccessFlags::new(entry.access_flags.bits()),
                });
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Record component extraction
// ---------------------------------------------------------------------------

/// Extract record components from the `Record` attribute (Java 16+).
fn extract_record_components(
    attrs: &[cafebabe::attributes::AttributeInfo<'_>],
) -> Vec<RecordComponent> {
    let mut result = Vec::new();
    for attr in attrs {
        if let AttributeData::Record(components) = &attr.data {
            for comp in components {
                result.push(RecordComponent {
                    name: comp.name.to_string(),
                    descriptor: comp.descriptor.to_string(),
                    generic_signature: None, // Populated by U05 enrichment parser.
                    annotations: vec![],     // Populated by U06 enrichment parser.
                });
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::model::{BaseType, ConstantValue, TypeSignature};

    // -----------------------------------------------------------------------
    // Minimal class file builder for tests
    // -----------------------------------------------------------------------

    /// Builder for constructing minimal valid Java class file bytes.
    ///
    /// Produces valid class files that `cafebabe` can parse, with configurable
    /// class name, access flags, superclass, interfaces, fields, methods, and
    /// attributes.
    struct ClassFileBuilder {
        /// Major version (52 = Java 8).
        major_version: u16,
        /// Constant pool entries (raw bytes, each entry prefixed with tag byte).
        cp_entries: Vec<Vec<u8>>,
        /// Access flags for the class.
        access_flags: u16,
        /// Constant pool index of this class.
        this_class_idx: u16,
        /// Constant pool index of super class (0 for java/lang/Object).
        super_class_idx: u16,
        /// Interface constant pool indices.
        interface_indices: Vec<u16>,
        /// Raw field bytes.
        fields: Vec<Vec<u8>>,
        /// Raw method bytes.
        methods: Vec<Vec<u8>>,
        /// Raw attribute bytes.
        attributes: Vec<Vec<u8>>,
    }

    impl ClassFileBuilder {
        /// Create a builder with a given class name and default superclass
        /// (`java/lang/Object`).
        fn new(class_name: &str) -> Self {
            let mut builder = Self {
                major_version: 52,
                cp_entries: Vec::new(),
                access_flags: 0x0021, // ACC_PUBLIC | ACC_SUPER
                this_class_idx: 0,
                super_class_idx: 0,
                interface_indices: Vec::new(),
                fields: Vec::new(),
                methods: Vec::new(),
                attributes: Vec::new(),
            };
            // Add class name and java/lang/Object to constant pool.
            let class_name_idx = builder.add_utf8(class_name);
            builder.this_class_idx = builder.add_class(class_name_idx);
            let object_name_idx = builder.add_utf8("java/lang/Object");
            builder.super_class_idx = builder.add_class(object_name_idx);
            builder
        }

        /// Add a UTF-8 constant pool entry. Returns 1-based index.
        fn add_utf8(&mut self, s: &str) -> u16 {
            let mut entry = vec![1u8]; // CONSTANT_Utf8
            let bytes = s.as_bytes();
            entry.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
            entry.extend_from_slice(bytes);
            self.cp_entries.push(entry);
            self.cp_entries.len() as u16
        }

        /// Add a Class constant pool entry. Returns 1-based index.
        fn add_class(&mut self, name_idx: u16) -> u16 {
            let mut entry = vec![7u8]; // CONSTANT_Class
            entry.extend_from_slice(&name_idx.to_be_bytes());
            self.cp_entries.push(entry);
            self.cp_entries.len() as u16
        }

        /// Add an Integer constant pool entry. Returns 1-based index.
        fn add_integer(&mut self, value: i32) -> u16 {
            let mut entry = vec![3u8]; // CONSTANT_Integer
            entry.extend_from_slice(&value.to_be_bytes());
            self.cp_entries.push(entry);
            self.cp_entries.len() as u16
        }

        /// Add a String constant pool entry. Returns 1-based index.
        fn add_string(&mut self, utf8_idx: u16) -> u16 {
            let mut entry = vec![8u8]; // CONSTANT_String
            entry.extend_from_slice(&utf8_idx.to_be_bytes());
            self.cp_entries.push(entry);
            self.cp_entries.len() as u16
        }

        /// Set access flags.
        fn access_flags(mut self, flags: u16) -> Self {
            self.access_flags = flags;
            self
        }

        /// Set superclass to none (for java.lang.Object itself).
        fn no_superclass(mut self) -> Self {
            self.super_class_idx = 0;
            self
        }

        /// Add a superclass by name. Replaces the default `java/lang/Object`.
        fn superclass(mut self, name: &str) -> Self {
            let name_idx = self.add_utf8(name);
            self.super_class_idx = self.add_class(name_idx);
            self
        }

        /// Add an interface.
        fn add_interface(&mut self, name: &str) -> &mut Self {
            let name_idx = self.add_utf8(name);
            let class_idx = self.add_class(name_idx);
            self.interface_indices.push(class_idx);
            self
        }

        /// Add a field with access flags and descriptor.
        fn add_field(
            &mut self,
            name: &str,
            descriptor: &str,
            access_flags: u16,
            constant_value_cp_idx: Option<u16>,
        ) -> &mut Self {
            let name_idx = self.add_utf8(name);
            let desc_idx = self.add_utf8(descriptor);

            let mut field_bytes = Vec::new();
            field_bytes.extend_from_slice(&access_flags.to_be_bytes());
            field_bytes.extend_from_slice(&name_idx.to_be_bytes());
            field_bytes.extend_from_slice(&desc_idx.to_be_bytes());

            if let Some(cv_idx) = constant_value_cp_idx {
                // 1 attribute: ConstantValue
                let attr_name_idx = self.add_utf8("ConstantValue");
                field_bytes.extend_from_slice(&1u16.to_be_bytes()); // attributes_count
                field_bytes.extend_from_slice(&attr_name_idx.to_be_bytes());
                field_bytes.extend_from_slice(&2u32.to_be_bytes()); // attribute_length
                field_bytes.extend_from_slice(&cv_idx.to_be_bytes());
            } else {
                field_bytes.extend_from_slice(&0u16.to_be_bytes()); // attributes_count = 0
            }

            self.fields.push(field_bytes);
            self
        }

        /// Add a method with access flags and descriptor.
        fn add_method(&mut self, name: &str, descriptor: &str, access_flags: u16) -> &mut Self {
            let name_idx = self.add_utf8(name);
            let desc_idx = self.add_utf8(descriptor);

            let mut method_bytes = Vec::new();
            method_bytes.extend_from_slice(&access_flags.to_be_bytes());
            method_bytes.extend_from_slice(&name_idx.to_be_bytes());
            method_bytes.extend_from_slice(&desc_idx.to_be_bytes());
            method_bytes.extend_from_slice(&0u16.to_be_bytes()); // attributes_count = 0

            self.methods.push(method_bytes);
            self
        }

        /// Add a method with MethodParameters attribute.
        fn add_method_with_params(
            &mut self,
            name: &str,
            descriptor: &str,
            access_flags: u16,
            param_names: &[&str],
        ) -> &mut Self {
            let name_idx = self.add_utf8(name);
            let desc_idx = self.add_utf8(descriptor);

            let mut method_bytes = Vec::new();
            method_bytes.extend_from_slice(&access_flags.to_be_bytes());
            method_bytes.extend_from_slice(&name_idx.to_be_bytes());
            method_bytes.extend_from_slice(&desc_idx.to_be_bytes());

            // Build MethodParameters attribute.
            let attr_name_idx = self.add_utf8("MethodParameters");
            let param_name_indices: Vec<u16> =
                param_names.iter().map(|pn| self.add_utf8(pn)).collect();

            // 1 attribute
            method_bytes.extend_from_slice(&1u16.to_be_bytes());
            method_bytes.extend_from_slice(&attr_name_idx.to_be_bytes());
            // attribute_length: 1 byte (parameters_count) + 4 bytes per param
            let attr_length = 1 + param_name_indices.len() as u32 * 4;
            method_bytes.extend_from_slice(&attr_length.to_be_bytes());
            method_bytes.push(param_name_indices.len() as u8);
            for idx in &param_name_indices {
                method_bytes.extend_from_slice(&idx.to_be_bytes());
                method_bytes.extend_from_slice(&0u16.to_be_bytes()); // access_flags
            }

            self.methods.push(method_bytes);
            self
        }

        /// Add an InnerClasses attribute.
        fn add_inner_classes_attribute(
            &mut self,
            entries: &[(&str, Option<&str>, Option<&str>, u16)],
        ) -> &mut Self {
            let attr_name_idx = self.add_utf8("InnerClasses");

            let mut attr_data = Vec::new();
            attr_data.extend_from_slice(&(entries.len() as u16).to_be_bytes());

            for (inner, outer, inner_name, flags) in entries {
                let inner_name_idx = self.add_utf8(inner);
                let inner_class_idx = self.add_class(inner_name_idx);
                attr_data.extend_from_slice(&inner_class_idx.to_be_bytes());

                if let Some(outer_name) = outer {
                    let outer_name_idx = self.add_utf8(outer_name);
                    let outer_class_idx = self.add_class(outer_name_idx);
                    attr_data.extend_from_slice(&outer_class_idx.to_be_bytes());
                } else {
                    attr_data.extend_from_slice(&0u16.to_be_bytes());
                }

                if let Some(name) = inner_name {
                    let name_idx = self.add_utf8(name);
                    attr_data.extend_from_slice(&name_idx.to_be_bytes());
                } else {
                    attr_data.extend_from_slice(&0u16.to_be_bytes());
                }

                attr_data.extend_from_slice(&flags.to_be_bytes());
            }

            let mut attr_bytes = Vec::new();
            attr_bytes.extend_from_slice(&attr_name_idx.to_be_bytes());
            attr_bytes.extend_from_slice(&(attr_data.len() as u32).to_be_bytes());
            attr_bytes.extend_from_slice(&attr_data);

            self.attributes.push(attr_bytes);
            self
        }

        /// Add a SourceFile attribute.
        fn add_source_file_attribute(&mut self, source_file: &str) -> &mut Self {
            let attr_name_idx = self.add_utf8("SourceFile");
            let source_idx = self.add_utf8(source_file);

            let mut attr_bytes = Vec::new();
            attr_bytes.extend_from_slice(&attr_name_idx.to_be_bytes());
            attr_bytes.extend_from_slice(&2u32.to_be_bytes()); // attribute_length
            attr_bytes.extend_from_slice(&source_idx.to_be_bytes());

            self.attributes.push(attr_bytes);
            self
        }

        /// Build the class file bytes.
        fn build(&self) -> Vec<u8> {
            let mut bytes = Vec::new();

            // Magic
            bytes.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
            // Minor version
            bytes.extend_from_slice(&0u16.to_be_bytes());
            // Major version
            bytes.extend_from_slice(&self.major_version.to_be_bytes());

            // Constant pool count (entries + 1)
            let cp_count = self.cp_entries.len() as u16 + 1;
            bytes.extend_from_slice(&cp_count.to_be_bytes());
            for entry in &self.cp_entries {
                bytes.extend_from_slice(entry);
            }

            // Access flags
            bytes.extend_from_slice(&self.access_flags.to_be_bytes());
            // This class
            bytes.extend_from_slice(&self.this_class_idx.to_be_bytes());
            // Super class
            bytes.extend_from_slice(&self.super_class_idx.to_be_bytes());

            // Interfaces
            bytes.extend_from_slice(&(self.interface_indices.len() as u16).to_be_bytes());
            for idx in &self.interface_indices {
                bytes.extend_from_slice(&idx.to_be_bytes());
            }

            // Fields
            bytes.extend_from_slice(&(self.fields.len() as u16).to_be_bytes());
            for field in &self.fields {
                bytes.extend_from_slice(field);
            }

            // Methods
            bytes.extend_from_slice(&(self.methods.len() as u16).to_be_bytes());
            for method in &self.methods {
                bytes.extend_from_slice(method);
            }

            // Attributes
            bytes.extend_from_slice(&(self.attributes.len() as u16).to_be_bytes());
            for attr in &self.attributes {
                bytes.extend_from_slice(attr);
            }

            bytes
        }
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_minimal_class() {
        let bytes = ClassFileBuilder::new("com/example/Minimal").build();
        let stub = parse_class(&bytes).unwrap();

        assert_eq!(stub.fqn, "com.example.Minimal");
        assert_eq!(stub.name, "Minimal");
        assert_eq!(stub.kind, ClassKind::Class);
        assert!(stub.access.is_public());
        assert_eq!(stub.superclass.as_deref(), Some("java.lang.Object"));
        assert!(stub.interfaces.is_empty());
        assert!(stub.methods.is_empty());
        assert!(stub.fields.is_empty());
        assert!(stub.inner_classes.is_empty());
        assert!(stub.enum_constants.is_empty());
        assert!(stub.record_components.is_empty());
    }

    #[test]
    fn test_parse_class_with_methods_and_fields() {
        let mut builder = ClassFileBuilder::new("com/example/MyClass");
        builder.add_method("toString", "()Ljava/lang/String;", 0x0001); // public
        builder.add_method("hashCode", "()I", 0x0001); // public
        builder.add_field("name", "Ljava/lang/String;", 0x0002, None); // private
        builder.add_field("age", "I", 0x0001, None); // public

        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();

        assert_eq!(stub.methods.len(), 2);
        assert_eq!(stub.methods[0].name, "toString");
        assert_eq!(stub.methods[0].descriptor, "()Ljava/lang/String;");
        assert!(stub.methods[0].access.is_public());
        assert_eq!(stub.methods[1].name, "hashCode");

        assert_eq!(stub.fields.len(), 2);
        assert_eq!(stub.fields[0].name, "name");
        assert!(stub.fields[0].access.is_private());
        assert_eq!(stub.fields[1].name, "age");
        assert_eq!(stub.fields[1].descriptor, "I");
    }

    #[test]
    fn test_parse_enum_class() {
        let mut builder = ClassFileBuilder::new("com/example/Color");
        // ACC_PUBLIC | ACC_FINAL | ACC_SUPER | ACC_ENUM
        builder = builder.access_flags(0x0001 | 0x0010 | 0x0020 | 0x4000);
        // Superclass is java/lang/Enum
        builder = builder.superclass("java/lang/Enum");

        // Enum constants: static final fields of the enum type with ACC_ENUM
        // ACC_PUBLIC | ACC_STATIC | ACC_FINAL | ACC_ENUM = 0x4019
        builder.add_field("RED", "Lcom/example/Color;", 0x4019, None);
        builder.add_field("GREEN", "Lcom/example/Color;", 0x4019, None);
        builder.add_field("BLUE", "Lcom/example/Color;", 0x4019, None);

        // Non-enum field
        builder.add_field("rgb", "I", 0x0012, None); // private final

        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();

        assert_eq!(stub.kind, ClassKind::Enum);
        assert_eq!(stub.enum_constants, vec!["RED", "GREEN", "BLUE"]);
        assert_eq!(stub.superclass.as_deref(), Some("java.lang.Enum"));
    }

    #[test]
    fn test_parse_interface() {
        let builder = ClassFileBuilder::new("com/example/Readable").access_flags(0x0601); // ACC_PUBLIC | ACC_INTERFACE | ACC_ABSTRACT
        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();

        assert_eq!(stub.kind, ClassKind::Interface);
        assert!(stub.access.is_interface());
        assert!(stub.access.is_abstract());
    }

    #[test]
    fn test_parse_class_with_inner_classes() {
        let mut builder = ClassFileBuilder::new("com/example/Outer");
        builder.add_inner_classes_attribute(&[
            (
                "com/example/Outer$Inner",
                Some("com/example/Outer"),
                Some("Inner"),
                0x0001, // public
            ),
            (
                "com/example/Outer$1",
                None,   // anonymous: no outer
                None,   // anonymous: no inner name
                0x0000, // package-private
            ),
        ]);

        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();

        assert_eq!(stub.inner_classes.len(), 2);

        assert_eq!(stub.inner_classes[0].inner_fqn, "com.example.Outer$Inner");
        assert_eq!(
            stub.inner_classes[0].outer_fqn.as_deref(),
            Some("com.example.Outer")
        );
        assert_eq!(stub.inner_classes[0].inner_name.as_deref(), Some("Inner"));
        assert!(stub.inner_classes[0].access.is_public());

        assert_eq!(stub.inner_classes[1].inner_fqn, "com.example.Outer$1");
        assert!(stub.inner_classes[1].outer_fqn.is_none());
        assert!(stub.inner_classes[1].inner_name.is_none());
    }

    #[test]
    fn test_parse_class_with_constant_fields() {
        let mut builder = ClassFileBuilder::new("com/example/Constants");

        // Static final int
        let int_idx = builder.add_integer(42);
        builder.add_field("MAX_SIZE", "I", 0x0019, Some(int_idx)); // public static final

        // Static final String
        let str_utf8_idx = builder.add_utf8("hello");
        let str_idx = builder.add_string(str_utf8_idx);
        builder.add_field("DEFAULT_NAME", "Ljava/lang/String;", 0x0019, Some(str_idx));

        // Non-static-final field should not have constant value extracted
        builder.add_field("mutable", "I", 0x0001, None);

        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();

        assert_eq!(stub.fields.len(), 3);

        // MAX_SIZE = 42
        assert_eq!(stub.fields[0].name, "MAX_SIZE");
        assert_eq!(stub.fields[0].constant_value, Some(ConstantValue::Int(42)));

        // DEFAULT_NAME = "hello"
        assert_eq!(stub.fields[1].name, "DEFAULT_NAME");
        assert_eq!(
            stub.fields[1].constant_value,
            Some(ConstantValue::String("hello".to_owned()))
        );

        // mutable has no constant value
        assert!(stub.fields[2].constant_value.is_none());
    }

    #[test]
    fn test_synthetic_methods_filtered() {
        let mut builder = ClassFileBuilder::new("com/example/Filtered");
        builder.add_method("realMethod", "()V", 0x0001); // public
        builder.add_method("access$000", "()V", METHOD_ACC_SYNTHETIC); // synthetic
        builder.add_method("bridge$0", "()V", METHOD_ACC_BRIDGE); // bridge

        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();

        assert_eq!(stub.methods.len(), 1);
        assert_eq!(stub.methods[0].name, "realMethod");
    }

    #[test]
    fn test_bridge_and_synthetic_combined_filtered() {
        let mut builder = ClassFileBuilder::new("com/example/BridgeSynthetic");
        builder.add_method("realMethod", "()V", 0x0001);
        // Both bridge and synthetic set
        builder.add_method("combined", "()V", METHOD_ACC_BRIDGE | METHOD_ACC_SYNTHETIC);

        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();

        assert_eq!(stub.methods.len(), 1);
        assert_eq!(stub.methods[0].name, "realMethod");
    }

    #[test]
    fn test_malformed_bytes_returns_error() {
        // Empty bytes
        assert!(parse_class(&[]).is_err());

        // Wrong magic
        assert!(parse_class(&[0xDE, 0xAD, 0xBE, 0xEF]).is_err());

        // Truncated class file (just magic + version)
        assert!(parse_class(&[0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0x00, 0x00, 0x34]).is_err());

        // Random garbage
        assert!(parse_class(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]).is_err());
    }

    #[test]
    fn test_method_descriptor_parsing_produces_correct_types() {
        let mut builder = ClassFileBuilder::new("com/example/Types");
        // Method: void process(int, String, double[])
        builder.add_method("process", "(ILjava/lang/String;[D)V", 0x0001);

        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();

        assert_eq!(stub.methods.len(), 1);
        let method = &stub.methods[0];
        assert_eq!(method.parameter_types.len(), 3);

        assert!(matches!(
            method.parameter_types[0],
            TypeSignature::Base(BaseType::Int)
        ));
        match &method.parameter_types[1] {
            TypeSignature::Class { fqn, .. } => assert_eq!(fqn, "java.lang.String"),
            other => panic!("Expected Class, got {other:?}"),
        }
        match &method.parameter_types[2] {
            TypeSignature::Array(inner) => {
                assert!(matches!(
                    inner.as_ref(),
                    TypeSignature::Base(BaseType::Double)
                ));
            }
            other => panic!("Expected Array, got {other:?}"),
        }
        assert!(matches!(
            method.return_type,
            TypeSignature::Base(BaseType::Void)
        ));
    }

    #[test]
    fn test_access_flags_combinations() {
        // public abstract
        let builder = ClassFileBuilder::new("com/example/Abstract").access_flags(0x0421); // PUBLIC | SUPER | ABSTRACT
        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();
        assert!(stub.access.is_public());
        assert!(stub.access.is_abstract());

        // public final
        let builder = ClassFileBuilder::new("com/example/Final").access_flags(0x0031); // PUBLIC | SUPER | FINAL
        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();
        assert!(stub.access.is_public());
        assert!(stub.access.is_final());
    }

    #[test]
    fn test_class_with_interfaces() {
        let mut builder = ClassFileBuilder::new("com/example/MyList");
        builder.add_interface("java/io/Serializable");
        builder.add_interface("java/lang/Comparable");

        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();

        assert_eq!(stub.interfaces.len(), 2);
        assert_eq!(stub.interfaces[0], "java.io.Serializable");
        assert_eq!(stub.interfaces[1], "java.lang.Comparable");
    }

    #[test]
    fn test_source_file_attribute() {
        let mut builder = ClassFileBuilder::new("com/example/WithSource");
        builder.add_source_file_attribute("WithSource.java");

        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();

        assert_eq!(stub.source_file.as_deref(), Some("WithSource.java"));
    }

    #[test]
    fn test_method_with_parameter_names() {
        let mut builder = ClassFileBuilder::new("com/example/Params");
        builder.add_method_with_params(
            "greet",
            "(Ljava/lang/String;I)V",
            0x0001, // public
            &["name", "count"],
        );

        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();

        assert_eq!(stub.methods.len(), 1);
        assert_eq!(stub.methods[0].parameter_names, vec!["name", "count"]);
    }

    #[test]
    fn test_annotation_type() {
        // ACC_PUBLIC | ACC_INTERFACE | ACC_ABSTRACT | ACC_ANNOTATION
        let builder = ClassFileBuilder::new("com/example/MyAnnotation").access_flags(0x2601);
        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();
        assert_eq!(stub.kind, ClassKind::Annotation);
    }

    #[test]
    fn test_module_info_skipped() {
        let builder = ClassFileBuilder::new("module-info");
        let bytes = builder.build();
        let result = parse_class(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_package_info_skipped() {
        let builder = ClassFileBuilder::new("com/example/package-info");
        let bytes = builder.build();
        let result = parse_class(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_simple_name_extraction() {
        assert_eq!(extract_simple_name("java.util.HashMap"), "HashMap");
        assert_eq!(extract_simple_name("SimpleClass"), "SimpleClass");
        assert_eq!(extract_simple_name("java.util.Map.Entry"), "Entry");
    }

    #[test]
    fn test_no_superclass_for_object() {
        let builder = ClassFileBuilder::new("java/lang/Object").no_superclass();
        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();
        assert!(stub.superclass.is_none());
    }

    #[test]
    fn test_constructor_and_static_init() {
        let mut builder = ClassFileBuilder::new("com/example/WithInit");
        builder.add_method("<init>", "()V", 0x0001); // public constructor
        builder.add_method("<clinit>", "()V", 0x0008); // static initializer

        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();

        assert_eq!(stub.methods.len(), 2);
        assert_eq!(stub.methods[0].name, "<init>");
        assert_eq!(stub.methods[1].name, "<clinit>");
    }

    #[test]
    fn test_field_method_return_type_object() {
        let mut builder = ClassFileBuilder::new("com/example/ReturnTypes");
        builder.add_method("getList", "()Ljava/util/List;", 0x0001);

        let bytes = builder.build();
        let stub = parse_class(&bytes).unwrap();

        match &stub.methods[0].return_type {
            TypeSignature::Class { fqn, .. } => assert_eq!(fqn, "java.util.List"),
            other => panic!("Expected Class return type, got {other:?}"),
        }
    }
}
