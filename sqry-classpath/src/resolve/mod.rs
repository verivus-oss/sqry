//! Classpath resolution via build tool subprocesses.
//!
//! Each resolver extracts the list of dependency JARs from the project's build system.
//! Resolvers support timeout, caching, and graceful fallback.

pub mod bazel;
pub mod gradle;
pub mod maven;
pub mod sbt;
pub mod source_jars;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A resolved classpath entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClasspathEntry {
    /// Path to the JAR file.
    pub jar_path: PathBuf,
    /// Maven coordinates if known (e.g., `com.google.guava:guava:33.0.0`).
    pub coordinates: Option<String>,
    /// Whether this is a direct (compile) dependency vs transitive.
    pub is_direct: bool,
    /// Source JAR path if available.
    pub source_jar: Option<PathBuf>,
}

/// Result of classpath resolution for a project or module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedClasspath {
    /// Module name (e.g., `app`, `lib`, or project root name).
    pub module_name: String,
    /// Concrete module root used to scope importer -> classpath resolution.
    pub module_root: PathBuf,
    /// Classpath entries.
    pub entries: Vec<ClasspathEntry>,
}

/// Classpath resolution configuration.
pub struct ResolveConfig {
    /// Project root directory.
    pub project_root: PathBuf,
    /// Timeout for subprocess execution in seconds.
    pub timeout_secs: u64,
    /// Path to cached classpath directory (for fallback).
    pub cache_path: Option<PathBuf>,
}

impl Default for ResolveConfig {
    fn default() -> Self {
        Self {
            project_root: PathBuf::new(),
            timeout_secs: 60,
            cache_path: None,
        }
    }
}
