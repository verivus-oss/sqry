//! Stub data model for parsed bytecode.
//!
//! Defines the intermediate representation produced by bytecode parsing:
//! `ClassStub`, `MethodStub`, `FieldStub`, `AnnotationStub`, and associated types.
//! These stubs are cached per-JAR and merged into the `ClasspathIndex`.

pub mod cache;
pub mod index;
pub mod model;
