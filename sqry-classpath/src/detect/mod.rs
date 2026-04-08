//! Build system detection for JVM projects.
//!
//! Scans for marker files (build.gradle, pom.xml, BUILD, build.sbt) to identify
//! the build system in use. Priority order: Bazel > Gradle > Maven > sbt.

use std::path::{Path, PathBuf};

use log::warn;
use serde::{Deserialize, Serialize};

/// JVM build system type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BuildSystem {
    Gradle,
    Maven,
    Bazel,
    Sbt,
}

impl BuildSystem {
    /// Parse a build system name from a string (case-insensitive).
    ///
    /// Returns `None` for unrecognized values.
    fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "gradle" => Some(Self::Gradle),
            "maven" => Some(Self::Maven),
            "bazel" => Some(Self::Bazel),
            "sbt" => Some(Self::Sbt),
            _ => None,
        }
    }

    /// Detection priority (higher wins when multiple build systems are present).
    ///
    /// Bazel projects often also contain `pom.xml` or `build.gradle` for IDE
    /// compatibility, so Bazel markers should take precedence.
    fn priority(self) -> u8 {
        match self {
            Self::Bazel => 4,
            Self::Gradle => 3,
            Self::Maven => 2,
            Self::Sbt => 1,
        }
    }

    /// Marker files that indicate this build system is in use.
    fn markers(self) -> &'static [&'static str] {
        match self {
            Self::Gradle => &[
                "build.gradle",
                "build.gradle.kts",
                "settings.gradle",
                "settings.gradle.kts",
                "gradlew",
            ],
            Self::Maven => &["pom.xml"],
            Self::Bazel => &[
                "BUILD",
                "BUILD.bazel",
                "WORKSPACE",
                "WORKSPACE.bazel",
                "MODULE.bazel",
            ],
            Self::Sbt => &["build.sbt", "project/build.properties"],
        }
    }

    /// All build system variants for iteration.
    const ALL: [Self; 4] = [Self::Gradle, Self::Maven, Self::Bazel, Self::Sbt];
}

/// Result of build system detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionResult {
    /// Detected build system, if any.
    pub build_system: Option<BuildSystem>,
    /// Project root directory where the build system was detected.
    pub project_root: PathBuf,
    /// Marker files that were found.
    pub markers_found: Vec<String>,
    /// Override source (if `--build-system` CLI flag was used).
    pub override_source: Option<String>,
}

/// Detect the build system for a JVM project.
///
/// Scans for marker files starting at `project_root`. If `override_build_system`
/// is provided, it takes precedence over auto-detection.
///
/// # Priority (when multiple build systems detected)
/// Bazel > Gradle > Maven > sbt
///
/// This priority reflects that Bazel projects often also contain `pom.xml` or
/// `build.gradle` for IDE compatibility, so the Bazel marker should win.
#[must_use]
pub fn detect_build_system(
    project_root: &Path,
    override_build_system: Option<&str>,
) -> DetectionResult {
    // Handle override case first.
    if let Some(override_value) = override_build_system {
        let result = if let Some(bs) = BuildSystem::from_str_loose(override_value) {
            DetectionResult {
                build_system: Some(bs),
                project_root: project_root.to_path_buf(),
                markers_found: Vec::new(),
                override_source: Some(override_value.to_string()),
            }
        } else {
            warn!(
                "Invalid build system override '{override_value}'. Valid values: gradle, maven, bazel, sbt"
            );
            DetectionResult {
                build_system: None,
                project_root: project_root.to_path_buf(),
                markers_found: Vec::new(),
                override_source: Some(override_value.to_string()),
            }
        };
        write_diagnostics(project_root, &result);
        return result;
    }

    // Auto-detect by scanning for marker files.
    let mut markers_found = Vec::new();
    let mut best_system: Option<BuildSystem> = None;

    for build_system in BuildSystem::ALL {
        for marker in build_system.markers() {
            let marker_path = project_root.join(marker);
            if marker_path.exists() {
                markers_found.push((*marker).to_string());

                match best_system {
                    Some(current) if current.priority() >= build_system.priority() => {
                        // Current winner has equal or higher priority; keep it.
                    }
                    _ => {
                        best_system = Some(build_system);
                    }
                }
            }
        }
    }

    let result = DetectionResult {
        build_system: best_system,
        project_root: project_root.to_path_buf(),
        markers_found,
        override_source: None,
    };

    write_diagnostics(project_root, &result);
    result
}

/// Write the detection result as JSON to `.sqry/classpath/build-system.json`
/// for debugging. Silently skips if the directory does not exist or the write fails.
fn write_diagnostics(project_root: &Path, result: &DetectionResult) {
    let sqry_dir = project_root.join(".sqry").join("classpath");

    // Create the directory if it doesn't exist. If creation fails, skip silently.
    if let Err(e) = std::fs::create_dir_all(&sqry_dir) {
        warn!(
            "Could not create diagnostics directory {}: {}",
            sqry_dir.display(),
            e
        );
        return;
    }

    let diagnostics_path = sqry_dir.join("build-system.json");
    match serde_json::to_string_pretty(result) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&diagnostics_path, json) {
                warn!(
                    "Could not write build system diagnostics to {}: {}",
                    diagnostics_path.display(),
                    e
                );
            }
        }
        Err(e) => {
            warn!("Could not serialize detection result: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper: create marker files in the temp directory.
    fn create_markers(dir: &Path, markers: &[&str]) {
        for marker in markers {
            let path = dir.join(marker);
            // Create parent directories if needed (e.g., project/build.properties).
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, "").unwrap();
        }
    }

    #[test]
    fn test_build_gradle_detected() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["build.gradle"]);

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Gradle));
        assert!(result.markers_found.contains(&"build.gradle".to_string()));
        assert!(result.override_source.is_none());
    }

    #[test]
    fn test_pom_xml_only_maven() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["pom.xml"]);

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Maven));
        assert!(result.markers_found.contains(&"pom.xml".to_string()));
    }

    #[test]
    fn test_build_and_pom_bazel_wins() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["BUILD", "pom.xml"]);

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Bazel));
        assert!(result.markers_found.contains(&"BUILD".to_string()));
        assert!(result.markers_found.contains(&"pom.xml".to_string()));
    }

    #[test]
    fn test_build_sbt_only() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["build.sbt"]);

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Sbt));
        assert!(result.markers_found.contains(&"build.sbt".to_string()));
    }

    #[test]
    fn test_no_markers_none() {
        let tmp = TempDir::new().unwrap();

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, None);
        assert!(result.markers_found.is_empty());
    }

    #[test]
    fn test_override_works() {
        let tmp = TempDir::new().unwrap();
        // Even though pom.xml is present, override should force Gradle.
        create_markers(tmp.path(), &["pom.xml"]);

        let result = detect_build_system(tmp.path(), Some("gradle"));
        assert_eq!(result.build_system, Some(BuildSystem::Gradle));
        assert_eq!(result.override_source, Some("gradle".to_string()));
        // Override skips marker scanning.
        assert!(result.markers_found.is_empty());
    }

    #[test]
    fn test_invalid_override_returns_none() {
        let tmp = TempDir::new().unwrap();

        let result = detect_build_system(tmp.path(), Some("ninja"));
        assert_eq!(result.build_system, None);
        assert_eq!(result.override_source, Some("ninja".to_string()));
    }

    #[test]
    fn test_all_markers_bazel_wins() {
        let tmp = TempDir::new().unwrap();
        create_markers(
            tmp.path(),
            &["build.gradle", "pom.xml", "BUILD", "build.sbt", "WORKSPACE"],
        );

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Bazel));
        // All markers should be recorded.
        assert!(result.markers_found.len() >= 4);
    }

    #[test]
    fn test_build_gradle_kts_detected() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["build.gradle.kts"]);

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Gradle));
        assert!(
            result
                .markers_found
                .contains(&"build.gradle.kts".to_string())
        );
    }

    #[test]
    fn test_workspace_bazel_detected() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["WORKSPACE.bazel"]);

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Bazel));
        assert!(
            result
                .markers_found
                .contains(&"WORKSPACE.bazel".to_string())
        );
    }

    #[test]
    fn test_diagnostics_file_written() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["pom.xml"]);

        let _result = detect_build_system(tmp.path(), None);

        let diagnostics_path = tmp.path().join(".sqry/classpath/build-system.json");
        assert!(diagnostics_path.exists(), "diagnostics file should exist");

        let contents = std::fs::read_to_string(&diagnostics_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["build_system"], "Maven");
    }

    #[test]
    fn test_project_root_recorded() {
        let tmp = TempDir::new().unwrap();
        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.project_root, tmp.path());
    }

    #[test]
    fn test_override_case_insensitive() {
        let tmp = TempDir::new().unwrap();

        let result = detect_build_system(tmp.path(), Some("MAVEN"));
        assert_eq!(result.build_system, Some(BuildSystem::Maven));

        let result = detect_build_system(tmp.path(), Some("Gradle"));
        assert_eq!(result.build_system, Some(BuildSystem::Gradle));

        let result = detect_build_system(tmp.path(), Some("SBT"));
        assert_eq!(result.build_system, Some(BuildSystem::Sbt));

        let result = detect_build_system(tmp.path(), Some("BAZEL"));
        assert_eq!(result.build_system, Some(BuildSystem::Bazel));
    }

    #[test]
    fn test_settings_gradle_detected() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["settings.gradle"]);

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Gradle));
    }

    #[test]
    fn test_settings_gradle_kts_detected() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["settings.gradle.kts"]);

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Gradle));
    }

    #[test]
    fn test_gradlew_detected() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["gradlew"]);

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Gradle));
    }

    #[test]
    fn test_module_bazel_detected() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["MODULE.bazel"]);

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Bazel));
    }

    #[test]
    fn test_sbt_project_build_properties() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["project/build.properties"]);

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Sbt));
        assert!(
            result
                .markers_found
                .contains(&"project/build.properties".to_string())
        );
    }

    #[test]
    fn test_gradle_vs_maven_gradle_wins() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["build.gradle", "pom.xml"]);

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Gradle));
    }

    #[test]
    fn test_gradle_vs_sbt_gradle_wins() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["build.gradle", "build.sbt"]);

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Gradle));
    }

    #[test]
    fn test_maven_vs_sbt_maven_wins() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["pom.xml", "build.sbt"]);

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Maven));
    }

    #[test]
    fn test_multiple_gradle_markers() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["build.gradle", "settings.gradle", "gradlew"]);

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Gradle));
        assert_eq!(result.markers_found.len(), 3);
    }

    #[test]
    fn test_multiple_bazel_markers() {
        let tmp = TempDir::new().unwrap();
        create_markers(
            tmp.path(),
            &["BUILD", "BUILD.bazel", "WORKSPACE", "MODULE.bazel"],
        );

        let result = detect_build_system(tmp.path(), None);
        assert_eq!(result.build_system, Some(BuildSystem::Bazel));
        assert_eq!(result.markers_found.len(), 4);
    }
}
