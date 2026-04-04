//! Provenance tracking types for classpath entries.
//!
//! Tracks the origin and dependency status of each JAR on the classpath,
//! enabling FQN precedence decisions (workspace > direct > transitive) and
//! metadata attachment to emitted graph nodes.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Tracks the origin and dependency status of a classpath entry.
///
/// Used during graph emission to:
/// 1. Set `is_direct_dependency` on [`ClasspathNodeMetadata`].
/// 2. Control ExportMap registration order (direct before transitive).
/// 3. Attach Maven/Gradle coordinates to nodes for provenance queries.
///
/// [`ClasspathNodeMetadata`]: sqry_core::graph::unified::storage::ClasspathNodeMetadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClasspathProvenance {
    /// Absolute path to the JAR file.
    pub jar_path: PathBuf,
    /// Maven coordinates if known (e.g., `"com.google.guava:guava:33.0.0"`).
    pub coordinates: Option<String>,
    /// `true` if this JAR is a direct dependency of the project;
    /// `false` if it is a transitive (indirect) dependency.
    pub is_direct: bool,
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
        };

        let bytes = postcard::to_allocvec(&prov).unwrap();
        let deserialized: ClasspathProvenance = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(deserialized.jar_path, prov.jar_path);
        assert_eq!(deserialized.coordinates, prov.coordinates);
        assert_eq!(deserialized.is_direct, prov.is_direct);
    }
}
