//! Shared metadata key constants for language plugins
//!
//! This module provides standardized metadata keys that language plugins use to
//! annotate symbols with language-specific attributes. Using these constants
//! ensures consistency across plugins and prevents typos in metadata keys.
//!
//! # Design Principles
//!
//! 1. **Naming Convention**: All keys use snake_case with `is_` prefix for booleans
//! 2. **Organization**: Keys are grouped by symbol type (function, class, property, etc.)
//! 3. **Documentation**: Each key includes its type, applicable symbol types, and examples
//! 4. **Stability**: Once published, keys should not be renamed to maintain compatibility
//!
//! # Usage
//!
//! Language plugins should import this module and use these constants instead of
//! hard-coded strings:
//!
//! ```rust
//! use sqry_core::metadata::keys;
//! use std::collections::HashMap;
//!
//! let mut metadata = HashMap::new();
//!
//! // Good: Using constants
//! metadata.insert(keys::IS_ASYNC.to_string(), "true".to_string());
//!
//! // Bad: Hard-coded string (prone to typos)
//! // metadata.insert("is_async".to_string(), "true".to_string());
//! ```

/// Metadata key constants organized by category
pub mod keys {
    // ========================================================================
    // Function Metadata
    // ========================================================================

    /// Whether a function is asynchronous
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions, Methods
    /// **Languages**: JavaScript, TypeScript, Python, Swift, C#, Dart, Kotlin, etc.
    ///
    /// # Examples
    /// - JavaScript/TypeScript: `async function foo() {}`
    /// - Python: `async def foo():`
    /// - Swift: `func foo() async {}`
    /// - C#: `async Task FooAsync()`
    pub const IS_ASYNC: &str = "is_async";

    /// Whether a function can throw/raise exceptions
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions, Methods
    /// **Languages**: Swift, Kotlin, Java, C++, Python, etc.
    ///
    /// # Examples
    /// - Swift: `func foo() throws {}`
    /// - Kotlin: `@Throws(IOException::class) fun foo()`
    /// - Java: `void foo() throws IOException`
    /// - C++: `void foo() noexcept(false)`
    pub const IS_THROWING: &str = "is_throwing";

    /// Whether a function is static (class-level, not instance)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions, Methods
    /// **Languages**: Most OOP languages
    ///
    /// # Examples
    /// - Java/C#: `static void foo()`
    /// - Python: `@staticmethod`
    /// - Swift: `static func foo()`
    pub const IS_STATIC: &str = "is_static";

    /// Whether a function is abstract (no implementation)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Methods, Functions
    /// **Languages**: Java, C#, Kotlin, TypeScript, etc.
    ///
    /// # Examples
    /// - Java: `abstract void foo();`
    /// - C#: `abstract void Foo();`
    /// - TypeScript: `abstract foo(): void;`
    pub const IS_ABSTRACT: &str = "is_abstract";

    /// Whether a function is marked as final (cannot be overridden)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Methods, Classes
    /// **Languages**: Java, Kotlin, Swift, etc.
    ///
    /// # Examples
    /// - Java: `final void foo()`
    /// - Kotlin: `final fun foo()`
    /// - Swift: `final func foo()`
    pub const IS_FINAL: &str = "is_final";

    /// Whether a method overrides a parent class method
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Methods
    /// **Languages**: Java, C#, Kotlin, Swift, TypeScript, etc.
    ///
    /// # Examples
    /// - Java: `@Override void foo()`
    /// - C#: `override void Foo()`
    /// - Swift: `override func foo()`
    pub const IS_OVERRIDE: &str = "is_override";

    /// Whether a function mutates its containing struct/enum (Swift, Rust)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Methods
    /// **Languages**: Swift, Rust
    ///
    /// # Examples
    /// - Swift: `mutating func increment()`
    /// - Rust: `fn increment(&mut self)`
    pub const IS_MUTATING: &str = "is_mutating";

    /// Whether a function is a generator (can yield values)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions
    /// **Languages**: JavaScript, TypeScript, Python, C#, etc.
    ///
    /// # Examples
    /// - JavaScript: `function* generator()`
    /// - Python: `def generator(): yield x`
    pub const IS_GENERATOR: &str = "is_generator";

    /// Whether a function is exported from a module
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions, Classes, Variables
    /// **Languages**: JavaScript, TypeScript, Rust, Go, etc.
    ///
    /// # Examples
    /// - JavaScript: `export function foo()`
    /// - Rust: `pub fn foo()`
    /// - Go: `func Foo()` (capitalized)
    pub const IS_EXPORTED: &str = "is_exported";

    /// Total number of clauses defined for a function (Elixir multi-clause)
    ///
    /// **Type**: Integer (as string)
    /// **Applies to**: Functions
    pub const CLAUSE_COUNT: &str = "clause_count";

    /// Whether a definition is a macro (Elixir)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions
    pub const IS_MACRO: &str = "is_macro";

    /// Function/method arity (number of declared parameters)
    ///
    /// **Type**: Integer (stored as string)
    /// **Applies to**: Functions, Methods
    /// **Languages**: All (used by new plugins for analytics)
    pub const ARITY: &str = "arity";

    /// Whether a function has guard clauses (Elixir)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions
    pub const HAS_GUARDS: &str = "has_guards";

    /// Number of guard clauses present (Elixir)
    ///
    /// **Type**: Integer (as string)
    /// **Applies to**: Functions
    pub const GUARD_COUNT: &str = "guard_count";

    /// Whether a function accepts varargs / variadic parameters
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions, Methods
    pub const HAS_VARARGS: &str = "has_varargs";

    /// Generic name for S3/S4 method registrations (R)
    ///
    /// **Type**: String
    /// **Applies to**: Functions, Methods
    pub const GENERIC: &str = "generic";

    /// Class name for S3/S4/R6 method registrations (R)
    ///
    /// **Type**: String
    /// **Applies to**: Functions, Methods
    pub const CLASS: &str = "class";

    /// Whether a function represents an R6 class method or constructor
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Classes, Methods
    pub const IS_R6: &str = "is_r6";

    /// Roxygen tag list serialized as comma-separated string
    ///
    /// **Type**: String
    /// **Applies to**: Functions, Classes
    pub const ROXYGEN_TAGS: &str = "roxygen_tags";

    // ========================================================================
    // Class/Type Metadata
    // ========================================================================

    /// Whether a type is a struct (value type)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Classes
    /// **Languages**: Swift, C#, Rust, Go, C++, etc.
    ///
    /// # Examples
    /// - Swift: `struct Point {}`
    /// - C#: `struct Point {}`
    /// - Rust: `struct Point {}`
    pub const IS_STRUCT: &str = "is_struct";

    /// Whether a type is an enum
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Classes
    /// **Languages**: Most languages
    ///
    /// # Examples
    /// - Java: `enum Color {}`
    /// - Swift: `enum Color {}`
    /// - TypeScript: `enum Color {}`
    pub const IS_ENUM: &str = "is_enum";

    /// Whether a type is an interface/protocol
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Classes
    /// **Languages**: Java, TypeScript, Swift, Kotlin, etc.
    ///
    /// # Examples
    /// - Java: `interface Drawable {}`
    /// - TypeScript: `interface Drawable {}`
    /// - Swift: `protocol Drawable {}`
    pub const IS_INTERFACE: &str = "is_interface";

    /// Alternative name for `IS_INTERFACE` (Swift-specific)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Classes
    /// **Languages**: Swift
    ///
    /// **Note**: Swift calls interfaces "protocols". This is an alias for `IS_INTERFACE`.
    pub const IS_PROTOCOL: &str = "is_protocol";

    /// Receiver/table name for method definitions (Lua, JavaScript, etc.)
    ///
    /// **Type**: String (receiver identifier)
    /// **Applies to**: Methods
    pub const RECEIVER: &str = "receiver";

    /// Whether a type is an actor (concurrency primitive)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Classes
    /// **Languages**: Swift, Dart
    ///
    /// # Examples
    /// - Swift: `actor DataManager {}`
    pub const IS_ACTOR: &str = "is_actor";

    /// Whether a type is an extension (adds functionality to existing type)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Classes
    /// **Languages**: Swift, Kotlin, C#, Dart
    ///
    /// # Examples
    /// - Swift: `extension String {}`
    /// - Kotlin: `fun String.reversed()`
    /// - C#: `static class StringExtensions {}`
    pub const IS_EXTENSION: &str = "is_extension";

    /// Whether a type is a trait (Rust)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Classes
    /// **Languages**: Rust
    ///
    /// # Examples
    /// - Rust: `trait Display {}`
    pub const IS_TRAIT: &str = "is_trait";

    /// Whether a type is sealed (can only be subclassed in same module/assembly)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Classes
    /// **Languages**: Kotlin, C#, Scala
    ///
    /// # Examples
    /// - Kotlin: `sealed class Result {}`
    /// - C#: `sealed class Logger {}`
    pub const IS_SEALED: &str = "is_sealed";

    /// Whether a type has generic type parameters
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Classes, Functions
    /// **Languages**: Most statically-typed languages
    ///
    /// # Examples
    /// - Java: `class Box<T> {}`
    /// - TypeScript: `class Box<T> {}`
    /// - Swift: `class Box<T> {}`
    pub const HAS_GENERICS: &str = "has_generics";

    // ========================================================================
    // Script/Environment Metadata
    // ========================================================================

    /// Shell variant detected from shebang (bash, sh, zsh, etc.)
    ///
    /// **Type**: String (e.g., "bash", "sh", "zsh")
    /// **Applies to**: Script-level symbols, functions (Shell plugin)
    pub const SHELL_VARIANT: &str = "shell_variant";

    // ========================================================================
    // Property/Field Metadata
    // ========================================================================

    /// Whether a property is computed (has getter/setter, not stored)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Properties
    /// **Languages**: Swift, C#, Kotlin, Python, etc.
    ///
    /// # Examples
    /// - Swift: `var computed: Int { get { return 42 } }`
    /// - C#: `int Computed { get { return 42; } }`
    pub const IS_COMPUTED: &str = "is_computed";

    /// Whether a property is lazy-initialized
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Properties
    /// **Languages**: Swift, Kotlin, C#, etc.
    ///
    /// # Examples
    /// - Swift: `lazy var data = loadData()`
    /// - Kotlin: `val data by lazy { loadData() }`
    pub const IS_LAZY: &str = "is_lazy";

    /// Whether a property is a weak reference (doesn't prevent deallocation)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Properties
    /// **Languages**: Swift, Objective-C, Dart
    ///
    /// # Examples
    /// - Swift: `weak var delegate: Delegate?`
    /// - Dart: `WeakReference<Delegate> delegate;`
    pub const IS_WEAK: &str = "is_weak";

    /// Whether a property is mutable (can be modified after initialization)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Properties
    /// **Languages**: Most languages
    ///
    /// # Examples
    /// - Rust: `let mut x = 5;`
    /// - Swift: `var x = 5` (vs `let x = 5`)
    /// - Kotlin: `var x = 5` (vs `val x = 5`)
    pub const IS_MUTABLE: &str = "is_mutable";

    // ========================================================================
    // Visibility/Access Control Metadata
    // ========================================================================

    /// Visibility modifier for the symbol
    ///
    /// **Type**: String (public, private, protected, internal, etc.)
    /// **Applies to**: All symbol types
    /// **Languages**: Most languages
    ///
    /// # Common Values
    /// - `public`: Accessible everywhere
    /// - `private`: Accessible only within same class/file
    /// - `protected`: Accessible within class and subclasses
    /// - `internal`: Accessible within same module/package (Swift, C#, Kotlin)
    /// - `fileprivate`: Accessible within same file (Swift)
    /// - `package`: Accessible within same package (Java, Dart)
    pub const VISIBILITY: &str = "visibility";

    // ========================================================================
    // Markup & Styles Metadata
    // ========================================================================

    /// HTML element tag name (e.g., div, span, custom-element)
    ///
    /// **Type**: String
    /// **Applies to**: Markup element symbols (HTML, XML-like plugins)
    pub const ELEMENT_TAG: &str = "element_tag";

    /// HTML element id attribute (if present)
    ///
    /// **Type**: String
    /// **Applies to**: Markup element symbols
    pub const ELEMENT_ID: &str = "element_id";

    /// HTML element class list (space-delimited in source, stored comma-separated)
    ///
    /// **Type**: Comma-separated string
    /// **Applies to**: Markup element symbols
    pub const CLASS_LIST: &str = "class_list";

    /// Whether the HTML element is a custom element (contains a dash in tag name)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Markup element symbols
    pub const IS_CUSTOM_ELEMENT: &str = "is_custom_element";

    /// Whether the element represents a `<template>` block
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Markup element symbols
    pub const IS_TEMPLATE: &str = "is_template";

    /// Primary selector associated with a stylesheet rule (e.g., `.btn.primary`)
    ///
    /// **Type**: String
    /// **Applies to**: Stylesheet rule symbols (CSS, SCSS, etc.)
    pub const SELECTOR: &str = "selector";

    /// Name or kind of an at-rule (e.g., `media`, `supports`, `keyframes`)
    ///
    /// **Type**: String
    /// **Applies to**: Stylesheet at-rule symbols
    pub const AT_RULE: &str = "at_rule";

    /// Serialized query/prelude associated with an at-rule (e.g., `screen and (max-width: 768px)`)
    ///
    /// **Type**: String
    /// **Applies to**: Stylesheet at-rule symbols
    pub const AT_RULE_PRELUDE: &str = "at_rule_prelude";

    /// Custom property identifier (`--color-primary`)
    ///
    /// **Type**: String
    /// **Applies to**: Stylesheet custom property symbols
    pub const CUSTOM_PROPERTY: &str = "custom_property";

    /// Resource kind associated with relation edges extracted from markup/style files
    ///
    /// **Type**: String (e.g., `script`, `style`, `image`, `font`)
    /// **Applies to**: Import relations metadata
    pub const RESOURCE_KIND: &str = "resource_kind";

    // ========================================================================
    // Haskell Metadata
    // ========================================================================

    /// Embedded type signature associated with a function/value declaration
    ///
    /// **Type**: String
    /// **Applies to**: Functions, Variables
    pub const TYPE_SIGNATURE: &str = "type_signature";

    /// Whether a declaration has a paired type signature
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions, Variables
    pub const HAS_TYPE_SIGNATURE: &str = "has_type_signature";

    /// Whether the declaration or module contains Template Haskell constructs
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions, Modules
    pub const IS_TEMPLATE_HASKELL: &str = "is_template_haskell";

    /// Whether the function name is an operator definition
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions
    pub const IS_OPERATOR: &str = "is_operator";

    /// Module export list (comma separated)
    ///
    /// **Type**: String (comma-separated entries)
    /// **Applies to**: Modules
    pub const EXPORTS: &str = "exports";

    /// Recorded type class for an instance declaration
    ///
    /// **Type**: String
    /// **Applies to**: Classes (instances)
    pub const INSTANCE_CLASS: &str = "instance_class";

    /// Type target for an instance declaration
    ///
    /// **Type**: String
    /// **Applies to**: Classes (instances)
    pub const INSTANCE_TYPE: &str = "instance_type";

    /// Flag marking a symbol as type class instance declaration
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Classes (instances)
    pub const IS_INSTANCE: &str = "is_instance";

    /// Number of constructors associated with a data declaration
    ///
    /// **Type**: Integer (stored as string)
    /// **Applies to**: Struct symbols (Haskell data)
    pub const CONSTRUCTOR_COUNT: &str = "constructor_count";

    // ========================================================================
    // Perl Metadata
    // ========================================================================

    /// Package / namespace name for Perl modules
    pub const PACKAGE_NAME: &str = "package_name";

    /// Prototype string declared for a subroutine
    pub const PROTOTYPE: &str = "prototype";

    /// Attribute list applied to a subroutine or variable (comma-separated)
    pub const ATTRIBUTES: &str = "attributes";

    /// Signature parameters for Perl subs (string form)
    pub const SIGNATURE: &str = "signature";

    /// Indicates the symbol originates from Moose/Moo DSL helpers
    pub const IS_MOOSE_METHOD: &str = "is_moose_method";

    /// Flag for anonymous subroutines captured as symbols
    pub const IS_ANONYMOUS: &str = "is_anonymous";

    // ========================================================================
    // Language-Specific Metadata
    // ========================================================================

    /// Python: Whether a method is decorated with @classmethod
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Methods
    /// **Languages**: Python
    pub const IS_CLASSMETHOD: &str = "is_classmethod";

    /// Python: Whether a method is decorated with @staticmethod
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Methods
    /// **Languages**: Python
    pub const IS_STATICMETHOD: &str = "is_staticmethod";

    /// Python: Whether a method is decorated with @property
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Methods
    /// **Languages**: Python
    pub const IS_PROPERTY_DECORATOR: &str = "is_property_decorator";

    /// Go: Whether a function is a method (has receiver)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions
    /// **Languages**: Go
    pub const HAS_RECEIVER: &str = "has_receiver";

    /// Go: Whether a method has a pointer receiver
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Methods
    /// **Languages**: Go
    pub const IS_POINTER_RECEIVER: &str = "is_pointer_receiver";

    /// Go: Struct field tags aggregated from struct definition
    ///
    /// **Type**: String (comma-separated tag names like "json,db,validate")
    /// **Applies to**: Struct types
    /// **Languages**: Go
    ///
    /// # Examples
    /// - ``type User struct { ID int `json:"id" db:"user_id"` }`` → `"json,db"`
    /// - ``type Config struct { Name string `yaml:"name" mapstructure:"name"` }`` → `"yaml,mapstructure"`
    ///
    /// # Notes
    /// - Stores only the tag names, not values (e.g., `json` not `json:"id,omitempty"`)
    /// - Multiple tags are comma-separated: `"json,db,validate"`
    /// - Tags are aggregated from all fields in the struct
    pub const STRUCT_TAGS: &str = "struct_tags";

    /// Java/C#: Whether a method is synchronized (thread-safe)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Methods
    /// **Languages**: Java, C#
    pub const IS_SYNCHRONIZED: &str = "is_synchronized";

    /// C++: Whether a function is constexpr (compile-time evaluable)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions
    /// **Languages**: C++
    pub const IS_CONSTEXPR: &str = "is_constexpr";

    /// C++: Whether a function is const (doesn't modify object state)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Methods
    /// **Languages**: C++
    pub const IS_CONST: &str = "is_const";

    /// C++: Whether a function is noexcept (guaranteed not to throw)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions
    /// **Languages**: C++
    pub const IS_NOEXCEPT: &str = "is_noexcept";

    /// Rust: Whether a function is unsafe
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions
    /// **Languages**: Rust
    pub const IS_UNSAFE: &str = "is_unsafe";

    /// Rust: Whether a function is const (compile-time evaluable)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions
    /// **Languages**: Rust
    pub const IS_CONST_FN: &str = "is_const_fn";

    /// TypeScript: Whether a symbol is readonly
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Properties
    /// **Languages**: TypeScript
    pub const IS_READONLY: &str = "is_readonly";

    /// Dart: Whether a factory constructor
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions
    /// **Languages**: Dart
    pub const IS_FACTORY: &str = "is_factory";

    /// Dart: Whether a method is external (implemented outside Dart)
    ///
    /// **Type**: Boolean (true/false as string)
    /// **Applies to**: Functions
    /// **Languages**: Dart
    pub const IS_EXTERNAL: &str = "is_external";

    // ========================================================================
    // Decorator Metadata
    // ========================================================================

    /// Comma-separated list of decorator names applied to a symbol
    ///
    /// **Type**: String (comma-separated decorator names)
    /// **Applies to**: Functions, Methods, Classes, Properties, Parameters
    /// **Languages**: TypeScript, Python, Java (annotations), C# (attributes), Kotlin
    ///
    /// # Examples
    /// - TypeScript: `@Component, @Injectable` from `@Component({...}) @Injectable() class MyService`
    /// - Python: `staticmethod, dataclass` from `@staticmethod @dataclass class Config`
    /// - Java: `Override, Deprecated` from `@Override @Deprecated void foo()`
    ///
    /// # Notes
    /// - Stores only the decorator name, not arguments (e.g., `Component` not `Component({...})`)
    /// - Multiple decorators are comma-separated: `"Component,Injectable"`
    /// - Arguments can be extracted separately if needed via dedicated keys
    pub const DECORATORS: &str = "decorators";

    /// Python: Comma-separated list of parameter type annotations
    ///
    /// **Type**: String (JSON array of type names)
    /// **Applies to**: Functions, Methods
    /// **Languages**: Python
    ///
    /// # Examples
    /// - `def foo(x: int, name: str) -> bool:` → `["int","str"]`
    /// - `def bar(items: list[int], config: dict[str, Any]) -> None:` → `["list[int]","dict[str, Any]"]`
    /// - `def baz(user: User, options: Optional[Config] = None) -> Response:` → `["User","Optional[Config]"]`
    /// - `def log(*msgs: str, **opts: Any) -> None:` → `["str","Any"]` (variadic types)
    ///
    /// # Notes
    /// - Stored as JSON array to safely handle types containing commas (e.g., `dict[str, Any]`)
    /// - Stores only the type annotations, not parameter names
    /// - Generic types are preserved: `list[int]`, `Optional[str]`, `dict[str, Any]`
    /// - Variadic parameters (`*args`, `**kwargs`) types are included
    /// - Parameters without type annotations are omitted
    /// - For methods, `self`/`cls` parameters are excluded (not for top-level functions)
    /// - Whitespace is normalized (newlines/multi-space → single space)
    /// - Query syntax: `parameter_types:User` matches functions with a User parameter
    /// - Regex queries: `parameter_types~=/dict/` matches functions with any dict-like parameter
    pub const PARAMETER_TYPES: &str = "parameter_types";
}

#[cfg(test)]
mod tests {
    use super::keys;

    #[test]
    fn test_metadata_keys_exist() {
        // Verify all keys are defined and have expected values
        assert_eq!(keys::IS_ASYNC, "is_async");
        assert_eq!(keys::IS_THROWING, "is_throwing");
        assert_eq!(keys::IS_STATIC, "is_static");
        assert_eq!(keys::IS_ABSTRACT, "is_abstract");
        assert_eq!(keys::IS_FINAL, "is_final");
        assert_eq!(keys::IS_OVERRIDE, "is_override");
        assert_eq!(keys::IS_MUTATING, "is_mutating");
        assert_eq!(keys::IS_GENERATOR, "is_generator");
        assert_eq!(keys::IS_EXPORTED, "is_exported");
        assert_eq!(keys::CLAUSE_COUNT, "clause_count");
        assert_eq!(keys::IS_MACRO, "is_macro");
        assert_eq!(keys::ARITY, "arity");
        assert_eq!(keys::HAS_GUARDS, "has_guards");
        assert_eq!(keys::GUARD_COUNT, "guard_count");
        assert_eq!(keys::HAS_VARARGS, "has_varargs");
        assert_eq!(keys::GENERIC, "generic");
        assert_eq!(keys::CLASS, "class");
        assert_eq!(keys::IS_R6, "is_r6");
        assert_eq!(keys::ROXYGEN_TAGS, "roxygen_tags");

        assert_eq!(keys::IS_STRUCT, "is_struct");
        assert_eq!(keys::IS_ENUM, "is_enum");
        assert_eq!(keys::IS_INTERFACE, "is_interface");
        assert_eq!(keys::IS_PROTOCOL, "is_protocol");
        assert_eq!(keys::IS_ACTOR, "is_actor");
        assert_eq!(keys::IS_EXTENSION, "is_extension");
        assert_eq!(keys::IS_TRAIT, "is_trait");
        assert_eq!(keys::IS_SEALED, "is_sealed");
        assert_eq!(keys::HAS_GENERICS, "has_generics");
        assert_eq!(keys::RECEIVER, "receiver");
        assert_eq!(keys::SHELL_VARIANT, "shell_variant");

        assert_eq!(keys::IS_COMPUTED, "is_computed");
        assert_eq!(keys::IS_LAZY, "is_lazy");
        assert_eq!(keys::IS_WEAK, "is_weak");
        assert_eq!(keys::IS_MUTABLE, "is_mutable");

        assert_eq!(keys::VISIBILITY, "visibility");
    }

    #[test]
    fn test_protocol_is_alias_for_interface() {
        // Swift's "protocol" is semantically equivalent to "interface"
        // Both keys should exist for compatibility
        assert_eq!(keys::IS_PROTOCOL, "is_protocol");
        assert_eq!(keys::IS_INTERFACE, "is_interface");
    }

    #[test]
    fn test_language_specific_keys() {
        // Python
        assert_eq!(keys::IS_CLASSMETHOD, "is_classmethod");
        assert_eq!(keys::IS_STATICMETHOD, "is_staticmethod");
        assert_eq!(keys::IS_PROPERTY_DECORATOR, "is_property_decorator");
        assert_eq!(keys::PARAMETER_TYPES, "parameter_types");

        // Go
        assert_eq!(keys::HAS_RECEIVER, "has_receiver");
        assert_eq!(keys::IS_POINTER_RECEIVER, "is_pointer_receiver");
        assert_eq!(keys::STRUCT_TAGS, "struct_tags");

        // C++
        assert_eq!(keys::IS_CONSTEXPR, "is_constexpr");
        assert_eq!(keys::IS_CONST, "is_const");
        assert_eq!(keys::IS_NOEXCEPT, "is_noexcept");

        // Rust
        assert_eq!(keys::IS_UNSAFE, "is_unsafe");
        assert_eq!(keys::IS_CONST_FN, "is_const_fn");
    }
}
