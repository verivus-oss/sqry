//! Scala signature extraction from `@ScalaSignature` annotations.
//!
//! Reads the binary-encoded Scala 2 signature format to recover Scala-specific
//! information: trait vs object vs case class, visibility, sealed/abstract
//! modifiers, and basic superclass/trait hierarchy.
//!
//! # Architecture
//!
//! The `@ScalaSignature` annotation embeds a binary entry table (the "Scala 2
//! signature format", version 5.x) containing symbol definitions with flags
//! and type references. This module provides:
//!
//! - [`signature`] — Low-level binary format reader that parses the entry table
//!   and provides accessors for individual entries, names, and symbol info.
//! - [`decoder`] — High-level decoder that produces [`ScalaClassMetadata`] from
//!   a [`ScalaSignatureStub`](crate::stub::model::ScalaSignatureStub).
//!
//! # Supported features (Tier 1)
//!
//! - Class kind detection: class, trait, object, package object
//! - Visibility: public, private, protected
//! - Modifiers: case, sealed, abstract
//! - Basic hierarchy: superclass and mixed-in trait names
//!
//! # Graceful degradation
//!
//! When the signature format is unsupported (e.g., Scala 3 TASTy) or the data
//! is malformed, [`decode_scala_signature`] returns `None` and the caller falls
//! back to bytecode-only analysis.

mod decoder;
pub mod signature;

pub use decoder::{ScalaClassKind, ScalaClassMetadata, ScalaVisibility, decode_scala_signature};
