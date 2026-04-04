//! Stub data model types for parsed JVM bytecode.
//!
//! This module defines the intermediate representation produced by bytecode
//! parsing. Each `.class` file is parsed into a [`ClassStub`] containing
//! methods, fields, annotations, generic signatures, and JVM-specific metadata
//! (modules, records, lambdas, Kotlin/Scala metadata).
//!
//! All types are pure data — no parsing logic lives here. They are designed for
//! efficient binary serialization via `postcard` and are `Send + Sync` by
//! construction (all fields are owned).

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Core stub types
// ---------------------------------------------------------------------------

/// Parsed representation of a single `.class` file.
///
/// This is the primary unit of bytecode analysis. One `ClassStub` is produced
/// per `.class` entry in a JAR (or on the filesystem classpath). Stubs are
/// cached per-JAR and merged into a [`super::ClasspathIndex`] for fast FQN
/// lookup during graph construction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassStub {
    /// Fully qualified name (e.g., `"java.util.HashMap"`).
    pub fqn: String,
    /// Simple name (e.g., `"HashMap"`).
    pub name: String,
    /// Kind of class (class, interface, enum, annotation, record, module).
    pub kind: ClassKind,
    /// Access flags (public, final, abstract, etc.).
    pub access: AccessFlags,
    /// Superclass FQN (`None` for `java.lang.Object`).
    pub superclass: Option<String>,
    /// Implemented interface FQNs.
    pub interfaces: Vec<String>,
    /// Methods declared in this class.
    pub methods: Vec<MethodStub>,
    /// Fields declared in this class.
    pub fields: Vec<FieldStub>,
    /// Class-level annotations.
    pub annotations: Vec<AnnotationStub>,
    /// Generic signature (if the class has type parameters or parameterized
    /// super types).
    pub generic_signature: Option<GenericClassSignature>,
    /// Inner class entries from the `InnerClasses` attribute.
    pub inner_classes: Vec<InnerClassEntry>,
    /// Lambda / method-reference targets extracted from `BootstrapMethods`.
    pub lambda_targets: Vec<LambdaTargetStub>,
    /// Java 9+ module info (only present for `module-info.class`).
    pub module: Option<ModuleStub>,
    /// Record components (Java 16+, only for record classes).
    pub record_components: Vec<RecordComponent>,
    /// Enum constant names (only for enum classes, in declaration order).
    pub enum_constants: Vec<String>,
    /// Source file name from the `SourceFile` attribute.
    pub source_file: Option<String>,
    /// Path of the JAR file this class was scanned from.
    ///
    /// Set by [`crate::bytecode::scan_jar`] during JAR scanning. `None` for
    /// classes parsed from loose `.class` files or deserialized from cache
    /// entries that predate this field.
    #[serde(default)]
    pub source_jar: Option<String>,
    /// Kotlin metadata (if `@kotlin.Metadata` annotation is present).
    pub kotlin_metadata: Option<KotlinMetadataStub>,
    /// Scala signature (if `@ScalaSignature` annotation is present).
    pub scala_signature: Option<ScalaSignatureStub>,
}

/// Discriminates the kind of a class-file entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClassKind {
    /// Regular class.
    Class,
    /// Interface (including functional interfaces).
    Interface,
    /// Enum class.
    Enum,
    /// Annotation type (`@interface`).
    Annotation,
    /// Record class (Java 16+).
    Record,
    /// Module descriptor (`module-info.class`).
    Module,
}

// ---------------------------------------------------------------------------
// Access flags
// ---------------------------------------------------------------------------

/// Bitflag-style JVM access modifiers (JVMS Table 4.1-A/B).
///
/// Stored as a raw `u16` to match the bytecode representation exactly. Helper
/// methods provide convenient boolean queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccessFlags {
    bits: u16,
}

impl AccessFlags {
    // -- Constants matching the JVM spec (JVMS 4.1, 4.5, 4.6) ---------------

    /// `public`
    pub const ACC_PUBLIC: u16 = 0x0001;
    /// `private`
    pub const ACC_PRIVATE: u16 = 0x0002;
    /// `protected`
    pub const ACC_PROTECTED: u16 = 0x0004;
    /// `static`
    pub const ACC_STATIC: u16 = 0x0008;
    /// `final`
    pub const ACC_FINAL: u16 = 0x0010;
    /// `synchronized` (methods) / `super` (classes — always set since Java 1.1)
    pub const ACC_SYNCHRONIZED: u16 = 0x0020;
    /// Alias for [`Self::ACC_SYNCHRONIZED`] in class context.
    pub const ACC_SUPER: u16 = 0x0020;
    /// `volatile` (fields) / bridge method (methods)
    pub const ACC_VOLATILE: u16 = 0x0040;
    /// Alias for [`Self::ACC_VOLATILE`] in method context.
    pub const ACC_BRIDGE: u16 = 0x0040;
    /// `transient` (fields) / varargs (methods)
    pub const ACC_TRANSIENT: u16 = 0x0080;
    /// Alias for [`Self::ACC_TRANSIENT`] in method context.
    pub const ACC_VARARGS: u16 = 0x0080;
    /// `native`
    pub const ACC_NATIVE: u16 = 0x0100;
    /// `interface`
    pub const ACC_INTERFACE: u16 = 0x0200;
    /// `abstract`
    pub const ACC_ABSTRACT: u16 = 0x0400;
    /// `strictfp`
    pub const ACC_STRICT: u16 = 0x0800;
    /// Compiler-generated (not present in source)
    pub const ACC_SYNTHETIC: u16 = 0x1000;
    /// Annotation type
    pub const ACC_ANNOTATION: u16 = 0x2000;
    /// Enum class or enum constant field
    pub const ACC_ENUM: u16 = 0x4000;
    /// Module (Java 9+)
    pub const ACC_MODULE: u16 = 0x8000;

    // -- Construction --------------------------------------------------------

    /// Create from raw bits.
    #[must_use]
    pub const fn new(bits: u16) -> Self {
        Self { bits }
    }

    /// Empty flags (package-private, no modifiers).
    #[must_use]
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    // -- Query methods -------------------------------------------------------

    #[must_use]
    pub const fn is_public(&self) -> bool {
        self.bits & Self::ACC_PUBLIC != 0
    }

    #[must_use]
    pub const fn is_private(&self) -> bool {
        self.bits & Self::ACC_PRIVATE != 0
    }

    #[must_use]
    pub const fn is_protected(&self) -> bool {
        self.bits & Self::ACC_PROTECTED != 0
    }

    #[must_use]
    pub const fn is_static(&self) -> bool {
        self.bits & Self::ACC_STATIC != 0
    }

    #[must_use]
    pub const fn is_final(&self) -> bool {
        self.bits & Self::ACC_FINAL != 0
    }

    #[must_use]
    pub const fn is_synchronized(&self) -> bool {
        self.bits & Self::ACC_SYNCHRONIZED != 0
    }

    #[must_use]
    pub const fn is_volatile(&self) -> bool {
        self.bits & Self::ACC_VOLATILE != 0
    }

    #[must_use]
    pub const fn is_bridge(&self) -> bool {
        self.bits & Self::ACC_BRIDGE != 0
    }

    #[must_use]
    pub const fn is_transient(&self) -> bool {
        self.bits & Self::ACC_TRANSIENT != 0
    }

    #[must_use]
    pub const fn is_varargs(&self) -> bool {
        self.bits & Self::ACC_VARARGS != 0
    }

    #[must_use]
    pub const fn is_native(&self) -> bool {
        self.bits & Self::ACC_NATIVE != 0
    }

    #[must_use]
    pub const fn is_interface(&self) -> bool {
        self.bits & Self::ACC_INTERFACE != 0
    }

    #[must_use]
    pub const fn is_abstract(&self) -> bool {
        self.bits & Self::ACC_ABSTRACT != 0
    }

    #[must_use]
    pub const fn is_strict(&self) -> bool {
        self.bits & Self::ACC_STRICT != 0
    }

    #[must_use]
    pub const fn is_synthetic(&self) -> bool {
        self.bits & Self::ACC_SYNTHETIC != 0
    }

    #[must_use]
    pub const fn is_annotation(&self) -> bool {
        self.bits & Self::ACC_ANNOTATION != 0
    }

    #[must_use]
    pub const fn is_enum(&self) -> bool {
        self.bits & Self::ACC_ENUM != 0
    }

    #[must_use]
    pub const fn is_module(&self) -> bool {
        self.bits & Self::ACC_MODULE != 0
    }

    /// Raw bit representation.
    #[must_use]
    pub const fn bits(&self) -> u16 {
        self.bits
    }

    /// Returns `true` if the given flag bit(s) are set.
    #[must_use]
    pub const fn contains(&self, flag: u16) -> bool {
        self.bits & flag == flag
    }

    /// Combine two flag sets (bitwise OR).
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }
}

impl std::fmt::Display for AccessFlags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if self.is_public() {
            parts.push("public");
        }
        if self.is_private() {
            parts.push("private");
        }
        if self.is_protected() {
            parts.push("protected");
        }
        if self.is_static() {
            parts.push("static");
        }
        if self.is_final() {
            parts.push("final");
        }
        if self.is_abstract() {
            parts.push("abstract");
        }
        if self.is_native() {
            parts.push("native");
        }
        if self.is_synchronized() {
            parts.push("synchronized");
        }
        if self.is_synthetic() {
            parts.push("synthetic");
        }
        write!(f, "{}", parts.join(" "))
    }
}

// ---------------------------------------------------------------------------
// Method and field stubs
// ---------------------------------------------------------------------------

/// Parsed method declaration from a `.class` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodStub {
    /// Method name (e.g., `"hashCode"`, `"<init>"`, `"<clinit>"`).
    pub name: String,
    /// Access flags.
    pub access: AccessFlags,
    /// JVM method descriptor (e.g., `"(Ljava/lang/String;)V"`).
    pub descriptor: String,
    /// Generic method signature (if the method has type parameters or
    /// parameterized types not expressible in the descriptor).
    pub generic_signature: Option<GenericMethodSignature>,
    /// Method-level annotations.
    pub annotations: Vec<AnnotationStub>,
    /// Parameter annotations. Outer vec is indexed by parameter position;
    /// inner vec contains the annotations for that parameter.
    pub parameter_annotations: Vec<Vec<AnnotationStub>>,
    /// Parameter names from the `MethodParameters` attribute (may be empty
    /// if compiled without `-parameters`).
    pub parameter_names: Vec<String>,
    /// Return type parsed from the descriptor.
    pub return_type: TypeSignature,
    /// Parameter types parsed from the descriptor.
    pub parameter_types: Vec<TypeSignature>,
}

/// Parsed field declaration from a `.class` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldStub {
    /// Field name.
    pub name: String,
    /// Access flags.
    pub access: AccessFlags,
    /// JVM field descriptor (e.g., `"Ljava/lang/String;"`).
    pub descriptor: String,
    /// Generic field type signature (if the field uses a parameterized type
    /// not expressible in the descriptor).
    pub generic_signature: Option<TypeSignature>,
    /// Field-level annotations.
    pub annotations: Vec<AnnotationStub>,
    /// Constant value for `static final` fields of primitive or `String` type.
    pub constant_value: Option<ConstantValue>,
}

// ---------------------------------------------------------------------------
// Annotation types
// ---------------------------------------------------------------------------

/// A single annotation instance (runtime-visible or runtime-invisible).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationStub {
    /// Annotation type FQN (e.g.,
    /// `"org.springframework.web.bind.annotation.RequestMapping"`).
    pub type_fqn: String,
    /// Annotation element-value pairs.
    pub elements: Vec<AnnotationElement>,
    /// Whether this annotation is retained at runtime
    /// (`RuntimeVisibleAnnotations` vs `RuntimeInvisibleAnnotations`).
    pub is_runtime_visible: bool,
}

/// A single element (key-value pair) within an annotation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationElement {
    /// Element name (e.g., `"value"`, `"method"`).
    pub name: String,
    /// Element value.
    pub value: AnnotationElementValue,
}

/// The value of an annotation element (JVMS 4.7.16.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AnnotationElementValue {
    /// Primitive or `String` constant.
    Const(ConstantValue),
    /// Enum constant reference.
    EnumConst {
        /// Fully qualified name of the enum type.
        type_fqn: String,
        /// Name of the enum constant.
        const_name: String,
    },
    /// Class literal (e.g., `String.class`), stored as a type descriptor.
    ClassInfo(String),
    /// Nested annotation value.
    Annotation(Box<AnnotationStub>),
    /// Array of element values.
    Array(Vec<AnnotationElementValue>),
}

// ---------------------------------------------------------------------------
// Generic signature types (JVMS 4.7.9)
// ---------------------------------------------------------------------------

/// Class-level generic signature.
///
/// Encodes type parameters and parameterized super-types that are erased in
/// the regular class descriptor. Parsed from the `Signature` attribute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenericClassSignature {
    /// Type parameters (e.g., `[K, V]` in `HashMap<K, V>`).
    pub type_parameters: Vec<TypeParameterStub>,
    /// Superclass type (with type arguments applied).
    pub superclass: TypeSignature,
    /// Implemented interfaces (with type arguments applied).
    pub interfaces: Vec<TypeSignature>,
}

/// Method-level generic signature.
///
/// Encodes type parameters, parameterized parameter/return types, and
/// declared exception types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenericMethodSignature {
    /// Type parameters declared on the method.
    pub type_parameters: Vec<TypeParameterStub>,
    /// Parameter types (with generics).
    pub parameter_types: Vec<TypeSignature>,
    /// Return type (with generics).
    pub return_type: TypeSignature,
    /// Declared exception types (from `throws` clause).
    pub exception_types: Vec<TypeSignature>,
}

/// A formal type parameter declaration (e.g., `T extends Comparable<T>`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeParameterStub {
    /// Parameter name (e.g., `"T"`, `"K"`, `"V"`).
    pub name: String,
    /// Class bound (the first bound, which may be a class type). `None` if
    /// the bound is implicitly `Object`.
    pub class_bound: Option<TypeSignature>,
    /// Additional interface bounds (e.g., `T extends Serializable & Comparable<T>`
    /// — `Comparable<T>` would be an interface bound).
    pub interface_bounds: Vec<TypeSignature>,
}

/// JVM type signature with full generic information.
///
/// This is the central type representation used throughout the stub model.
/// It captures the full parameterized type structure that is erased in
/// standard JVM descriptors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypeSignature {
    /// Primitive type (byte, char, double, float, int, long, short, boolean)
    /// or `void`.
    Base(BaseType),
    /// Class or interface type with optional type arguments.
    Class {
        /// Fully qualified name (e.g., `"java.util.Map"`).
        fqn: String,
        /// Type arguments (e.g., `[String, List<Integer>]`). Empty if the
        /// type is used in its raw form.
        type_arguments: Vec<TypeArgument>,
    },
    /// Type variable reference (e.g., `T` in a generic method body).
    TypeVariable(String),
    /// Array type (e.g., `String[]` → `Array(Class { fqn: "java.lang.String" })`).
    Array(Box<TypeSignature>),
    /// Wildcard type (used only inside type argument positions).
    Wildcard {
        /// Optional bound. `None` represents `?` (unbounded).
        bound: Option<WildcardBound>,
    },
}

/// JVM primitive types and `void`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BaseType {
    /// `byte` (`B`)
    Byte,
    /// `char` (`C`)
    Char,
    /// `double` (`D`)
    Double,
    /// `float` (`F`)
    Float,
    /// `int` (`I`)
    Int,
    /// `long` (`J`)
    Long,
    /// `short` (`S`)
    Short,
    /// `boolean` (`Z`)
    Boolean,
    /// `void` (`V`)
    Void,
}

/// A type argument in a parameterized type (JVMS 4.7.9.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypeArgument {
    /// Concrete type (e.g., `String` in `List<String>`).
    Type(TypeSignature),
    /// Upper-bounded wildcard (e.g., `? extends Number`).
    Extends(TypeSignature),
    /// Lower-bounded wildcard (e.g., `? super Integer`).
    Super(TypeSignature),
    /// Unbounded wildcard (`?`).
    Unbounded,
}

/// Wildcard bound direction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WildcardBound {
    /// Upper bound (`? extends T`).
    Extends(Box<TypeSignature>),
    /// Lower bound (`? super T`).
    Super(Box<TypeSignature>),
}

// ---------------------------------------------------------------------------
// Lambda / method reference targets
// ---------------------------------------------------------------------------

/// A lambda or method-reference target extracted from the `BootstrapMethods`
/// attribute (specifically `LambdaMetafactory` entries).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LambdaTargetStub {
    /// Target method owner class FQN.
    pub owner_fqn: String,
    /// Target method name.
    pub method_name: String,
    /// Target method descriptor.
    pub method_descriptor: String,
    /// Bootstrap method handle kind (how the target is invoked).
    pub reference_kind: ReferenceKind,
}

/// Method handle reference kinds (JVMS 5.4.3.5, Table 5.4.3.5-A).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReferenceKind {
    /// `REF_getField` (1)
    GetField,
    /// `REF_getStatic` (2)
    GetStatic,
    /// `REF_putField` (3)
    PutField,
    /// `REF_putStatic` (4)
    PutStatic,
    /// `REF_invokeVirtual` (5)
    InvokeVirtual,
    /// `REF_invokeStatic` (6)
    InvokeStatic,
    /// `REF_invokeSpecial` (7)
    InvokeSpecial,
    /// `REF_newInvokeSpecial` (8)
    NewInvokeSpecial,
    /// `REF_invokeInterface` (9)
    InvokeInterface,
}

// ---------------------------------------------------------------------------
// Module info (Java 9+)
// ---------------------------------------------------------------------------

/// Module descriptor parsed from `module-info.class` (JVMS 4.7.25).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleStub {
    /// Module name (e.g., `"java.base"`).
    pub name: String,
    /// Module access flags.
    pub access: AccessFlags,
    /// Module version string (from `ModuleMainClass` or `ModulePackages`).
    pub version: Option<String>,
    /// `requires` directives.
    pub requires: Vec<ModuleRequires>,
    /// `exports` directives.
    pub exports: Vec<ModuleExports>,
    /// `opens` directives.
    pub opens: Vec<ModuleOpens>,
    /// `provides` directives.
    pub provides: Vec<ModuleProvides>,
    /// `uses` directives (service interfaces consumed).
    pub uses: Vec<String>,
}

/// A `requires` directive in a module descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleRequires {
    /// Required module name.
    pub module_name: String,
    /// Flags (e.g., `ACC_TRANSITIVE`, `ACC_STATIC_PHASE`).
    pub access: AccessFlags,
    /// Required module version.
    pub version: Option<String>,
}

/// An `exports` directive in a module descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleExports {
    /// Exported package name.
    pub package: String,
    /// Export flags.
    pub access: AccessFlags,
    /// Qualified exports target modules (empty = unqualified export).
    pub to_modules: Vec<String>,
}

/// An `opens` directive in a module descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleOpens {
    /// Opened package name.
    pub package: String,
    /// Opens flags.
    pub access: AccessFlags,
    /// Qualified opens target modules (empty = unqualified).
    pub to_modules: Vec<String>,
}

/// A `provides` directive in a module descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleProvides {
    /// Service interface FQN.
    pub service: String,
    /// Implementation class FQNs.
    pub implementations: Vec<String>,
}

// ---------------------------------------------------------------------------
// Record components (Java 16+)
// ---------------------------------------------------------------------------

/// A record component (parameter of a `record` class).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordComponent {
    /// Component name (also the accessor method name).
    pub name: String,
    /// JVM descriptor of the component type.
    pub descriptor: String,
    /// Generic signature (if the component uses a parameterized type).
    pub generic_signature: Option<TypeSignature>,
    /// Component-level annotations.
    pub annotations: Vec<AnnotationStub>,
}

// ---------------------------------------------------------------------------
// Inner classes
// ---------------------------------------------------------------------------

/// An entry from the `InnerClasses` attribute (JVMS 4.7.6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InnerClassEntry {
    /// Inner class FQN.
    pub inner_fqn: String,
    /// Outer class FQN (`None` for anonymous or local classes).
    pub outer_fqn: Option<String>,
    /// Simple name of the inner class (`None` for anonymous classes).
    pub inner_name: Option<String>,
    /// Inner class access flags.
    pub access: AccessFlags,
}

// ---------------------------------------------------------------------------
// Constant values
// ---------------------------------------------------------------------------

/// Constant value from the `ConstantValue` attribute (JVMS 4.7.2) or
/// annotation element values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ConstantValue {
    /// `int` (or smaller integral types: `byte`, `char`, `short`, `boolean`).
    Int(i32),
    /// `long`.
    Long(i64),
    /// `float`.
    Float(OrderedFloat<f32>),
    /// `double`.
    Double(OrderedFloat<f64>),
    /// `String` (`CONSTANT_String`).
    String(String),
}

/// Wrapper around `f32` that provides `PartialEq` with total ordering
/// (treating NaN as equal to NaN) for use in [`ConstantValue`].
///
/// This is needed because `f32`/`f64` do not implement `Eq`, but we need
/// `PartialEq` for testing round-trip serialization.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct OrderedFloat<T>(pub T);

impl PartialEq for OrderedFloat<f32> {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for OrderedFloat<f32> {}

impl PartialEq for OrderedFloat<f64> {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for OrderedFloat<f64> {}

impl std::hash::Hash for OrderedFloat<f32> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

impl std::hash::Hash for OrderedFloat<f64> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

// ---------------------------------------------------------------------------
// Kotlin metadata
// ---------------------------------------------------------------------------

/// Raw Kotlin metadata extracted from the `@kotlin.Metadata` annotation.
///
/// This is a placeholder populated during bytecode parsing (U03) and
/// interpreted by the Kotlin metadata decoder (U18).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KotlinMetadataStub {
    /// Metadata kind:
    /// - 1 = Class
    /// - 2 = File facade
    /// - 3 = Synthetic class
    /// - 4 = Multi-file class facade
    /// - 5 = Multi-file class part
    pub kind: u32,
    /// Metadata version (e.g., `[1, 9, 0]`).
    pub metadata_version: Vec<u32>,
    /// Raw protobuf data (`d1` array from the annotation).
    pub data1: Vec<String>,
    /// Raw string table (`d2` array from the annotation).
    pub data2: Vec<String>,
    /// Extra string from the annotation (`xs`).
    pub extra_string: Option<String>,
    /// Package name from the annotation (`pn`).
    pub package_name: Option<String>,
    /// Extra int from the annotation (`xi`).
    pub extra_int: Option<i32>,
}

// ---------------------------------------------------------------------------
// Scala signature
// ---------------------------------------------------------------------------

/// Raw Scala signature extracted from the `@ScalaSignature` annotation.
///
/// This is a placeholder populated during bytecode parsing (U03) and
/// interpreted by the Scala signature decoder (U19).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalaSignatureStub {
    /// Raw signature bytes (decoded from base64 `bytes` element).
    pub bytes: Vec<u8>,
    /// Major version of the signature format.
    pub major_version: u32,
    /// Minor version of the signature format.
    pub minor_version: u32,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Helper builders for test data --------------------------------------

    fn sample_access_flags() -> AccessFlags {
        AccessFlags::new(AccessFlags::ACC_PUBLIC | AccessFlags::ACC_FINAL | AccessFlags::ACC_STATIC)
    }

    fn sample_type_signature_class() -> TypeSignature {
        TypeSignature::Class {
            fqn: "java.lang.String".to_owned(),
            type_arguments: vec![],
        }
    }

    fn sample_annotation() -> AnnotationStub {
        AnnotationStub {
            type_fqn: "java.lang.Override".to_owned(),
            elements: vec![],
            is_runtime_visible: true,
        }
    }

    fn sample_method_stub() -> MethodStub {
        MethodStub {
            name: "toString".to_owned(),
            access: AccessFlags::new(AccessFlags::ACC_PUBLIC),
            descriptor: "()Ljava/lang/String;".to_owned(),
            generic_signature: None,
            annotations: vec![sample_annotation()],
            parameter_annotations: vec![],
            parameter_names: vec![],
            return_type: sample_type_signature_class(),
            parameter_types: vec![],
        }
    }

    fn sample_field_stub() -> FieldStub {
        FieldStub {
            name: "MAX_SIZE".to_owned(),
            access: AccessFlags::new(
                AccessFlags::ACC_PUBLIC | AccessFlags::ACC_STATIC | AccessFlags::ACC_FINAL,
            ),
            descriptor: "I".to_owned(),
            generic_signature: None,
            annotations: vec![],
            constant_value: Some(ConstantValue::Int(1024)),
        }
    }

    fn sample_class_stub() -> ClassStub {
        ClassStub {
            fqn: "com.example.MyClass".to_owned(),
            name: "MyClass".to_owned(),
            kind: ClassKind::Class,
            access: AccessFlags::new(AccessFlags::ACC_PUBLIC),
            superclass: Some("java.lang.Object".to_owned()),
            interfaces: vec!["java.io.Serializable".to_owned()],
            methods: vec![sample_method_stub()],
            fields: vec![sample_field_stub()],
            annotations: vec![],
            generic_signature: None,
            inner_classes: vec![],
            lambda_targets: vec![],
            module: None,
            record_components: vec![],
            enum_constants: vec![],
            source_file: Some("MyClass.java".to_owned()),
            source_jar: None,
            kotlin_metadata: None,
            scala_signature: None,
        }
    }

    // -- postcard round-trip helpers ----------------------------------------

    fn round_trip<T: Serialize + for<'de> Deserialize<'de> + std::fmt::Debug>(value: &T) -> T {
        let bytes = postcard::to_allocvec(value).expect("postcard serialization should succeed");
        postcard::from_bytes(&bytes).expect("postcard deserialization should succeed")
    }

    // -- AccessFlags tests --------------------------------------------------

    #[test]
    fn access_flags_individual_bits() {
        let flags = AccessFlags::new(AccessFlags::ACC_PUBLIC);
        assert!(flags.is_public());
        assert!(!flags.is_private());
        assert!(!flags.is_protected());
        assert!(!flags.is_static());
        assert!(!flags.is_final());
        assert!(!flags.is_abstract());
        assert!(!flags.is_synthetic());
    }

    #[test]
    fn access_flags_combined_bits() {
        let flags = sample_access_flags();
        assert!(flags.is_public());
        assert!(flags.is_final());
        assert!(flags.is_static());
        assert!(!flags.is_private());
        assert!(!flags.is_abstract());
    }

    #[test]
    fn access_flags_empty() {
        let flags = AccessFlags::empty();
        assert_eq!(flags.bits(), 0);
        assert!(!flags.is_public());
        assert!(!flags.is_private());
    }

    #[test]
    fn access_flags_all_class_flags() {
        let flags = AccessFlags::new(
            AccessFlags::ACC_PUBLIC
                | AccessFlags::ACC_INTERFACE
                | AccessFlags::ACC_ABSTRACT
                | AccessFlags::ACC_ANNOTATION,
        );
        assert!(flags.is_public());
        assert!(flags.is_interface());
        assert!(flags.is_abstract());
        assert!(flags.is_annotation());
        assert!(!flags.is_enum());
        assert!(!flags.is_module());
    }

    #[test]
    fn access_flags_contains() {
        let flags = AccessFlags::new(AccessFlags::ACC_PUBLIC | AccessFlags::ACC_STATIC);
        assert!(flags.contains(AccessFlags::ACC_PUBLIC));
        assert!(flags.contains(AccessFlags::ACC_STATIC));
        assert!(flags.contains(AccessFlags::ACC_PUBLIC | AccessFlags::ACC_STATIC));
        assert!(!flags.contains(AccessFlags::ACC_FINAL));
    }

    #[test]
    fn access_flags_union() {
        let a = AccessFlags::new(AccessFlags::ACC_PUBLIC);
        let b = AccessFlags::new(AccessFlags::ACC_STATIC);
        let combined = a.union(b);
        assert!(combined.is_public());
        assert!(combined.is_static());
        assert_eq!(
            combined.bits(),
            AccessFlags::ACC_PUBLIC | AccessFlags::ACC_STATIC
        );
    }

    #[test]
    fn access_flags_display() {
        let flags = AccessFlags::new(AccessFlags::ACC_PUBLIC | AccessFlags::ACC_STATIC);
        let display = format!("{flags}");
        assert!(display.contains("public"));
        assert!(display.contains("static"));
    }

    #[test]
    fn access_flags_method_specific() {
        let flags = AccessFlags::new(
            AccessFlags::ACC_PUBLIC
                | AccessFlags::ACC_SYNCHRONIZED
                | AccessFlags::ACC_BRIDGE
                | AccessFlags::ACC_VARARGS
                | AccessFlags::ACC_NATIVE,
        );
        assert!(flags.is_public());
        assert!(flags.is_synchronized());
        assert!(flags.is_bridge());
        assert!(flags.is_varargs());
        assert!(flags.is_native());
    }

    #[test]
    fn access_flags_field_specific() {
        let flags = AccessFlags::new(
            AccessFlags::ACC_PRIVATE | AccessFlags::ACC_VOLATILE | AccessFlags::ACC_TRANSIENT,
        );
        assert!(flags.is_private());
        assert!(flags.is_volatile());
        assert!(flags.is_transient());
    }

    #[test]
    fn access_flags_round_trip() {
        let flags = sample_access_flags();
        let rt = round_trip(&flags);
        assert_eq!(flags.bits(), rt.bits());
    }

    // -- ClassKind tests ----------------------------------------------------

    #[test]
    fn class_kind_round_trip() {
        for kind in [
            ClassKind::Class,
            ClassKind::Interface,
            ClassKind::Enum,
            ClassKind::Annotation,
            ClassKind::Record,
            ClassKind::Module,
        ] {
            let rt = round_trip(&kind);
            assert_eq!(kind, rt);
        }
    }

    // -- ClassStub round-trip -----------------------------------------------

    #[test]
    fn class_stub_round_trip() {
        let stub = sample_class_stub();
        let rt = round_trip(&stub);
        assert_eq!(stub.fqn, rt.fqn);
        assert_eq!(stub.name, rt.name);
        assert_eq!(stub.kind, rt.kind);
        assert_eq!(stub.access.bits(), rt.access.bits());
        assert_eq!(stub.superclass, rt.superclass);
        assert_eq!(stub.interfaces, rt.interfaces);
        assert_eq!(stub.methods.len(), rt.methods.len());
        assert_eq!(stub.fields.len(), rt.fields.len());
        assert_eq!(stub.source_file, rt.source_file);
    }

    // -- MethodStub round-trip ----------------------------------------------

    #[test]
    fn method_stub_round_trip() {
        let method = sample_method_stub();
        let rt = round_trip(&method);
        assert_eq!(method.name, rt.name);
        assert_eq!(method.descriptor, rt.descriptor);
        assert_eq!(method.access.bits(), rt.access.bits());
        assert_eq!(method.annotations.len(), rt.annotations.len());
    }

    // -- FieldStub round-trip -----------------------------------------------

    #[test]
    fn field_stub_round_trip() {
        let field = sample_field_stub();
        let rt = round_trip(&field);
        assert_eq!(field.name, rt.name);
        assert_eq!(field.descriptor, rt.descriptor);
        assert_eq!(field.access.bits(), rt.access.bits());
        assert_eq!(field.constant_value, rt.constant_value);
    }

    // -- ConstantValue round-trip -------------------------------------------

    #[test]
    fn constant_value_int_round_trip() {
        let cv = ConstantValue::Int(42);
        assert_eq!(cv, round_trip(&cv));
    }

    #[test]
    fn constant_value_long_round_trip() {
        let cv = ConstantValue::Long(i64::MAX);
        assert_eq!(cv, round_trip(&cv));
    }

    #[test]
    fn constant_value_float_round_trip() {
        let cv = ConstantValue::Float(OrderedFloat(std::f32::consts::PI));
        assert_eq!(cv, round_trip(&cv));
    }

    #[test]
    fn constant_value_double_round_trip() {
        let cv = ConstantValue::Double(OrderedFloat(std::f64::consts::E));
        assert_eq!(cv, round_trip(&cv));
    }

    #[test]
    fn constant_value_string_round_trip() {
        let cv = ConstantValue::String("hello world".to_owned());
        assert_eq!(cv, round_trip(&cv));
    }

    // -- Annotation round-trip (simple) -------------------------------------

    #[test]
    fn annotation_simple_round_trip() {
        let ann = sample_annotation();
        let rt = round_trip(&ann);
        assert_eq!(ann.type_fqn, rt.type_fqn);
        assert_eq!(ann.is_runtime_visible, rt.is_runtime_visible);
    }

    // -- Annotation with nested annotation ----------------------------------

    #[test]
    fn annotation_nested_round_trip() {
        let inner = AnnotationStub {
            type_fqn: "javax.validation.constraints.Size".to_owned(),
            elements: vec![
                AnnotationElement {
                    name: "min".to_owned(),
                    value: AnnotationElementValue::Const(ConstantValue::Int(1)),
                },
                AnnotationElement {
                    name: "max".to_owned(),
                    value: AnnotationElementValue::Const(ConstantValue::Int(100)),
                },
            ],
            is_runtime_visible: true,
        };

        let outer = AnnotationStub {
            type_fqn: "javax.validation.constraints.NotNull".to_owned(),
            elements: vec![AnnotationElement {
                name: "payload".to_owned(),
                value: AnnotationElementValue::Annotation(Box::new(inner)),
            }],
            is_runtime_visible: true,
        };

        let rt = round_trip(&outer);
        assert_eq!(outer.type_fqn, rt.type_fqn);
        assert_eq!(outer.elements.len(), rt.elements.len());
        match &rt.elements[0].value {
            AnnotationElementValue::Annotation(inner_rt) => {
                assert_eq!(inner_rt.type_fqn, "javax.validation.constraints.Size");
                assert_eq!(inner_rt.elements.len(), 2);
            }
            other => panic!("expected nested Annotation, got {other:?}"),
        }
    }

    // -- Annotation with array and enum values ------------------------------

    #[test]
    fn annotation_complex_elements_round_trip() {
        let ann = AnnotationStub {
            type_fqn: "org.springframework.web.bind.annotation.RequestMapping".to_owned(),
            elements: vec![
                AnnotationElement {
                    name: "value".to_owned(),
                    value: AnnotationElementValue::Array(vec![AnnotationElementValue::Const(
                        ConstantValue::String("/api/users".to_owned()),
                    )]),
                },
                AnnotationElement {
                    name: "method".to_owned(),
                    value: AnnotationElementValue::Array(vec![
                        AnnotationElementValue::EnumConst {
                            type_fqn: "org.springframework.web.bind.annotation.RequestMethod"
                                .to_owned(),
                            const_name: "GET".to_owned(),
                        },
                        AnnotationElementValue::EnumConst {
                            type_fqn: "org.springframework.web.bind.annotation.RequestMethod"
                                .to_owned(),
                            const_name: "POST".to_owned(),
                        },
                    ]),
                },
                AnnotationElement {
                    name: "produces".to_owned(),
                    value: AnnotationElementValue::ClassInfo("Ljava/lang/String;".to_owned()),
                },
            ],
            is_runtime_visible: true,
        };

        let rt = round_trip(&ann);
        assert_eq!(ann.elements.len(), rt.elements.len());
        assert_eq!(ann.type_fqn, rt.type_fqn);
    }

    // -- Generic signature round-trip (complex nested type args) ------------

    #[test]
    fn generic_class_signature_round_trip() {
        // Represents: class MyMap<K extends Comparable<K>, V> extends HashMap<K, List<V>>
        let sig = GenericClassSignature {
            type_parameters: vec![
                TypeParameterStub {
                    name: "K".to_owned(),
                    class_bound: Some(TypeSignature::Class {
                        fqn: "java.lang.Comparable".to_owned(),
                        type_arguments: vec![TypeArgument::Type(TypeSignature::TypeVariable(
                            "K".to_owned(),
                        ))],
                    }),
                    interface_bounds: vec![],
                },
                TypeParameterStub {
                    name: "V".to_owned(),
                    class_bound: None,
                    interface_bounds: vec![],
                },
            ],
            superclass: TypeSignature::Class {
                fqn: "java.util.HashMap".to_owned(),
                type_arguments: vec![
                    TypeArgument::Type(TypeSignature::TypeVariable("K".to_owned())),
                    TypeArgument::Type(TypeSignature::Class {
                        fqn: "java.util.List".to_owned(),
                        type_arguments: vec![TypeArgument::Type(TypeSignature::TypeVariable(
                            "V".to_owned(),
                        ))],
                    }),
                ],
            },
            interfaces: vec![TypeSignature::Class {
                fqn: "java.io.Serializable".to_owned(),
                type_arguments: vec![],
            }],
        };

        let rt = round_trip(&sig);
        assert_eq!(sig.type_parameters.len(), rt.type_parameters.len());
        assert_eq!(sig.type_parameters[0].name, rt.type_parameters[0].name);
        assert_eq!(sig.interfaces.len(), rt.interfaces.len());
    }

    #[test]
    fn generic_method_signature_round_trip() {
        // Represents: <T extends Number> List<T> convert(Collection<? extends T> input)
        let sig = GenericMethodSignature {
            type_parameters: vec![TypeParameterStub {
                name: "T".to_owned(),
                class_bound: Some(TypeSignature::Class {
                    fqn: "java.lang.Number".to_owned(),
                    type_arguments: vec![],
                }),
                interface_bounds: vec![],
            }],
            parameter_types: vec![TypeSignature::Class {
                fqn: "java.util.Collection".to_owned(),
                type_arguments: vec![TypeArgument::Extends(TypeSignature::TypeVariable(
                    "T".to_owned(),
                ))],
            }],
            return_type: TypeSignature::Class {
                fqn: "java.util.List".to_owned(),
                type_arguments: vec![TypeArgument::Type(TypeSignature::TypeVariable(
                    "T".to_owned(),
                ))],
            },
            exception_types: vec![TypeSignature::Class {
                fqn: "java.lang.Exception".to_owned(),
                type_arguments: vec![],
            }],
        };

        let rt = round_trip(&sig);
        assert_eq!(sig.type_parameters.len(), rt.type_parameters.len());
        assert_eq!(sig.parameter_types.len(), rt.parameter_types.len());
        assert_eq!(sig.exception_types.len(), rt.exception_types.len());
    }

    // -- TypeSignature variants round-trip ----------------------------------

    #[test]
    fn type_signature_base_round_trip() {
        for base in [
            BaseType::Byte,
            BaseType::Char,
            BaseType::Double,
            BaseType::Float,
            BaseType::Int,
            BaseType::Long,
            BaseType::Short,
            BaseType::Boolean,
            BaseType::Void,
        ] {
            let sig = TypeSignature::Base(base);
            let bytes = postcard::to_allocvec(&sig).expect("serialization should succeed");
            let rt: TypeSignature =
                postcard::from_bytes(&bytes).expect("deserialization should succeed");
            match (&sig, &rt) {
                (TypeSignature::Base(a), TypeSignature::Base(b)) => assert_eq!(a, b),
                _ => panic!("expected Base variant"),
            }
        }
    }

    #[test]
    fn type_signature_array_round_trip() {
        // String[][]
        let sig = TypeSignature::Array(Box::new(TypeSignature::Array(Box::new(
            TypeSignature::Class {
                fqn: "java.lang.String".to_owned(),
                type_arguments: vec![],
            },
        ))));
        let rt = round_trip(&sig);
        match rt {
            TypeSignature::Array(inner) => match *inner {
                TypeSignature::Array(inner2) => match *inner2 {
                    TypeSignature::Class { ref fqn, .. } => {
                        assert_eq!(fqn, "java.lang.String");
                    }
                    _ => panic!("expected Class"),
                },
                _ => panic!("expected nested Array"),
            },
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn type_signature_wildcard_round_trip() {
        let sig = TypeSignature::Wildcard {
            bound: Some(WildcardBound::Extends(Box::new(TypeSignature::Class {
                fqn: "java.lang.Number".to_owned(),
                type_arguments: vec![],
            }))),
        };
        let rt = round_trip(&sig);
        match rt {
            TypeSignature::Wildcard {
                bound: Some(WildcardBound::Extends(inner)),
            } => match *inner {
                TypeSignature::Class { ref fqn, .. } => {
                    assert_eq!(fqn, "java.lang.Number");
                }
                _ => panic!("expected Class inside wildcard bound"),
            },
            _ => panic!("expected Wildcard with Extends bound"),
        }
    }

    // -- TypeArgument variants round-trip -----------------------------------

    #[test]
    fn type_argument_variants_round_trip() {
        let args = vec![
            TypeArgument::Type(TypeSignature::Base(BaseType::Int)),
            TypeArgument::Extends(TypeSignature::Class {
                fqn: "java.lang.Number".to_owned(),
                type_arguments: vec![],
            }),
            TypeArgument::Super(TypeSignature::Class {
                fqn: "java.lang.Integer".to_owned(),
                type_arguments: vec![],
            }),
            TypeArgument::Unbounded,
        ];
        let rt = round_trip(&args);
        assert_eq!(args.len(), rt.len());
    }

    // -- ReferenceKind round-trip -------------------------------------------

    #[test]
    fn reference_kind_round_trip() {
        for kind in [
            ReferenceKind::GetField,
            ReferenceKind::GetStatic,
            ReferenceKind::PutField,
            ReferenceKind::PutStatic,
            ReferenceKind::InvokeVirtual,
            ReferenceKind::InvokeStatic,
            ReferenceKind::InvokeSpecial,
            ReferenceKind::NewInvokeSpecial,
            ReferenceKind::InvokeInterface,
        ] {
            let rt = round_trip(&kind);
            assert_eq!(kind, rt);
        }
    }

    // -- LambdaTargetStub round-trip ----------------------------------------

    #[test]
    fn lambda_target_round_trip() {
        let target = LambdaTargetStub {
            owner_fqn: "java.util.stream.Stream".to_owned(),
            method_name: "map".to_owned(),
            method_descriptor: "(Ljava/util/function/Function;)Ljava/util/stream/Stream;"
                .to_owned(),
            reference_kind: ReferenceKind::InvokeInterface,
        };
        let rt = round_trip(&target);
        assert_eq!(target.owner_fqn, rt.owner_fqn);
        assert_eq!(target.method_name, rt.method_name);
        assert_eq!(target.method_descriptor, rt.method_descriptor);
        assert_eq!(target.reference_kind, rt.reference_kind);
    }

    // -- ModuleStub round-trip ----------------------------------------------

    #[test]
    fn module_stub_round_trip() {
        let module = ModuleStub {
            name: "java.base".to_owned(),
            access: AccessFlags::new(AccessFlags::ACC_MODULE),
            version: Some("17".to_owned()),
            requires: vec![ModuleRequires {
                module_name: "java.logging".to_owned(),
                access: AccessFlags::empty(),
                version: None,
            }],
            exports: vec![ModuleExports {
                package: "java.lang".to_owned(),
                access: AccessFlags::empty(),
                to_modules: vec![],
            }],
            opens: vec![ModuleOpens {
                package: "java.lang.invoke".to_owned(),
                access: AccessFlags::empty(),
                to_modules: vec!["jdk.internal.vm.ci".to_owned()],
            }],
            provides: vec![ModuleProvides {
                service: "java.security.Provider".to_owned(),
                implementations: vec!["sun.security.provider.Sun".to_owned()],
            }],
            uses: vec!["java.security.Provider".to_owned()],
        };
        let rt = round_trip(&module);
        assert_eq!(module.name, rt.name);
        assert_eq!(module.requires.len(), rt.requires.len());
        assert_eq!(module.exports.len(), rt.exports.len());
        assert_eq!(module.opens.len(), rt.opens.len());
        assert_eq!(module.provides.len(), rt.provides.len());
        assert_eq!(module.uses, rt.uses);
    }

    // -- RecordComponent round-trip -----------------------------------------

    #[test]
    fn record_component_round_trip() {
        let comp = RecordComponent {
            name: "name".to_owned(),
            descriptor: "Ljava/lang/String;".to_owned(),
            generic_signature: None,
            annotations: vec![AnnotationStub {
                type_fqn: "javax.annotation.Nonnull".to_owned(),
                elements: vec![],
                is_runtime_visible: true,
            }],
        };
        let rt = round_trip(&comp);
        assert_eq!(comp.name, rt.name);
        assert_eq!(comp.descriptor, rt.descriptor);
        assert_eq!(comp.annotations.len(), rt.annotations.len());
    }

    // -- InnerClassEntry round-trip -----------------------------------------

    #[test]
    fn inner_class_entry_round_trip() {
        let entry = InnerClassEntry {
            inner_fqn: "com.example.Outer.Inner".to_owned(),
            outer_fqn: Some("com.example.Outer".to_owned()),
            inner_name: Some("Inner".to_owned()),
            access: AccessFlags::new(AccessFlags::ACC_PUBLIC | AccessFlags::ACC_STATIC),
        };
        let rt = round_trip(&entry);
        assert_eq!(entry.inner_fqn, rt.inner_fqn);
        assert_eq!(entry.outer_fqn, rt.outer_fqn);
        assert_eq!(entry.inner_name, rt.inner_name);
        assert_eq!(entry.access.bits(), rt.access.bits());
    }

    #[test]
    fn inner_class_anonymous_round_trip() {
        let entry = InnerClassEntry {
            inner_fqn: "com.example.Outer$1".to_owned(),
            outer_fqn: None,
            inner_name: None,
            access: AccessFlags::empty(),
        };
        let rt = round_trip(&entry);
        assert_eq!(entry.inner_fqn, rt.inner_fqn);
        assert!(rt.outer_fqn.is_none());
        assert!(rt.inner_name.is_none());
    }

    // -- KotlinMetadataStub round-trip --------------------------------------

    #[test]
    fn kotlin_metadata_round_trip() {
        let meta = KotlinMetadataStub {
            kind: 1,
            metadata_version: vec![1, 9, 0],
            data1: vec!["proto_data_chunk_1".to_owned()],
            data2: vec!["string_table_entry".to_owned()],
            extra_string: Some("com/example/MyClass".to_owned()),
            package_name: Some("com.example".to_owned()),
            extra_int: Some(50),
        };
        let rt = round_trip(&meta);
        assert_eq!(meta.kind, rt.kind);
        assert_eq!(meta.metadata_version, rt.metadata_version);
        assert_eq!(meta.data1, rt.data1);
        assert_eq!(meta.data2, rt.data2);
        assert_eq!(meta.extra_string, rt.extra_string);
        assert_eq!(meta.package_name, rt.package_name);
        assert_eq!(meta.extra_int, rt.extra_int);
    }

    // -- ScalaSignatureStub round-trip --------------------------------------

    #[test]
    fn scala_signature_round_trip() {
        let sig = ScalaSignatureStub {
            bytes: vec![0x05, 0x00, 0x01, 0x09, 0x02],
            major_version: 5,
            minor_version: 0,
        };
        let rt = round_trip(&sig);
        assert_eq!(sig.bytes, rt.bytes);
        assert_eq!(sig.major_version, rt.major_version);
        assert_eq!(sig.minor_version, rt.minor_version);
    }

    // -- Full ClassStub with all optional fields ----------------------------

    #[test]
    fn class_stub_fully_populated_round_trip() {
        let stub = ClassStub {
            fqn: "com.example.FullClass".to_owned(),
            name: "FullClass".to_owned(),
            kind: ClassKind::Record,
            access: AccessFlags::new(AccessFlags::ACC_PUBLIC | AccessFlags::ACC_FINAL),
            superclass: Some("java.lang.Record".to_owned()),
            interfaces: vec![
                "java.io.Serializable".to_owned(),
                "java.lang.Comparable".to_owned(),
            ],
            methods: vec![sample_method_stub()],
            fields: vec![sample_field_stub()],
            annotations: vec![AnnotationStub {
                type_fqn: "java.lang.Deprecated".to_owned(),
                elements: vec![AnnotationElement {
                    name: "since".to_owned(),
                    value: AnnotationElementValue::Const(ConstantValue::String("17".to_owned())),
                }],
                is_runtime_visible: true,
            }],
            generic_signature: Some(GenericClassSignature {
                type_parameters: vec![TypeParameterStub {
                    name: "T".to_owned(),
                    class_bound: Some(TypeSignature::Class {
                        fqn: "java.lang.Number".to_owned(),
                        type_arguments: vec![],
                    }),
                    interface_bounds: vec![],
                }],
                superclass: TypeSignature::Class {
                    fqn: "java.lang.Record".to_owned(),
                    type_arguments: vec![],
                },
                interfaces: vec![],
            }),
            inner_classes: vec![InnerClassEntry {
                inner_fqn: "com.example.FullClass.Builder".to_owned(),
                outer_fqn: Some("com.example.FullClass".to_owned()),
                inner_name: Some("Builder".to_owned()),
                access: AccessFlags::new(AccessFlags::ACC_PUBLIC | AccessFlags::ACC_STATIC),
            }],
            lambda_targets: vec![LambdaTargetStub {
                owner_fqn: "com.example.FullClass".to_owned(),
                method_name: "lambda$process$0".to_owned(),
                method_descriptor: "(Ljava/lang/Object;)V".to_owned(),
                reference_kind: ReferenceKind::InvokeStatic,
            }],
            module: None,
            record_components: vec![RecordComponent {
                name: "value".to_owned(),
                descriptor: "Ljava/lang/Number;".to_owned(),
                generic_signature: Some(TypeSignature::TypeVariable("T".to_owned())),
                annotations: vec![],
            }],
            enum_constants: vec![],
            source_file: Some("FullClass.java".to_owned()),
            source_jar: Some("/jars/full.jar".to_owned()),
            kotlin_metadata: Some(KotlinMetadataStub {
                kind: 1,
                metadata_version: vec![1, 9, 0],
                data1: vec![],
                data2: vec![],
                extra_string: None,
                package_name: None,
                extra_int: None,
            }),
            scala_signature: Some(ScalaSignatureStub {
                bytes: vec![0x05, 0x00],
                major_version: 5,
                minor_version: 0,
            }),
        };

        let bytes = postcard::to_allocvec(&stub).expect("serialization should succeed");
        assert!(!bytes.is_empty(), "serialized bytes should not be empty");
        let rt: ClassStub = postcard::from_bytes(&bytes).expect("deserialization should succeed");
        assert_eq!(stub.fqn, rt.fqn);
        assert_eq!(stub.kind, rt.kind);
        assert_eq!(stub.methods.len(), rt.methods.len());
        assert_eq!(stub.fields.len(), rt.fields.len());
        assert_eq!(stub.inner_classes.len(), rt.inner_classes.len());
        assert_eq!(stub.lambda_targets.len(), rt.lambda_targets.len());
        assert_eq!(stub.record_components.len(), rt.record_components.len());
        assert!(rt.generic_signature.is_some());
        assert!(rt.kotlin_metadata.is_some());
        assert!(rt.scala_signature.is_some());
    }

    // -- ClassStub with module info -----------------------------------------

    #[test]
    fn class_stub_module_round_trip() {
        let stub = ClassStub {
            fqn: "module-info".to_owned(),
            name: "module-info".to_owned(),
            kind: ClassKind::Module,
            access: AccessFlags::new(AccessFlags::ACC_MODULE),
            superclass: None,
            interfaces: vec![],
            methods: vec![],
            fields: vec![],
            annotations: vec![],
            generic_signature: None,
            inner_classes: vec![],
            lambda_targets: vec![],
            module: Some(ModuleStub {
                name: "com.example.app".to_owned(),
                access: AccessFlags::new(AccessFlags::ACC_MODULE),
                version: Some("1.0".to_owned()),
                requires: vec![
                    ModuleRequires {
                        module_name: "java.base".to_owned(),
                        access: AccessFlags::empty(),
                        version: Some("17".to_owned()),
                    },
                    ModuleRequires {
                        module_name: "java.sql".to_owned(),
                        access: AccessFlags::empty(),
                        version: None,
                    },
                ],
                exports: vec![ModuleExports {
                    package: "com.example.api".to_owned(),
                    access: AccessFlags::empty(),
                    to_modules: vec![],
                }],
                opens: vec![],
                provides: vec![],
                uses: vec![],
            }),
            record_components: vec![],
            enum_constants: vec![],
            source_file: None,
            source_jar: None,
            kotlin_metadata: None,
            scala_signature: None,
        };

        let rt = round_trip(&stub);
        assert_eq!(stub.kind, rt.kind);
        let module = rt.module.expect("module should be present");
        assert_eq!(module.name, "com.example.app");
        assert_eq!(module.requires.len(), 2);
    }

    // -- ClassStub with enum constants --------------------------------------

    #[test]
    fn class_stub_enum_round_trip() {
        let stub = ClassStub {
            fqn: "com.example.Color".to_owned(),
            name: "Color".to_owned(),
            kind: ClassKind::Enum,
            access: AccessFlags::new(
                AccessFlags::ACC_PUBLIC | AccessFlags::ACC_FINAL | AccessFlags::ACC_ENUM,
            ),
            superclass: Some("java.lang.Enum".to_owned()),
            interfaces: vec![],
            methods: vec![],
            fields: vec![],
            annotations: vec![],
            generic_signature: None,
            inner_classes: vec![],
            lambda_targets: vec![],
            module: None,
            record_components: vec![],
            enum_constants: vec!["RED".to_owned(), "GREEN".to_owned(), "BLUE".to_owned()],
            source_file: Some("Color.java".to_owned()),
            source_jar: None,
            kotlin_metadata: None,
            scala_signature: None,
        };

        let rt = round_trip(&stub);
        assert_eq!(stub.kind, rt.kind);
        assert_eq!(
            stub.enum_constants, rt.enum_constants,
            "enum constants should survive round-trip in order"
        );
    }

    // -- Send + Sync static assertions --------------------------------------

    #[test]
    fn types_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ClassStub>();
        assert_send_sync::<MethodStub>();
        assert_send_sync::<FieldStub>();
        assert_send_sync::<AnnotationStub>();
        assert_send_sync::<GenericClassSignature>();
        assert_send_sync::<GenericMethodSignature>();
        assert_send_sync::<TypeSignature>();
        assert_send_sync::<LambdaTargetStub>();
        assert_send_sync::<ModuleStub>();
        assert_send_sync::<RecordComponent>();
        assert_send_sync::<InnerClassEntry>();
        assert_send_sync::<KotlinMetadataStub>();
        assert_send_sync::<ScalaSignatureStub>();
        assert_send_sync::<ConstantValue>();
        assert_send_sync::<AccessFlags>();
        assert_send_sync::<ClassKind>();
        assert_send_sync::<ReferenceKind>();
        assert_send_sync::<BaseType>();
        assert_send_sync::<TypeArgument>();
        assert_send_sync::<WildcardBound>();
        assert_send_sync::<OrderedFloat<f32>>();
        assert_send_sync::<OrderedFloat<f64>>();
    }
}
