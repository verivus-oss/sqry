//! Graph integration for classpath data.
//!
//! Emits synthetic graph nodes for classpath classes, methods, and fields,
//! and creates inheritance/generic/annotation edges.

pub mod emitter;
pub mod provenance;
