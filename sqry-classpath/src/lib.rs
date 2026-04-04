//! JVM classpath analysis for sqry.
//!
//! This crate provides bytecode parsing, build system resolution, and graph integration
//! for JVM classpath dependencies (Java, Kotlin, Scala). It enables sqry to resolve
//! imports and type references across workspace source code and compiled library JARs.
//!
//! # Architecture
//!
//! The classpath pipeline runs as a pre-pass before the main build graph pipeline:
//!
//! 1. **Detect** - Identify the build system (Gradle, Maven, Bazel, sbt)
//! 2. **Resolve** - Extract the classpath JAR list via build tool subprocess
//! 3. **Scan** - Parse `.class` bytecode from each JAR into `ClassStub` records
//! 4. **Cache** - Persist parsed stubs per JAR (SHA-256 keyed) for incremental rebuilds
//! 5. **Index** - Merge all stubs into a `ClasspathIndex` for fast FQN lookup
//! 6. **Emit** - Create synthetic graph nodes and register in `ExportMap`
//!
//! # Feature Gate
//!
//! All classpath functionality is gated behind the `jvm-classpath` feature on `sqry-core`.
//! When disabled, no classpath code is compiled and there is zero overhead.

pub mod bytecode;
pub mod detect;
pub mod graph;
pub mod kotlin;
pub mod pipeline;
pub mod resolve;
pub mod scala;
pub mod stub;

use thiserror::Error;

/// Errors that can occur during classpath analysis.
#[derive(Debug, Error)]
pub enum ClasspathError {
    /// Build system detection failed.
    #[error("build system detection failed: {0}")]
    DetectionFailed(String),

    /// Classpath resolution failed (build tool subprocess error).
    #[error("classpath resolution failed: {0}")]
    ResolutionFailed(String),

    /// Bytecode parsing error for a specific class.
    #[error("bytecode parse error in {class_name}: {reason}")]
    BytecodeParseError { class_name: String, reason: String },

    /// JAR file could not be read.
    #[error("JAR read error for {path}: {reason}")]
    JarReadError { path: String, reason: String },

    /// Cache I/O error.
    #[error("cache error: {0}")]
    CacheError(String),

    /// Index persistence error.
    #[error("index error: {0}")]
    IndexError(String),

    /// Graph emission error.
    #[error("graph emission error: {0}")]
    EmissionError(String),

    /// Generic I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result type for classpath operations.
pub type ClasspathResult<T> = Result<T, ClasspathError>;
