//! Provenance tracking types for classpath entries.
//!
//! Tracks the origin and dependency status of each JAR on the classpath,
//! enabling FQN precedence decisions (workspace > direct > transitive) and
//! metadata attachment to emitted graph nodes.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Module/root-level scope membership for a JAR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClasspathScope {
    /// Logical module name (may be empty for root projects).
    pub module_name: String,
    /// Concrete module root path used for importer scoping.
    pub module_root: PathBuf,
    /// Whether the JAR is direct within this scope.
    pub is_direct: bool,
}

/// Tracks the origin and dependency status of a classpath entry.
///
/// Used during graph emission to:
/// 1. Set `is_direct_dependency` on [`ClasspathNodeMetadata`].
/// 2. Attach Maven/Gradle coordinates to nodes for provenance queries.
///
/// [`ClasspathNodeMetadata`]: sqry_core::graph::unified::storage::ClasspathNodeMetadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClasspathProvenance {
    /// Absolute path to the JAR file.
    pub jar_path: PathBuf,
    /// Maven coordinates if known (e.g., `"com.google.guava:guava:33.0.0"`).
    pub coordinates: Option<String>,
    /// Conservative aggregate directness across all scopes.
    ///
    /// `true` means the JAR is direct in every recorded scope.
    /// Mixed direct/transitive jars persist `false`; consult `scopes` for
    /// precise per-module semantics.
    pub is_direct: bool,
    /// Module/root scopes where this JAR appears.
    #[serde(default)]
    pub scopes: Vec<ClasspathScope>,
}

impl ClasspathProvenance {
    /// Returns whether the JAR is direct in at least one scope.
    #[must_use]
    pub fn has_direct_scope(&self) -> bool {
        self.scopes.iter().any(|scope| scope.is_direct)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provenance_roundtrip_json() {
        let prov = ClasspathProvenance {
            jar_path: PathBuf::from("/home/user/.m2/repository/guava-33.0.0.jar"),
            coordinates: Some("com.google.guava:guava:33.0.0".to_owned()),
            is_direct: true,
            scopes: vec![ClasspathScope {
                module_name: "app".to_owned(),
                module_root: PathBuf::from("/repo/app"),
                is_direct: true,
            }],
        };

        let json = serde_json::to_string(&prov).unwrap();
        let deserialized: ClasspathProvenance = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.jar_path, prov.jar_path);
        assert_eq!(deserialized.coordinates, prov.coordinates);
        assert_eq!(deserialized.is_direct, prov.is_direct);
    }

    #[test]
    fn test_provenance_transitive_no_coordinates() {
        let prov = ClasspathProvenance {
            jar_path: PathBuf::from("/tmp/some-transitive-1.0.jar"),
            coordinates: None,
            is_direct: false,
            scopes: vec![ClasspathScope {
                module_name: "lib".to_owned(),
                module_root: PathBuf::from("/repo/lib"),
                is_direct: false,
            }],
        };

        assert!(!prov.is_direct);
        assert!(prov.coordinates.is_none());
    }

    #[test]
    fn test_provenance_postcard_roundtrip() {
        let prov = ClasspathProvenance {
            jar_path: PathBuf::from("/repo/.gradle/caches/guava-33.jar"),
            coordinates: Some("com.google.guava:guava:33.0.0".to_owned()),
            is_direct: false,
            scopes: vec![ClasspathScope {
                module_name: "app".to_owned(),
                module_root: PathBuf::from("/repo/app"),
                is_direct: false,
            }],
        };

        let bytes = postcard::to_allocvec(&prov).unwrap();
        let deserialized: ClasspathProvenance = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(deserialized.jar_path, prov.jar_path);
        assert_eq!(deserialized.coordinates, prov.coordinates);
        assert_eq!(deserialized.is_direct, prov.is_direct);
    }
}
