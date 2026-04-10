//! Build system detection for JVM projects.
//!
//! Scans for marker files (build.gradle, pom.xml, BUILD, build.sbt) to identify
//! the build system in use. Priority order: Bazel > Gradle > Maven > sbt.

use std::path::{Path, PathBuf};

use log::warn;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

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

/// Directories skipped during recursive JVM build-root discovery.
const IGNORED_DIR_NAMES: &[&str] = &[".git", ".sqry", "target", "build", "node_modules", "vendor"];

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
    let result = detect_build_system_inner(project_root, override_build_system);
    write_diagnostics(project_root, &result);
    result
}

fn detect_build_system_inner(
    project_root: &Path,
    override_build_system: Option<&str>,
) -> DetectionResult {
    // Handle override case first.
    if let Some(override_value) = override_build_system {
        return if let Some(bs) = BuildSystem::from_str_loose(override_value) {
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
    }

    let (markers_found, best_system) = scan_markers(project_root);

    DetectionResult {
        build_system: best_system,
        project_root: project_root.to_path_buf(),
        markers_found,
        override_source: None,
    }
}

fn scan_markers(project_root: &Path) -> (Vec<String>, Option<BuildSystem>) {
    let mut markers_found = Vec::new();
    let mut best_system: Option<BuildSystem> = None;

    for build_system in BuildSystem::ALL {
        for marker in build_system.markers() {
            let marker_path = project_root.join(marker);
            if marker_path.exists() {
                markers_found.push((*marker).to_string());

                match best_system {
                    Some(current) if current.priority() >= build_system.priority() => {}
                    _ => {
                        best_system = Some(build_system);
                    }
                }
            }
        }
    }

    (markers_found, best_system)
}

/// Discover JVM build roots recursively under a repository root.
///
/// Child roots that are already modeled by an ancestor Gradle settings file or
/// Maven reactor are pruned to avoid duplicate resolution.
#[must_use]
pub fn discover_build_roots(
    project_root: &Path,
    override_build_system: Option<&str>,
) -> Vec<DetectionResult> {
    if let Some(override_value) = override_build_system {
        let Some(build_system) = BuildSystem::from_str_loose(override_value) else {
            return vec![detect_build_system(project_root, Some(override_value))];
        };
        let mut roots = discover_build_roots_for_system(project_root, build_system);
        if roots.is_empty() {
            roots.push(DetectionResult {
                build_system: Some(build_system),
                project_root: project_root.to_path_buf(),
                markers_found: Vec::new(),
                override_source: Some(override_value.to_string()),
            });
        } else {
            for root in &mut roots {
                root.override_source = Some(override_value.to_string());
            }
        }
        return roots;
    }

    let mut candidates = Vec::new();
    for entry in WalkDir::new(project_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| should_descend(entry.path()))
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_dir() {
            continue;
        }

        let detection = detect_build_system_inner(entry.path(), None);
        if detection.build_system.is_some() {
            candidates.push(detection);
        }
    }

    prune_discovered_roots(candidates)
}

fn discover_build_roots_for_system(
    project_root: &Path,
    build_system: BuildSystem,
) -> Vec<DetectionResult> {
    let mut candidates = Vec::new();
    for entry in WalkDir::new(project_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| should_descend(entry.path()))
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_dir() {
            continue;
        }

        let (markers_found, detected) = scan_markers(entry.path());
        if detected == Some(build_system) {
            candidates.push(DetectionResult {
                build_system: Some(build_system),
                project_root: entry.path().to_path_buf(),
                markers_found,
                override_source: None,
            });
        }
    }

    prune_discovered_roots(candidates)
}

fn should_descend(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_none_or(|name| !IGNORED_DIR_NAMES.contains(&name))
}

fn prune_discovered_roots(mut candidates: Vec<DetectionResult>) -> Vec<DetectionResult> {
    candidates.sort_by(|a, b| {
        let depth_a = a.project_root.components().count();
        let depth_b = b.project_root.components().count();
        depth_a
            .cmp(&depth_b)
            .then_with(|| a.project_root.cmp(&b.project_root))
    });

    let mut accepted = Vec::new();
    'candidate: for candidate in candidates {
        for ancestor in &accepted {
            if should_prune_under_ancestor(&candidate, ancestor) {
                continue 'candidate;
            }
        }
        accepted.push(candidate);
    }

    accepted
}

fn should_prune_under_ancestor(candidate: &DetectionResult, ancestor: &DetectionResult) -> bool {
    let Some(candidate_system) = candidate.build_system else {
        return false;
    };
    let Some(ancestor_system) = ancestor.build_system else {
        return false;
    };
    if candidate_system != ancestor_system || candidate.project_root == ancestor.project_root {
        return false;
    }

    match candidate_system {
        BuildSystem::Gradle => {
            candidate.project_root.starts_with(&ancestor.project_root)
                && ancestor
                    .markers_found
                    .iter()
                    .any(|marker| marker == "settings.gradle" || marker == "settings.gradle.kts")
        }
        BuildSystem::Maven => {
            maven_reactor_contains(&ancestor.project_root, &candidate.project_root)
        }
        BuildSystem::Bazel | BuildSystem::Sbt => {
            candidate.project_root.starts_with(&ancestor.project_root)
        }
    }
}

fn maven_reactor_contains(ancestor_root: &Path, candidate_root: &Path) -> bool {
    let pom_path = ancestor_root.join("pom.xml");
    if !pom_path.exists() {
        return false;
    }

    let candidate_root = canonicalish(candidate_root);
    crate::resolve::maven::detect_modules(&pom_path)
        .into_iter()
        .map(|module| canonicalish(&ancestor_root.join(module)))
        .any(|module_root| module_root == candidate_root)
}

fn canonicalish(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
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

    #[test]
    fn test_discover_build_roots_mixed_nested_repo() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path().join("services/app").as_path(), &["build.gradle"]);
        create_markers(tmp.path().join("libs/shared").as_path(), &["pom.xml"]);
        create_markers(tmp.path().join("target/generated").as_path(), &["pom.xml"]);

        let roots = discover_build_roots(tmp.path(), None);
        let root_paths: Vec<_> = roots.iter().map(|root| root.project_root.clone()).collect();

        assert!(root_paths.contains(&tmp.path().join("services/app")));
        assert!(root_paths.contains(&tmp.path().join("libs/shared")));
        assert!(!root_paths.contains(&tmp.path().join("target/generated")));
    }

    #[test]
    fn test_discover_build_roots_prunes_gradle_children_under_settings_root() {
        let tmp = TempDir::new().unwrap();
        create_markers(tmp.path(), &["settings.gradle", "build.gradle"]);
        create_markers(tmp.path().join("app").as_path(), &["build.gradle"]);

        let roots = discover_build_roots(tmp.path(), None);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].project_root, tmp.path());
        assert_eq!(roots[0].build_system, Some(BuildSystem::Gradle));
    }
}
