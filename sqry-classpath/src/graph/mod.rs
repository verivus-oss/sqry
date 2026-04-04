//! Graph integration for classpath data.
//!
//! Emits synthetic graph nodes for classpath classes, methods, and fields,
//! registers them in the `ExportMap` for cross-file resolution, and creates
//! inheritance/generic/annotation edges.

pub mod emitter;
pub mod provenance;
