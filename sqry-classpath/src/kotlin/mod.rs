//! Kotlin metadata extraction from `@kotlin.Metadata` annotations.
//!
//! Reads the protobuf-encoded metadata to recover Kotlin-specific information
//! that is lost in bytecode compilation: extension receivers, nullable types,
//! companion objects, data classes, sealed hierarchies.
//!
//! # Architecture
//!
//! The `@kotlin.Metadata` annotation embeds a protobuf-encoded blob (`d1`)
//! alongside a string table (`d2`). This module provides a zero-dependency
//! protobuf wire-format reader that extracts the specific field numbers used
//! by `kotlin.metadata.jvm.proto` without requiring a full protobuf runtime.
//!
//! # Supported metadata kinds
//!
//! Currently only kind=1 (Class) is decoded. Other kinds (file facade,
//! synthetic, multi-file class) return `None`, falling back to bytecode-only
//! analysis.

mod decoder;

pub use decoder::{
    KotlinClassKind, KotlinClassMetadata, KotlinExtensionFunction, KotlinVisibility,
    decode_kotlin_metadata,
};
