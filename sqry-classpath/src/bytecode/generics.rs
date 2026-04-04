//! Recursive descent parser for JVM generic signatures (JVMS 4.7.9.1).
//!
//! The JVM stores generic type information in `Signature` attributes as compact
//! strings following a specific grammar. This module parses those strings into
//! the structured types defined in [`crate::stub::model`].
//!
//! # Grammar (JVMS 4.7.9.1)
//!
//! ```text
//! ClassSignature     = FormalTypeParameters? SuperclassSignature SuperinterfaceSignature*
//! MethodSignature    = FormalTypeParameters? '(' TypeSignature* ')' ReturnType ThrowsSignature*
//! FormalTypeParameters = '<' FormalTypeParameter+ '>'
//! FormalTypeParameter = Identifier ClassBound InterfaceBound*
//! ClassBound         = ':' FieldTypeSignature?
//! InterfaceBound     = ':' FieldTypeSignature
//! FieldTypeSignature = ClassTypeSignature | ArrayTypeSignature | TypeVariableSignature
//! ClassTypeSignature = 'L' (Identifier '/')* Identifier TypeArguments? ('.' Identifier TypeArguments?)* ';'
//! TypeArguments      = '<' TypeArgument+ '>'
//! TypeArgument       = WildcardIndicator? FieldTypeSignature | '*'
//! WildcardIndicator  = '+' | '-'
//! TypeVariableSignature = 'T' Identifier ';'
//! ArrayTypeSignature = '[' TypeSignature
//! TypeSignature      = FieldTypeSignature | BaseType
//! ReturnType         = TypeSignature | 'V'
//! ThrowsSignature    = '^' ClassTypeSignature | '^' TypeVariableSignature
//! BaseType           = 'B' | 'C' | 'D' | 'F' | 'I' | 'J' | 'S' | 'Z'
//! ```
//!
//! # Examples
//!
//! ```
//! use sqry_classpath::bytecode::generics::{parse_class_signature, parse_method_signature, parse_field_signature};
//!
//! // HashMap<K, V> extends AbstractMap<K, V> implements Map<K, V>
//! let sig = "<K:Ljava/lang/Object;V:Ljava/lang/Object;>Ljava/util/AbstractMap<TK;TV;>;Ljava/util/Map<TK;TV;>;";
//! let parsed = parse_class_signature(sig).unwrap();
//! assert_eq!(parsed.type_parameters.len(), 2);
//!
//! // <T:Object>(T)T
//! let method_sig = "<T:Ljava/lang/Object;>(TT;)TT;";
//! let parsed = parse_method_signature(method_sig).unwrap();
//! assert_eq!(parsed.type_parameters.len(), 1);
//!
//! // List<String>
//! let field_sig = "Ljava/util/List<Ljava/lang/String;>;";
//! let parsed = parse_field_signature(field_sig).unwrap();
//! ```

use crate::stub::model::{
    BaseType, GenericClassSignature, GenericMethodSignature, TypeArgument, TypeParameterStub,
    TypeSignature,
};
use crate::{ClasspathError, ClasspathResult};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a class-level generic signature (JVMS 4.7.9.1 `ClassSignature`).
///
/// The input is the raw string from the `Signature` attribute of a class file.
///
/// # Example
///
/// ```text
/// <K:Ljava/lang/Object;V:Ljava/lang/Object;>Ljava/util/AbstractMap<TK;TV;>;Ljava/util/Map<TK;TV;>;
/// ```
///
/// # Errors
///
/// Returns `ClasspathError::BytecodeParseError` if the signature is malformed.
pub fn parse_class_signature(input: &str) -> ClasspathResult<GenericClassSignature> {
    let mut parser = SignatureParser::new(input);
    let result = parser.parse_class_signature()?;
    parser.expect_end()?;
    Ok(result)
}

/// Parse a method-level generic signature (JVMS 4.7.9.1 `MethodSignature`).
///
/// # Example
///
/// ```text
/// <T:Ljava/lang/Object;>(TT;)TT;
/// ```
///
/// # Errors
///
/// Returns `ClasspathError::BytecodeParseError` if the signature is malformed.
pub fn parse_method_signature(input: &str) -> ClasspathResult<GenericMethodSignature> {
    let mut parser = SignatureParser::new(input);
    let result = parser.parse_method_signature()?;
    parser.expect_end()?;
    Ok(result)
}

/// Parse a field-level type signature (JVMS 4.7.9.1 `FieldTypeSignature`).
///
/// This handles class type signatures, array type signatures, and type variable
/// signatures. It does **not** accept bare base types.
///
/// # Example
///
/// ```text
/// Ljava/util/List<Ljava/lang/String;>;
/// ```
///
/// # Errors
///
/// Returns `ClasspathError::BytecodeParseError` if the signature is malformed.
pub fn parse_field_signature(input: &str) -> ClasspathResult<TypeSignature> {
    let mut parser = SignatureParser::new(input);
    let result = parser.parse_field_type_signature()?;
    parser.expect_end()?;
    Ok(result)
}

// ---------------------------------------------------------------------------
// Parser internals
// ---------------------------------------------------------------------------

/// Byte-level cursor over a JVM generic signature string.
///
/// All positions and reads operate on the raw UTF-8 bytes. JVM signatures are
/// specified as Modified UTF-8 in the constant pool, but the signature grammar
/// itself only uses ASCII characters for delimiters. Identifiers (class names,
/// type parameter names) may theoretically contain non-ASCII characters, but in
/// practice they are ASCII.
struct SignatureParser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> SignatureParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    // -- Primitives ----------------------------------------------------------

    /// Peek at the current byte without advancing.
    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    /// Advance the cursor by one byte.
    fn advance(&mut self) {
        self.pos += 1;
    }

    /// Consume the expected byte or return an error.
    fn expect(&mut self, expected: u8) -> ClasspathResult<()> {
        match self.peek() {
            Some(b) if b == expected => {
                self.advance();
                Ok(())
            }
            Some(b) => Err(self.error(format!(
                "expected '{}' but found '{}' at position {}",
                expected as char, b as char, self.pos
            ))),
            None => Err(self.error(format!(
                "expected '{}' but reached end of input at position {}",
                expected as char, self.pos
            ))),
        }
    }

    /// Assert that the entire input has been consumed.
    fn expect_end(&self) -> ClasspathResult<()> {
        if self.pos == self.input.len() {
            Ok(())
        } else {
            let remaining = &self.input[self.pos..];
            let remaining_str = std::str::from_utf8(remaining).unwrap_or("<invalid utf8>");
            Err(self.error(format!(
                "unexpected trailing input at position {}: {:?}",
                self.pos, remaining_str
            )))
        }
    }

    /// Read an identifier — a sequence of bytes terminated by one of the
    /// delimiter characters used in the signature grammar.
    ///
    /// Delimiters: `:`, `;`, `<`, `>`, `.`, `/`, `(`, `)`, `[`, `^`
    fn read_identifier(&mut self) -> ClasspathResult<String> {
        let start = self.pos;
        while self.pos < self.input.len() && !is_delimiter(self.input[self.pos]) {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(self.error(format!(
                "expected identifier at position {} but found '{}'",
                self.pos,
                self.peek()
                    .map_or_else(|| "end of input".to_owned(), |b| (b as char).to_string())
            )));
        }
        // Safety: JVM identifiers are valid UTF-8 (Modified UTF-8 subset).
        let ident = std::str::from_utf8(&self.input[start..self.pos])
            .map_err(|e| self.error(format!("invalid UTF-8 in identifier: {e}")))?;
        Ok(ident.to_owned())
    }

    /// Build a `ClasspathError::BytecodeParseError` with the full input as
    /// context.
    fn error(&self, reason: String) -> ClasspathError {
        let input_str = std::str::from_utf8(self.input).unwrap_or("<invalid utf8>");
        ClasspathError::BytecodeParseError {
            class_name: format!("<signature:{input_str}>"),
            reason,
        }
    }

    // -- Grammar productions -------------------------------------------------

    /// ```text
    /// ClassSignature = FormalTypeParameters? SuperclassSignature SuperinterfaceSignature*
    /// ```
    fn parse_class_signature(&mut self) -> ClasspathResult<GenericClassSignature> {
        let type_parameters = if self.peek() == Some(b'<') {
            self.parse_formal_type_parameters()?
        } else {
            Vec::new()
        };

        // SuperclassSignature = ClassTypeSignature
        let superclass = self.parse_class_type_signature()?;

        // SuperinterfaceSignature* = ClassTypeSignature*
        let mut interfaces = Vec::new();
        while self.peek() == Some(b'L') {
            interfaces.push(self.parse_class_type_signature()?);
        }

        Ok(GenericClassSignature {
            type_parameters,
            superclass,
            interfaces,
        })
    }

    /// ```text
    /// MethodSignature = FormalTypeParameters? '(' TypeSignature* ')' ReturnType ThrowsSignature*
    /// ```
    fn parse_method_signature(&mut self) -> ClasspathResult<GenericMethodSignature> {
        let type_parameters = if self.peek() == Some(b'<') {
            self.parse_formal_type_parameters()?
        } else {
            Vec::new()
        };

        self.expect(b'(')?;
        let mut parameter_types = Vec::new();
        while self.peek() != Some(b')') {
            parameter_types.push(self.parse_type_signature()?);
        }
        self.expect(b')')?;

        // ReturnType = TypeSignature | 'V'
        let return_type = self.parse_return_type()?;

        // ThrowsSignature*
        let mut exception_types = Vec::new();
        while self.peek() == Some(b'^') {
            self.advance(); // consume '^'
            exception_types.push(self.parse_throws_target()?);
        }

        Ok(GenericMethodSignature {
            type_parameters,
            parameter_types,
            return_type,
            exception_types,
        })
    }

    /// ```text
    /// FormalTypeParameters = '<' FormalTypeParameter+ '>'
    /// FormalTypeParameter  = Identifier ClassBound InterfaceBound*
    /// ClassBound           = ':' FieldTypeSignature?
    /// InterfaceBound       = ':' FieldTypeSignature
    /// ```
    fn parse_formal_type_parameters(&mut self) -> ClasspathResult<Vec<TypeParameterStub>> {
        self.expect(b'<')?;
        let mut params = Vec::new();
        while self.peek() != Some(b'>') {
            params.push(self.parse_formal_type_parameter()?);
        }
        self.expect(b'>')?;
        if params.is_empty() {
            return Err(self.error("formal type parameter list must not be empty".to_owned()));
        }
        Ok(params)
    }

    fn parse_formal_type_parameter(&mut self) -> ClasspathResult<TypeParameterStub> {
        let name = self.read_identifier()?;

        // ClassBound = ':' FieldTypeSignature?
        self.expect(b':')?;
        let class_bound = if is_field_type_start(self.peek()) {
            Some(self.parse_field_type_signature()?)
        } else {
            // Empty class bound — implicit Object bound.
            None
        };

        // InterfaceBound* = (':' FieldTypeSignature)*
        let mut interface_bounds = Vec::new();
        while self.peek() == Some(b':') {
            self.advance(); // consume ':'
            interface_bounds.push(self.parse_field_type_signature()?);
        }

        Ok(TypeParameterStub {
            name,
            class_bound,
            interface_bounds,
        })
    }

    /// ```text
    /// FieldTypeSignature = ClassTypeSignature | ArrayTypeSignature | TypeVariableSignature
    /// ```
    fn parse_field_type_signature(&mut self) -> ClasspathResult<TypeSignature> {
        match self.peek() {
            Some(b'L') => self.parse_class_type_signature(),
            Some(b'[') => self.parse_array_type_signature(),
            Some(b'T') => self.parse_type_variable_signature(),
            Some(b) => Err(self.error(format!(
                "expected field type signature (L, [, or T) but found '{}' at position {}",
                b as char, self.pos
            ))),
            None => {
                Err(self.error("expected field type signature but reached end of input".to_owned()))
            }
        }
    }

    /// ```text
    /// ClassTypeSignature = 'L' PackageSpecifier* SimpleClassTypeSignature
    ///                      ClassTypeSignatureSuffix* ';'
    /// PackageSpecifier   = Identifier '/'
    /// SimpleClassTypeSignature = Identifier TypeArguments?
    /// ClassTypeSignatureSuffix = '.' SimpleClassTypeSignature
    /// ```
    ///
    /// We read `L`, then accumulate `Identifier/` segments for the package,
    /// then the final `Identifier` (class name), optional type arguments,
    /// then `ClassTypeSignatureSuffix*`, then `;`.
    ///
    /// Inner class suffixes (`.InnerName<Args>`) are appended to the FQN
    /// with `$` separators (matching JVM internal convention) and the
    /// outermost type arguments are replaced by the inner class's arguments.
    fn parse_class_type_signature(&mut self) -> ClasspathResult<TypeSignature> {
        self.expect(b'L')?;

        // Read package/class segments separated by '/'.
        // The last segment before a non-'/' delimiter is the class name.
        let mut segments: Vec<String> = Vec::new();
        loop {
            let ident = self.read_identifier()?;
            segments.push(ident);
            if self.peek() == Some(b'/') {
                self.advance(); // consume '/'
            } else {
                break;
            }
        }

        // Build FQN: replace '/' separators with '.' for the package.
        let fqn = segments.join(".");

        // TypeArguments?
        let mut type_arguments = if self.peek() == Some(b'<') {
            self.parse_type_arguments()?
        } else {
            Vec::new()
        };

        // ClassTypeSignatureSuffix* = ('.' SimpleClassTypeSignature)*
        let mut full_fqn = fqn;
        while self.peek() == Some(b'.') {
            self.advance(); // consume '.'
            let inner_name = self.read_identifier()?;
            full_fqn = format!("{full_fqn}${inner_name}");
            type_arguments = if self.peek() == Some(b'<') {
                self.parse_type_arguments()?
            } else {
                Vec::new()
            };
        }

        self.expect(b';')?;

        Ok(TypeSignature::Class {
            fqn: full_fqn,
            type_arguments,
        })
    }

    /// ```text
    /// TypeArguments = '<' TypeArgument+ '>'
    /// TypeArgument  = WildcardIndicator? FieldTypeSignature | '*'
    /// WildcardIndicator = '+' | '-'
    /// ```
    fn parse_type_arguments(&mut self) -> ClasspathResult<Vec<TypeArgument>> {
        self.expect(b'<')?;
        let mut args = Vec::new();
        while self.peek() != Some(b'>') {
            args.push(self.parse_type_argument()?);
        }
        self.expect(b'>')?;
        if args.is_empty() {
            return Err(self.error("type argument list must not be empty".to_owned()));
        }
        Ok(args)
    }

    fn parse_type_argument(&mut self) -> ClasspathResult<TypeArgument> {
        match self.peek() {
            Some(b'*') => {
                self.advance();
                Ok(TypeArgument::Unbounded)
            }
            Some(b'+') => {
                self.advance();
                let sig = self.parse_field_type_signature()?;
                Ok(TypeArgument::Extends(sig))
            }
            Some(b'-') => {
                self.advance();
                let sig = self.parse_field_type_signature()?;
                Ok(TypeArgument::Super(sig))
            }
            _ => {
                let sig = self.parse_field_type_signature()?;
                Ok(TypeArgument::Type(sig))
            }
        }
    }

    /// ```text
    /// TypeVariableSignature = 'T' Identifier ';'
    /// ```
    fn parse_type_variable_signature(&mut self) -> ClasspathResult<TypeSignature> {
        self.expect(b'T')?;
        let name = self.read_identifier()?;
        self.expect(b';')?;
        Ok(TypeSignature::TypeVariable(name))
    }

    /// ```text
    /// ArrayTypeSignature = '[' TypeSignature
    /// ```
    fn parse_array_type_signature(&mut self) -> ClasspathResult<TypeSignature> {
        self.expect(b'[')?;
        let element = self.parse_type_signature()?;
        Ok(TypeSignature::Array(Box::new(element)))
    }

    /// ```text
    /// TypeSignature = FieldTypeSignature | BaseType
    /// BaseType = 'B' | 'C' | 'D' | 'F' | 'I' | 'J' | 'S' | 'Z'
    /// ```
    fn parse_type_signature(&mut self) -> ClasspathResult<TypeSignature> {
        match self.peek() {
            Some(b'L' | b'[' | b'T') => self.parse_field_type_signature(),
            Some(b) if is_base_type(b) => {
                self.advance();
                Ok(TypeSignature::Base(byte_to_base_type(b)?))
            }
            Some(b) => Err(self.error(format!(
                "expected type signature but found '{}' at position {}",
                b as char, self.pos
            ))),
            None => Err(self.error("expected type signature but reached end of input".to_owned())),
        }
    }

    /// ```text
    /// ReturnType = TypeSignature | 'V'
    /// ```
    fn parse_return_type(&mut self) -> ClasspathResult<TypeSignature> {
        if self.peek() == Some(b'V') {
            self.advance();
            Ok(TypeSignature::Base(BaseType::Void))
        } else {
            self.parse_type_signature()
        }
    }

    /// Parse the target of a throws signature (after the `^` has been consumed).
    ///
    /// ```text
    /// ThrowsSignature = '^' ClassTypeSignature | '^' TypeVariableSignature
    /// ```
    fn parse_throws_target(&mut self) -> ClasspathResult<TypeSignature> {
        match self.peek() {
            Some(b'L') => self.parse_class_type_signature(),
            Some(b'T') => self.parse_type_variable_signature(),
            Some(b) => Err(self.error(format!(
                "expected class or type variable in throws signature but found '{}' at position {}",
                b as char, self.pos
            ))),
            None => Err(self.error("expected throws target but reached end of input".to_owned())),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns `true` if the byte is a delimiter in the signature grammar.
fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b':' | b';' | b'<' | b'>' | b'.' | b'/' | b'(' | b')' | b'[' | b'^'
    )
}

/// Returns `true` if the byte can start a `FieldTypeSignature`.
fn is_field_type_start(b: Option<u8>) -> bool {
    matches!(b, Some(b'L' | b'[' | b'T'))
}

/// Returns `true` if the byte is a JVM `BaseType` descriptor character.
fn is_base_type(b: u8) -> bool {
    matches!(b, b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z')
}

/// Convert a base type descriptor byte to the corresponding `BaseType`.
fn byte_to_base_type(b: u8) -> ClasspathResult<BaseType> {
    match b {
        b'B' => Ok(BaseType::Byte),
        b'C' => Ok(BaseType::Char),
        b'D' => Ok(BaseType::Double),
        b'F' => Ok(BaseType::Float),
        b'I' => Ok(BaseType::Int),
        b'J' => Ok(BaseType::Long),
        b'S' => Ok(BaseType::Short),
        b'Z' => Ok(BaseType::Boolean),
        _ => Err(ClasspathError::BytecodeParseError {
            class_name: "<signature>".to_owned(),
            reason: format!("unknown base type descriptor: '{}'", b as char),
        }),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Test 1: HashMap<K,V> class signature --------------------------------

    #[test]
    fn test_hashmap_class_signature() {
        // <K:Ljava/lang/Object;V:Ljava/lang/Object;>Ljava/util/AbstractMap<TK;TV;>;Ljava/util/Map<TK;TV;>;
        let input = "<K:Ljava/lang/Object;V:Ljava/lang/Object;>Ljava/util/AbstractMap<TK;TV;>;Ljava/util/Map<TK;TV;>;";
        let sig = parse_class_signature(input).unwrap();

        // Type parameters K, V
        assert_eq!(sig.type_parameters.len(), 2);

        let k = &sig.type_parameters[0];
        assert_eq!(k.name, "K");
        match &k.class_bound {
            Some(TypeSignature::Class {
                fqn,
                type_arguments,
            }) => {
                assert_eq!(fqn, "java.lang.Object");
                assert!(type_arguments.is_empty());
            }
            other => panic!("expected Class bound, got {other:?}"),
        }
        assert!(k.interface_bounds.is_empty());

        let v = &sig.type_parameters[1];
        assert_eq!(v.name, "V");
        match &v.class_bound {
            Some(TypeSignature::Class { fqn, .. }) => {
                assert_eq!(fqn, "java.lang.Object");
            }
            other => panic!("expected Class bound, got {other:?}"),
        }

        // Superclass: AbstractMap<K, V>
        match &sig.superclass {
            TypeSignature::Class {
                fqn,
                type_arguments,
            } => {
                assert_eq!(fqn, "java.util.AbstractMap");
                assert_eq!(type_arguments.len(), 2);
                match &type_arguments[0] {
                    TypeArgument::Type(TypeSignature::TypeVariable(name)) => {
                        assert_eq!(name, "K");
                    }
                    other => panic!("expected TypeVariable K, got {other:?}"),
                }
            }
            other => panic!("expected Class superclass, got {other:?}"),
        }

        // Interfaces: Map<K, V>
        assert_eq!(sig.interfaces.len(), 1);
        match &sig.interfaces[0] {
            TypeSignature::Class {
                fqn,
                type_arguments,
            } => {
                assert_eq!(fqn, "java.util.Map");
                assert_eq!(type_arguments.len(), 2);
            }
            other => panic!("expected Class interface, got {other:?}"),
        }
    }

    // -- Test 2: Wildcard with upper bound -----------------------------------

    #[test]
    fn test_wildcard_extends() {
        // List<? extends Number> => Ljava/util/List<+Ljava/lang/Number;>;
        let input = "Ljava/util/List<+Ljava/lang/Number;>;";
        let sig = parse_field_signature(input).unwrap();

        match sig {
            TypeSignature::Class {
                fqn,
                type_arguments,
            } => {
                assert_eq!(fqn, "java.util.List");
                assert_eq!(type_arguments.len(), 1);
                match &type_arguments[0] {
                    TypeArgument::Extends(TypeSignature::Class { fqn, .. }) => {
                        assert_eq!(fqn, "java.lang.Number");
                    }
                    other => panic!("expected Extends(Number), got {other:?}"),
                }
            }
            other => panic!("expected Class, got {other:?}"),
        }
    }

    // -- Test 3: Nested generics — Map<String, List<? super Integer>> --------

    #[test]
    fn test_nested_generics() {
        // Map<String, List<? super Integer>>
        // => Ljava/util/Map<Ljava/lang/String;Ljava/util/List<-Ljava/lang/Integer;>;>;
        let input = "Ljava/util/Map<Ljava/lang/String;Ljava/util/List<-Ljava/lang/Integer;>;>;";
        let sig = parse_field_signature(input).unwrap();

        match sig {
            TypeSignature::Class {
                fqn,
                type_arguments,
            } => {
                assert_eq!(fqn, "java.util.Map");
                assert_eq!(type_arguments.len(), 2);

                // First arg: String
                match &type_arguments[0] {
                    TypeArgument::Type(TypeSignature::Class { fqn, .. }) => {
                        assert_eq!(fqn, "java.lang.String");
                    }
                    other => panic!("expected String, got {other:?}"),
                }

                // Second arg: List<? super Integer>
                match &type_arguments[1] {
                    TypeArgument::Type(TypeSignature::Class {
                        fqn,
                        type_arguments: inner_args,
                    }) => {
                        assert_eq!(fqn, "java.util.List");
                        assert_eq!(inner_args.len(), 1);
                        match &inner_args[0] {
                            TypeArgument::Super(TypeSignature::Class { fqn, .. }) => {
                                assert_eq!(fqn, "java.lang.Integer");
                            }
                            other => panic!("expected Super(Integer), got {other:?}"),
                        }
                    }
                    other => panic!("expected List<? super Integer>, got {other:?}"),
                }
            }
            other => panic!("expected Class, got {other:?}"),
        }
    }

    // -- Test 4: Bounded type parameter with self-reference ------------------

    #[test]
    fn test_bounded_type_parameter_self_reference() {
        // <T:Ljava/lang/Comparable<TT;>;>Ljava/lang/Object;
        let input = "<T:Ljava/lang/Comparable<TT;>;>Ljava/lang/Object;";
        let sig = parse_class_signature(input).unwrap();

        assert_eq!(sig.type_parameters.len(), 1);
        let t = &sig.type_parameters[0];
        assert_eq!(t.name, "T");

        match &t.class_bound {
            Some(TypeSignature::Class {
                fqn,
                type_arguments,
            }) => {
                assert_eq!(fqn, "java.lang.Comparable");
                assert_eq!(type_arguments.len(), 1);
                match &type_arguments[0] {
                    TypeArgument::Type(TypeSignature::TypeVariable(name)) => {
                        assert_eq!(name, "T");
                    }
                    other => panic!("expected TypeVariable(T), got {other:?}"),
                }
            }
            other => panic!("expected Comparable<T> bound, got {other:?}"),
        }
    }

    // -- Test 5: Method signature with type params and return type ------------

    #[test]
    fn test_method_signature_with_type_params() {
        // <T:Ljava/lang/Object;>(TT;Ljava/util/List<TT;>;)TT;
        let input = "<T:Ljava/lang/Object;>(TT;Ljava/util/List<TT;>;)TT;";
        let sig = parse_method_signature(input).unwrap();

        assert_eq!(sig.type_parameters.len(), 1);
        assert_eq!(sig.type_parameters[0].name, "T");

        assert_eq!(sig.parameter_types.len(), 2);
        match &sig.parameter_types[0] {
            TypeSignature::TypeVariable(name) => assert_eq!(name, "T"),
            other => panic!("expected TypeVariable(T), got {other:?}"),
        }
        match &sig.parameter_types[1] {
            TypeSignature::Class {
                fqn,
                type_arguments,
            } => {
                assert_eq!(fqn, "java.util.List");
                assert_eq!(type_arguments.len(), 1);
            }
            other => panic!("expected List<T>, got {other:?}"),
        }

        match &sig.return_type {
            TypeSignature::TypeVariable(name) => assert_eq!(name, "T"),
            other => panic!("expected TypeVariable(T) return, got {other:?}"),
        }

        assert!(sig.exception_types.is_empty());
    }

    // -- Test 6: Array type in signature -------------------------------------

    #[test]
    fn test_array_type() {
        // String[] => [Ljava/lang/String;
        let input = "[Ljava/lang/String;";
        let sig = parse_field_signature(input).unwrap();

        match sig {
            TypeSignature::Array(inner) => match *inner {
                TypeSignature::Class { ref fqn, .. } => {
                    assert_eq!(fqn, "java.lang.String");
                }
                other => panic!("expected Class inside Array, got {other:?}"),
            },
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn test_nested_array_type() {
        // int[][] => [[I
        // This is a TypeSignature, not a FieldTypeSignature, so we test via method sig
        let input = "([[I)V";
        let sig = parse_method_signature(input).unwrap();

        assert_eq!(sig.parameter_types.len(), 1);
        match &sig.parameter_types[0] {
            TypeSignature::Array(inner) => match inner.as_ref() {
                TypeSignature::Array(inner2) => match inner2.as_ref() {
                    TypeSignature::Base(BaseType::Int) => {}
                    other => panic!("expected Base(Int), got {other:?}"),
                },
                other => panic!("expected Array inside Array, got {other:?}"),
            },
            other => panic!("expected Array, got {other:?}"),
        }
    }

    // -- Test 7: Multiple interface bounds ------------------------------------

    #[test]
    fn test_multiple_interface_bounds() {
        // <T:Ljava/lang/Object;:Ljava/io/Serializable;:Ljava/lang/Comparable<TT;>;>Ljava/lang/Object;
        let input = "<T:Ljava/lang/Object;:Ljava/io/Serializable;:Ljava/lang/Comparable<TT;>;>Ljava/lang/Object;";
        let sig = parse_class_signature(input).unwrap();

        assert_eq!(sig.type_parameters.len(), 1);
        let t = &sig.type_parameters[0];
        assert_eq!(t.name, "T");

        // Class bound: Object
        match &t.class_bound {
            Some(TypeSignature::Class { fqn, .. }) => {
                assert_eq!(fqn, "java.lang.Object");
            }
            other => panic!("expected Object class bound, got {other:?}"),
        }

        // Interface bounds: Serializable, Comparable<T>
        assert_eq!(t.interface_bounds.len(), 2);
        match &t.interface_bounds[0] {
            TypeSignature::Class { fqn, .. } => {
                assert_eq!(fqn, "java.io.Serializable");
            }
            other => panic!("expected Serializable, got {other:?}"),
        }
        match &t.interface_bounds[1] {
            TypeSignature::Class {
                fqn,
                type_arguments,
            } => {
                assert_eq!(fqn, "java.lang.Comparable");
                assert_eq!(type_arguments.len(), 1);
            }
            other => panic!("expected Comparable<T>, got {other:?}"),
        }
    }

    // -- Test 8: Inner class signatures (ClassTypeSignatureSuffix) ------------

    #[test]
    fn test_inner_class_signature() {
        // Map.Entry<K, V> => Ljava/util/Map.Entry<TK;TV;>;
        let input = "Ljava/util/Map.Entry<TK;TV;>;";
        let sig = parse_field_signature(input).unwrap();

        match sig {
            TypeSignature::Class {
                fqn,
                type_arguments,
            } => {
                // Inner class FQN uses $ separator
                assert_eq!(fqn, "java.util.Map$Entry");
                assert_eq!(type_arguments.len(), 2);
                match &type_arguments[0] {
                    TypeArgument::Type(TypeSignature::TypeVariable(name)) => {
                        assert_eq!(name, "K");
                    }
                    other => panic!("expected TypeVariable(K), got {other:?}"),
                }
                match &type_arguments[1] {
                    TypeArgument::Type(TypeSignature::TypeVariable(name)) => {
                        assert_eq!(name, "V");
                    }
                    other => panic!("expected TypeVariable(V), got {other:?}"),
                }
            }
            other => panic!("expected Class, got {other:?}"),
        }
    }

    #[test]
    fn test_nested_inner_class() {
        // Outer.Middle.Inner => Ljava/util/Outer.Middle.Inner;
        let input = "Ljava/util/Outer.Middle.Inner;";
        let sig = parse_field_signature(input).unwrap();

        match sig {
            TypeSignature::Class {
                fqn,
                type_arguments,
            } => {
                assert_eq!(fqn, "java.util.Outer$Middle$Inner");
                assert!(type_arguments.is_empty());
            }
            other => panic!("expected Class, got {other:?}"),
        }
    }

    // -- Test 9: Throws signatures in method signature -----------------------

    #[test]
    fn test_method_with_throws() {
        // <T:Ljava/lang/Exception;>(TT;)V^TT;^Ljava/io/IOException;
        let input = "<T:Ljava/lang/Exception;>(TT;)V^TT;^Ljava/io/IOException;";
        let sig = parse_method_signature(input).unwrap();

        assert_eq!(sig.type_parameters.len(), 1);
        assert_eq!(sig.type_parameters[0].name, "T");

        assert_eq!(sig.parameter_types.len(), 1);
        match &sig.parameter_types[0] {
            TypeSignature::TypeVariable(name) => assert_eq!(name, "T"),
            other => panic!("expected TypeVariable(T), got {other:?}"),
        }

        match &sig.return_type {
            TypeSignature::Base(BaseType::Void) => {}
            other => panic!("expected Void return, got {other:?}"),
        }

        assert_eq!(sig.exception_types.len(), 2);
        match &sig.exception_types[0] {
            TypeSignature::TypeVariable(name) => assert_eq!(name, "T"),
            other => panic!("expected TypeVariable(T) exception, got {other:?}"),
        }
        match &sig.exception_types[1] {
            TypeSignature::Class { fqn, .. } => {
                assert_eq!(fqn, "java.io.IOException");
            }
            other => panic!("expected IOException, got {other:?}"),
        }
    }

    // -- Test 10: Malformed signatures return Err ----------------------------

    #[test]
    fn test_malformed_empty() {
        assert!(parse_class_signature("").is_err());
    }

    #[test]
    fn test_malformed_missing_semicolon() {
        // Missing trailing ';'
        assert!(parse_field_signature("Ljava/lang/Object").is_err());
    }

    #[test]
    fn test_malformed_unclosed_type_args() {
        // Missing '>'
        assert!(parse_field_signature("Ljava/util/List<Ljava/lang/String;;").is_err());
    }

    #[test]
    fn test_malformed_missing_class_bound_colon() {
        // Type parameter without ':'
        assert!(parse_class_signature("<T>Ljava/lang/Object;").is_err());
    }

    #[test]
    fn test_malformed_trailing_input() {
        // Valid signature followed by garbage
        assert!(parse_field_signature("Ljava/lang/Object;GARBAGE").is_err());
    }

    #[test]
    fn test_malformed_method_missing_paren() {
        assert!(parse_method_signature("V").is_err());
    }

    // -- Additional edge cases -----------------------------------------------

    #[test]
    fn test_unbounded_wildcard() {
        // List<?> => Ljava/util/List<*>;
        let input = "Ljava/util/List<*>;";
        let sig = parse_field_signature(input).unwrap();

        match sig {
            TypeSignature::Class {
                fqn,
                type_arguments,
            } => {
                assert_eq!(fqn, "java.util.List");
                assert_eq!(type_arguments.len(), 1);
                assert!(matches!(type_arguments[0], TypeArgument::Unbounded));
            }
            other => panic!("expected Class, got {other:?}"),
        }
    }

    #[test]
    fn test_method_void_return() {
        // ()V — no params, void return
        let input = "()V";
        let sig = parse_method_signature(input).unwrap();

        assert!(sig.type_parameters.is_empty());
        assert!(sig.parameter_types.is_empty());
        assert!(matches!(
            sig.return_type,
            TypeSignature::Base(BaseType::Void)
        ));
        assert!(sig.exception_types.is_empty());
    }

    #[test]
    fn test_method_primitive_params() {
        // (IDZ)J — int, double, boolean -> long
        let input = "(IDZ)J";
        let sig = parse_method_signature(input).unwrap();

        assert_eq!(sig.parameter_types.len(), 3);
        assert!(matches!(
            sig.parameter_types[0],
            TypeSignature::Base(BaseType::Int)
        ));
        assert!(matches!(
            sig.parameter_types[1],
            TypeSignature::Base(BaseType::Double)
        ));
        assert!(matches!(
            sig.parameter_types[2],
            TypeSignature::Base(BaseType::Boolean)
        ));
        assert!(matches!(
            sig.return_type,
            TypeSignature::Base(BaseType::Long)
        ));
    }

    #[test]
    fn test_empty_class_bound_with_interface_bound() {
        // <T::Ljava/io/Serializable;>Ljava/lang/Object;
        // Empty class bound (implicit Object), one interface bound
        let input = "<T::Ljava/io/Serializable;>Ljava/lang/Object;";
        let sig = parse_class_signature(input).unwrap();

        assert_eq!(sig.type_parameters.len(), 1);
        let t = &sig.type_parameters[0];
        assert_eq!(t.name, "T");
        assert!(t.class_bound.is_none());
        assert_eq!(t.interface_bounds.len(), 1);
        match &t.interface_bounds[0] {
            TypeSignature::Class { fqn, .. } => {
                assert_eq!(fqn, "java.io.Serializable");
            }
            other => panic!("expected Serializable, got {other:?}"),
        }
    }

    #[test]
    fn test_type_variable_field_signature() {
        // TT; — bare type variable reference
        let input = "TT;";
        let sig = parse_field_signature(input).unwrap();

        match sig {
            TypeSignature::TypeVariable(name) => assert_eq!(name, "T"),
            other => panic!("expected TypeVariable, got {other:?}"),
        }
    }

    #[test]
    fn test_complex_real_world_class_signature() {
        // AbstractMap<K,V> — class that implements multiple parameterized interfaces
        // Signature: <K:Ljava/lang/Object;V:Ljava/lang/Object;>Ljava/lang/Object;Ljava/util/Map<TK;TV;>;
        let input =
            "<K:Ljava/lang/Object;V:Ljava/lang/Object;>Ljava/lang/Object;Ljava/util/Map<TK;TV;>;";
        let sig = parse_class_signature(input).unwrap();

        assert_eq!(sig.type_parameters.len(), 2);
        match &sig.superclass {
            TypeSignature::Class {
                fqn,
                type_arguments,
            } => {
                assert_eq!(fqn, "java.lang.Object");
                assert!(type_arguments.is_empty());
            }
            other => panic!("expected Object superclass, got {other:?}"),
        }
        assert_eq!(sig.interfaces.len(), 1);
    }

    #[test]
    fn test_array_of_generic_type() {
        // List<String>[] in method param => ([Ljava/util/List<Ljava/lang/String;>;)V
        let input = "([Ljava/util/List<Ljava/lang/String;>;)V";
        let sig = parse_method_signature(input).unwrap();

        assert_eq!(sig.parameter_types.len(), 1);
        match &sig.parameter_types[0] {
            TypeSignature::Array(inner) => match inner.as_ref() {
                TypeSignature::Class {
                    fqn,
                    type_arguments,
                } => {
                    assert_eq!(fqn, "java.util.List");
                    assert_eq!(type_arguments.len(), 1);
                }
                other => panic!("expected List<String> inside array, got {other:?}"),
            },
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn test_wildcard_super_bound() {
        // Comparable<? super T> => Ljava/lang/Comparable<-TT;>;
        let input = "Ljava/lang/Comparable<-TT;>;";
        let sig = parse_field_signature(input).unwrap();

        match sig {
            TypeSignature::Class {
                fqn,
                type_arguments,
            } => {
                assert_eq!(fqn, "java.lang.Comparable");
                assert_eq!(type_arguments.len(), 1);
                match &type_arguments[0] {
                    TypeArgument::Super(TypeSignature::TypeVariable(name)) => {
                        assert_eq!(name, "T");
                    }
                    other => panic!("expected Super(T), got {other:?}"),
                }
            }
            other => panic!("expected Class, got {other:?}"),
        }
    }

    #[test]
    fn test_inner_class_with_outer_type_args() {
        // Outer<String>.Inner => Ljava/util/Outer<Ljava/lang/String;>.Inner;
        // The outer type args are present but inner has none, so final type_arguments is empty
        let input = "Ljava/util/Outer<Ljava/lang/String;>.Inner;";
        let sig = parse_field_signature(input).unwrap();

        match sig {
            TypeSignature::Class {
                fqn,
                type_arguments,
            } => {
                assert_eq!(fqn, "java.util.Outer$Inner");
                // Inner class has no type args of its own, so empty
                assert!(type_arguments.is_empty());
            }
            other => panic!("expected Class, got {other:?}"),
        }
    }

    #[test]
    fn test_multiple_type_params_method() {
        // <K:Ljava/lang/Object;V:Ljava/lang/Object;>(TK;TV;)V
        let input = "<K:Ljava/lang/Object;V:Ljava/lang/Object;>(TK;TV;)V";
        let sig = parse_method_signature(input).unwrap();

        assert_eq!(sig.type_parameters.len(), 2);
        assert_eq!(sig.type_parameters[0].name, "K");
        assert_eq!(sig.type_parameters[1].name, "V");
        assert_eq!(sig.parameter_types.len(), 2);
        assert!(matches!(
            sig.return_type,
            TypeSignature::Base(BaseType::Void)
        ));
    }
}
