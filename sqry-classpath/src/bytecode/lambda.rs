//! Lambda/method-reference target extraction from the `BootstrapMethods`
//! attribute.
//!
//! JVM compilers (javac, kotlinc, scalac, etc.) compile lambda expressions and
//! method references into `invokedynamic` instructions whose bootstrap method
//! is [`java/lang/invoke/LambdaMetafactory.metafactory`][metafactory] (or
//! `altMetafactory`). The third bootstrap argument (index 2) of such entries
//! is a `CONSTANT_MethodHandle_info` that points to the **actual target
//! method** being captured.
//!
//! This module extracts those targets from a parsed [`cafebabe::ClassFile`] and
//! returns them as [`LambdaTargetStub`] records for inclusion in the class
//! stub.
//!
//! [metafactory]: https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/lang/invoke/LambdaMetafactory.html

use cafebabe::attributes::AttributeData;
use cafebabe::constant_pool::{
    BootstrapArgument, MethodHandle, ReferenceKind as CafeReferenceKind,
};

use crate::stub::model::{LambdaTargetStub, ReferenceKind};

use super::constants::class_name_to_fqn;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The internal class name of `LambdaMetafactory`.
const LAMBDA_METAFACTORY_CLASS: &str = "java/lang/invoke/LambdaMetafactory";

/// The standard bootstrap method name for lambda expressions.
const METAFACTORY_METHOD: &str = "metafactory";

/// The alternative bootstrap method name for complex lambda expressions
/// (serialisable lambdas, intersection-type target, etc.).
const ALT_METAFACTORY_METHOD: &str = "altMetafactory";

/// Index of the implementation method handle within the bootstrap arguments.
/// For `LambdaMetafactory.metafactory`, the arguments are:
///   0 — samMethodType  (MethodType)
///   1 — implMethod     (MethodHandle) — **sometimes**
///   2 — implMethod     (MethodHandle) — **standard position**
///
/// Per the JVM spec and `LambdaMetafactory` javadoc, argument index 2 is the
/// implementation `MethodHandle`.
const IMPL_METHOD_ARG_INDEX: usize = 2;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Extract lambda/method-reference targets from a parsed class file.
///
/// Iterates class-level attributes, finds the `BootstrapMethods` attribute, and
/// filters entries whose bootstrap method handle points to
/// `LambdaMetafactory.metafactory` or `altMetafactory`. For each matching
/// entry, the third bootstrap argument (a `MethodHandle`) is converted to a
/// [`LambdaTargetStub`].
///
/// # Returns
///
/// A `Vec` of targets, potentially empty if no `BootstrapMethods` attribute
/// exists or none of the entries are `LambdaMetafactory` invocations.
///
/// Non-`LambdaMetafactory` bootstrap entries are silently skipped. Malformed
/// entries (e.g., fewer than 3 arguments, wrong argument type at index 2) are
/// logged as warnings and skipped.
pub fn extract_lambda_targets(class: &cafebabe::ClassFile<'_>) -> Vec<LambdaTargetStub> {
    let mut targets = Vec::new();

    for attr in &class.attributes {
        if let AttributeData::BootstrapMethods(entries) = &attr.data {
            for (idx, entry) in entries.iter().enumerate() {
                // Check if the bootstrap method points to LambdaMetafactory.
                if !is_lambda_metafactory(&entry.method) {
                    continue;
                }

                // Extract the implementation MethodHandle from argument index 2.
                match extract_impl_handle(idx, &entry.arguments) {
                    Some(stub) => targets.push(stub),
                    None => continue,
                }
            }
        }
    }

    targets
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Check whether a bootstrap method handle points to `LambdaMetafactory`.
///
/// The handle's class name must be `java/lang/invoke/LambdaMetafactory` and its
/// method name must be `metafactory` or `altMetafactory`.
fn is_lambda_metafactory(handle: &MethodHandle<'_>) -> bool {
    handle.class_name.as_ref() == LAMBDA_METAFACTORY_CLASS
        && (handle.member_ref.name.as_ref() == METAFACTORY_METHOD
            || handle.member_ref.name.as_ref() == ALT_METAFACTORY_METHOD)
}

/// Extract the implementation `MethodHandle` from bootstrap arguments and
/// convert it to a [`LambdaTargetStub`].
///
/// Returns `None` (with a warning log) if the arguments are too few or the
/// third argument is not a `MethodHandle`.
fn extract_impl_handle(
    bootstrap_idx: usize,
    arguments: &[BootstrapArgument<'_>],
) -> Option<LambdaTargetStub> {
    if arguments.len() <= IMPL_METHOD_ARG_INDEX {
        log::warn!(
            "BootstrapMethods entry {bootstrap_idx}: expected at least {} arguments, \
             found {}; skipping",
            IMPL_METHOD_ARG_INDEX + 1,
            arguments.len(),
        );
        return None;
    }

    match &arguments[IMPL_METHOD_ARG_INDEX] {
        BootstrapArgument::MethodHandle(handle) => {
            let reference_kind = match convert_reference_kind(handle.kind) {
                Some(kind) => kind,
                None => {
                    log::warn!(
                        "BootstrapMethods entry {bootstrap_idx}: \
                         unsupported reference kind {:?}; skipping",
                        handle.kind,
                    );
                    return None;
                }
            };

            Some(LambdaTargetStub {
                owner_fqn: class_name_to_fqn(handle.class_name.as_ref()),
                method_name: handle.member_ref.name.to_string(),
                method_descriptor: handle.member_ref.descriptor.to_string(),
                reference_kind,
            })
        }
        other => {
            log::warn!(
                "BootstrapMethods entry {bootstrap_idx}: expected MethodHandle at \
                 argument index {IMPL_METHOD_ARG_INDEX}, found {kind}; skipping",
                kind = bootstrap_arg_kind_name(other),
            );
            None
        }
    }
}

/// Convert a cafebabe [`CafeReferenceKind`] to our model [`ReferenceKind`].
fn convert_reference_kind(kind: CafeReferenceKind) -> Option<ReferenceKind> {
    Some(match kind {
        CafeReferenceKind::GetField => ReferenceKind::GetField,
        CafeReferenceKind::GetStatic => ReferenceKind::GetStatic,
        CafeReferenceKind::PutField => ReferenceKind::PutField,
        CafeReferenceKind::PutStatic => ReferenceKind::PutStatic,
        CafeReferenceKind::InvokeVirtual => ReferenceKind::InvokeVirtual,
        CafeReferenceKind::InvokeStatic => ReferenceKind::InvokeStatic,
        CafeReferenceKind::InvokeSpecial => ReferenceKind::InvokeSpecial,
        CafeReferenceKind::NewInvokeSpecial => ReferenceKind::NewInvokeSpecial,
        CafeReferenceKind::InvokeInterface => ReferenceKind::InvokeInterface,
    })
}

/// Return a human-readable name for a [`BootstrapArgument`] variant (for log
/// messages).
fn bootstrap_arg_kind_name(arg: &BootstrapArgument<'_>) -> &'static str {
    match arg {
        BootstrapArgument::LiteralConstant(_) => "LiteralConstant",
        BootstrapArgument::ClassInfo(_) => "ClassInfo",
        BootstrapArgument::MethodHandle(_) => "MethodHandle",
        BootstrapArgument::MethodType(_) => "MethodType",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cafebabe::attributes::{AttributeData, AttributeInfo, BootstrapMethodEntry};
    use cafebabe::constant_pool::{
        BootstrapArgument, MemberKind, MethodHandle, NameAndType,
        ReferenceKind as CafeReferenceKind,
    };
    use std::borrow::Cow;

    // -- Test helpers ---------------------------------------------------------

    /// Build a `MethodHandle` pointing to `LambdaMetafactory.metafactory`.
    fn metafactory_handle<'a>() -> MethodHandle<'a> {
        MethodHandle {
            kind: CafeReferenceKind::InvokeStatic,
            class_name: Cow::Borrowed(LAMBDA_METAFACTORY_CLASS),
            member_kind: MemberKind::Method,
            member_ref: NameAndType {
                name: Cow::Borrowed(METAFACTORY_METHOD),
                descriptor: Cow::Borrowed(
                    "(Ljava/lang/invoke/MethodHandles$Lookup;\
                     Ljava/lang/String;\
                     Ljava/lang/invoke/MethodType;\
                     Ljava/lang/invoke/MethodType;\
                     Ljava/lang/invoke/MethodHandle;\
                     Ljava/lang/invoke/MethodType;\
                     )Ljava/lang/invoke/CallSite;",
                ),
            },
        }
    }

    /// Build a `MethodHandle` pointing to `LambdaMetafactory.altMetafactory`.
    fn alt_metafactory_handle<'a>() -> MethodHandle<'a> {
        MethodHandle {
            kind: CafeReferenceKind::InvokeStatic,
            class_name: Cow::Borrowed(LAMBDA_METAFACTORY_CLASS),
            member_kind: MemberKind::Method,
            member_ref: NameAndType {
                name: Cow::Borrowed(ALT_METAFACTORY_METHOD),
                descriptor: Cow::Borrowed(
                    "(Ljava/lang/invoke/MethodHandles$Lookup;\
                     Ljava/lang/String;\
                     Ljava/lang/invoke/MethodType;\
                     [Ljava/lang/Object;\
                     )Ljava/lang/invoke/CallSite;",
                ),
            },
        }
    }

    /// Build a non-lambda bootstrap handle (e.g., `StringConcatFactory`).
    fn string_concat_handle<'a>() -> MethodHandle<'a> {
        MethodHandle {
            kind: CafeReferenceKind::InvokeStatic,
            class_name: Cow::Borrowed("java/lang/invoke/StringConcatFactory"),
            member_kind: MemberKind::Method,
            member_ref: NameAndType {
                name: Cow::Borrowed("makeConcatWithConstants"),
                descriptor: Cow::Borrowed(
                    "(Ljava/lang/invoke/MethodHandles$Lookup;\
                     Ljava/lang/String;\
                     Ljava/lang/invoke/MethodType;\
                     Ljava/lang/String;\
                     [Ljava/lang/Object;\
                     )Ljava/lang/invoke/CallSite;",
                ),
            },
        }
    }

    /// Build the standard 3-argument list for a `LambdaMetafactory` entry.
    ///
    /// Arguments: [MethodType(sam_descriptor), MethodType(instantiated),
    /// MethodHandle(impl)].
    fn lambda_bootstrap_args<'a>(
        impl_kind: CafeReferenceKind,
        impl_class: &'a str,
        impl_name: &'a str,
        impl_descriptor: &'a str,
    ) -> Vec<BootstrapArgument<'a>> {
        vec![
            // arg 0: SAM method type
            BootstrapArgument::MethodType(Cow::Borrowed("(Ljava/lang/Object;)Ljava/lang/Object;")),
            // arg 1: instantiated method type
            BootstrapArgument::MethodType(Cow::Borrowed("(Ljava/lang/String;)Ljava/lang/String;")),
            // arg 2: implementation method handle
            BootstrapArgument::MethodHandle(MethodHandle {
                kind: impl_kind,
                class_name: Cow::Borrowed(impl_class),
                member_kind: MemberKind::Method,
                member_ref: NameAndType {
                    name: Cow::Borrowed(impl_name),
                    descriptor: Cow::Borrowed(impl_descriptor),
                },
            }),
        ]
    }

    /// Parse a real class file and extract lambda targets. This requires
    /// building a `ClassFile` from bytes, which is the full integration path.
    /// These unit tests use the component function directly with constructed
    /// bootstrap entries instead.

    // -- Test 1: No BootstrapMethods attribute → empty result -----------------

    #[test]
    fn no_bootstrap_methods_returns_empty() {
        // Simulate a class with no BootstrapMethods attribute by calling the
        // extraction logic directly on empty attribute lists.
        let attrs: Vec<AttributeInfo<'_>> = vec![];
        let targets = extract_lambda_targets_from_attrs(&attrs);
        assert!(targets.is_empty(), "Expected empty targets");
    }

    // -- Test 2: Lambda target from stream().map(String::toUpperCase) ---------

    #[test]
    fn lambda_target_from_method_reference() {
        let entries = vec![BootstrapMethodEntry {
            method: metafactory_handle(),
            arguments: lambda_bootstrap_args(
                CafeReferenceKind::InvokeVirtual,
                "java/lang/String",
                "toUpperCase",
                "()Ljava/lang/String;",
            ),
        }];

        let attrs = vec![AttributeInfo {
            name: Cow::Borrowed("BootstrapMethods"),
            data: AttributeData::BootstrapMethods(entries),
        }];

        let targets = extract_lambda_targets_from_attrs(&attrs);

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].owner_fqn, "java.lang.String");
        assert_eq!(targets[0].method_name, "toUpperCase");
        assert_eq!(targets[0].method_descriptor, "()Ljava/lang/String;");
        assert_eq!(targets[0].reference_kind, ReferenceKind::InvokeVirtual);
    }

    // -- Test 3: Method reference target correctly identified -----------------

    #[test]
    fn method_reference_with_invoke_static() {
        let entries = vec![BootstrapMethodEntry {
            method: metafactory_handle(),
            arguments: lambda_bootstrap_args(
                CafeReferenceKind::InvokeStatic,
                "java/lang/Integer",
                "parseInt",
                "(Ljava/lang/String;)I",
            ),
        }];

        let attrs = vec![AttributeInfo {
            name: Cow::Borrowed("BootstrapMethods"),
            data: AttributeData::BootstrapMethods(entries),
        }];

        let targets = extract_lambda_targets_from_attrs(&attrs);

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].owner_fqn, "java.lang.Integer");
        assert_eq!(targets[0].method_name, "parseInt");
        assert_eq!(targets[0].method_descriptor, "(Ljava/lang/String;)I");
        assert_eq!(targets[0].reference_kind, ReferenceKind::InvokeStatic);
    }

    // -- Test 4: Non-LambdaMetafactory bootstrap entries skipped --------------

    #[test]
    fn non_lambda_metafactory_skipped() {
        let entries = vec![BootstrapMethodEntry {
            method: string_concat_handle(),
            arguments: vec![BootstrapArgument::LiteralConstant(
                cafebabe::constant_pool::LiteralConstant::String(Cow::Borrowed("\u{1}Hello \u{1}")),
            )],
        }];

        let attrs = vec![AttributeInfo {
            name: Cow::Borrowed("BootstrapMethods"),
            data: AttributeData::BootstrapMethods(entries),
        }];

        let targets = extract_lambda_targets_from_attrs(&attrs);
        assert!(
            targets.is_empty(),
            "Non-LambdaMetafactory should be skipped"
        );
    }

    // -- Test 5: Multiple lambda targets in one class -------------------------

    #[test]
    fn multiple_lambda_targets() {
        let entries = vec![
            // Entry 0: String::toUpperCase method reference
            BootstrapMethodEntry {
                method: metafactory_handle(),
                arguments: lambda_bootstrap_args(
                    CafeReferenceKind::InvokeVirtual,
                    "java/lang/String",
                    "toUpperCase",
                    "()Ljava/lang/String;",
                ),
            },
            // Entry 1: StringConcatFactory (not lambda — should be skipped)
            BootstrapMethodEntry {
                method: string_concat_handle(),
                arguments: vec![],
            },
            // Entry 2: Constructor reference (NewInvokeSpecial)
            BootstrapMethodEntry {
                method: metafactory_handle(),
                arguments: lambda_bootstrap_args(
                    CafeReferenceKind::NewInvokeSpecial,
                    "java/util/ArrayList",
                    "<init>",
                    "()V",
                ),
            },
            // Entry 3: altMetafactory — serialisable lambda
            BootstrapMethodEntry {
                method: alt_metafactory_handle(),
                arguments: lambda_bootstrap_args(
                    CafeReferenceKind::InvokeStatic,
                    "com/example/Service",
                    "lambda$process$0",
                    "(Ljava/lang/Object;)V",
                ),
            },
        ];

        let attrs = vec![AttributeInfo {
            name: Cow::Borrowed("BootstrapMethods"),
            data: AttributeData::BootstrapMethods(entries),
        }];

        let targets = extract_lambda_targets_from_attrs(&attrs);

        // 3 lambda entries (indices 0, 2, 3); index 1 is StringConcatFactory.
        assert_eq!(
            targets.len(),
            3,
            "Expected 3 lambda targets, got {}",
            targets.len()
        );

        assert_eq!(targets[0].owner_fqn, "java.lang.String");
        assert_eq!(targets[0].method_name, "toUpperCase");
        assert_eq!(targets[0].reference_kind, ReferenceKind::InvokeVirtual);

        assert_eq!(targets[1].owner_fqn, "java.util.ArrayList");
        assert_eq!(targets[1].method_name, "<init>");
        assert_eq!(targets[1].reference_kind, ReferenceKind::NewInvokeSpecial);

        assert_eq!(targets[2].owner_fqn, "com.example.Service");
        assert_eq!(targets[2].method_name, "lambda$process$0");
        assert_eq!(targets[2].reference_kind, ReferenceKind::InvokeStatic);
    }

    // -- Test 6: Reference kind correctly mapped for all variants -------------

    #[test]
    fn reference_kind_mapping_exhaustive() {
        let cafe_to_model = [
            (CafeReferenceKind::GetField, ReferenceKind::GetField),
            (CafeReferenceKind::GetStatic, ReferenceKind::GetStatic),
            (CafeReferenceKind::PutField, ReferenceKind::PutField),
            (CafeReferenceKind::PutStatic, ReferenceKind::PutStatic),
            (
                CafeReferenceKind::InvokeVirtual,
                ReferenceKind::InvokeVirtual,
            ),
            (CafeReferenceKind::InvokeStatic, ReferenceKind::InvokeStatic),
            (
                CafeReferenceKind::InvokeSpecial,
                ReferenceKind::InvokeSpecial,
            ),
            (
                CafeReferenceKind::NewInvokeSpecial,
                ReferenceKind::NewInvokeSpecial,
            ),
            (
                CafeReferenceKind::InvokeInterface,
                ReferenceKind::InvokeInterface,
            ),
        ];

        for (cafe_kind, expected_model_kind) in &cafe_to_model {
            let result = convert_reference_kind(*cafe_kind);
            assert_eq!(
                result,
                Some(*expected_model_kind),
                "Mapping failed for {cafe_kind:?}"
            );
        }
    }

    // -- Test 7: Malformed entry — too few arguments --------------------------

    #[test]
    fn too_few_arguments_skipped_with_warning() {
        // Only 2 arguments instead of the required 3.
        let entries = vec![BootstrapMethodEntry {
            method: metafactory_handle(),
            arguments: vec![
                BootstrapArgument::MethodType(Cow::Borrowed(
                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                )),
                BootstrapArgument::MethodType(Cow::Borrowed(
                    "(Ljava/lang/String;)Ljava/lang/String;",
                )),
            ],
        }];

        let attrs = vec![AttributeInfo {
            name: Cow::Borrowed("BootstrapMethods"),
            data: AttributeData::BootstrapMethods(entries),
        }];

        let targets = extract_lambda_targets_from_attrs(&attrs);
        assert!(targets.is_empty(), "Malformed entry should be skipped");
    }

    // -- Test 8: Wrong argument type at index 2 -------------------------------

    #[test]
    fn wrong_argument_type_at_index_2_skipped() {
        // Argument index 2 is a MethodType instead of a MethodHandle.
        let entries = vec![BootstrapMethodEntry {
            method: metafactory_handle(),
            arguments: vec![
                BootstrapArgument::MethodType(Cow::Borrowed(
                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                )),
                BootstrapArgument::MethodType(Cow::Borrowed(
                    "(Ljava/lang/String;)Ljava/lang/String;",
                )),
                BootstrapArgument::MethodType(Cow::Borrowed("()V")),
            ],
        }];

        let attrs = vec![AttributeInfo {
            name: Cow::Borrowed("BootstrapMethods"),
            data: AttributeData::BootstrapMethods(entries),
        }];

        let targets = extract_lambda_targets_from_attrs(&attrs);
        assert!(
            targets.is_empty(),
            "Wrong type at index 2 should be skipped"
        );
    }

    // -- Test 9: Interface method reference -----------------------------------

    #[test]
    fn interface_method_reference() {
        let entries = vec![BootstrapMethodEntry {
            method: metafactory_handle(),
            arguments: lambda_bootstrap_args(
                CafeReferenceKind::InvokeInterface,
                "java/util/List",
                "size",
                "()I",
            ),
        }];

        let attrs = vec![AttributeInfo {
            name: Cow::Borrowed("BootstrapMethods"),
            data: AttributeData::BootstrapMethods(entries),
        }];

        let targets = extract_lambda_targets_from_attrs(&attrs);

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].owner_fqn, "java.util.List");
        assert_eq!(targets[0].method_name, "size");
        assert_eq!(targets[0].reference_kind, ReferenceKind::InvokeInterface);
    }

    // -- Test 10: FQN conversion from internal format -------------------------

    #[test]
    fn fqn_conversion_internal_to_dotted() {
        let entries = vec![BootstrapMethodEntry {
            method: metafactory_handle(),
            arguments: lambda_bootstrap_args(
                CafeReferenceKind::InvokeStatic,
                "com/example/deeply/nested/ServiceImpl",
                "handle",
                "(Ljava/lang/Object;)V",
            ),
        }];

        let attrs = vec![AttributeInfo {
            name: Cow::Borrowed("BootstrapMethods"),
            data: AttributeData::BootstrapMethods(entries),
        }];

        let targets = extract_lambda_targets_from_attrs(&attrs);

        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0].owner_fqn,
            "com.example.deeply.nested.ServiceImpl"
        );
    }

    // -- Test helper: extract from attributes without a full ClassFile ---------

    /// Helper that mirrors `extract_lambda_targets` but operates on a bare
    /// attribute slice so tests don't need to construct a full `ClassFile`.
    fn extract_lambda_targets_from_attrs(attrs: &[AttributeInfo<'_>]) -> Vec<LambdaTargetStub> {
        let mut targets = Vec::new();
        for attr in attrs {
            if let AttributeData::BootstrapMethods(entries) = &attr.data {
                for (idx, entry) in entries.iter().enumerate() {
                    if !is_lambda_metafactory(&entry.method) {
                        continue;
                    }
                    if let Some(stub) = extract_impl_handle(idx, &entry.arguments) {
                        targets.push(stub);
                    }
                }
            }
        }
        targets
    }
}
