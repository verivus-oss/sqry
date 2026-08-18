//! Bazel classpath resolver.
//!
//! Resolves JVM classpath entries from Bazel workspaces by:
//! 1. Running `bazel cquery` to list Java compilation outputs
//! 2. Parsing output for JAR paths in `bazel-out/` and external repository cache
//! 3. Parsing `maven_install.json` for Maven coordinate mapping (`rules_jvm_external`)
//! 4. Looking up source JARs in the Coursier cache
//! 5. Falling back to cached classpath on failure

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use log::{debug, info, warn};

use crate::{ClasspathError, ClasspathResult};

use super::{ClasspathEntry, ResolveConfig, ResolvedClasspath};

const BAZEL_CACHE_FILE: &str = "bazel-resolved-classpath.json";

/// Bazel cquery command and arguments for listing Java dependency outputs.
const BAZEL_CQUERY_KIND_PATTERN: &str =
    r#"kind("java_library|java_import|jvm_import", deps(//...))"#;

// ── Public API ──────────────────────────────────────────────────────────────

/// Resolve classpath for a Bazel project.
///
/// Strategy:
/// 1. Try `bazel cquery` to list Java compilation outputs
/// 2. Parse output for JAR paths in `bazel-out/` and external repository cache
/// 3. Try `maven_install.json` for coordinates mapping
/// 4. On failure, fall back to cache
#[allow(clippy::missing_errors_doc)] // Internal helper
pub fn resolve_bazel_classpath(config: &ResolveConfig) -> ClasspathResult<Vec<ResolvedClasspath>> {
    info!(
        "Resolving Bazel classpath in {}",
        config.project_root.display()
    );

    // Attempt live resolution via bazel cquery.
    match run_bazel_cquery(config) {
        Ok(jar_paths) => {
            info!("Bazel cquery returned {} JAR paths", jar_paths.len());
            let coordinates_map = load_maven_install_json(&config.project_root);
            let entries = build_entries(&jar_paths, &coordinates_map);
            let resolved = ResolvedClasspath {
                module_name: infer_module_name(&config.project_root),
                module_root: config.project_root.clone(),
                entries,
            };
            Ok(vec![resolved])
        }
        Err(e) => {
            warn!("Bazel cquery failed: {e}. Attempting cache fallback.");
            try_cache_fallback(config, &e)
        }
    }
}

// ── Bazel cquery execution ──────────────────────────────────────────────────

/// Run `bazel cquery` and return the list of JAR file paths from its output.
fn run_bazel_cquery(config: &ResolveConfig) -> ClasspathResult<Vec<PathBuf>> {
    let bazel_bin = find_bazel_binary()?;

    let mut cmd = Command::new(&bazel_bin);
    cmd.arg("cquery")
        .arg(BAZEL_CQUERY_KIND_PATTERN)
        .arg("--output=files")
        .current_dir(&config.project_root)
        // Suppress Bazel's own stderr noise.
        .stderr(std::process::Stdio::null());

    debug!("Running: {} cquery ... --output=files", bazel_bin.display());

    let output = run_command_with_timeout(&mut cmd, config.timeout_secs)?;

    if !output.status.success() {
        return Err(ClasspathError::ResolutionFailed(format!(
            "bazel cquery exited with status {}",
            output.status
        )));
    }

    let jars = parse_cquery_output(&output.stdout);
    Ok(jars)
}

/// Locate the `bazel` binary on `$PATH`.
fn find_bazel_binary() -> ClasspathResult<PathBuf> {
    which_binary("bazel").ok_or_else(|| {
        ClasspathError::ResolutionFailed(
            "bazel binary not found on PATH. Install Bazel to resolve classpath.".to_string(),
        )
    })
}

/// Parse raw `bazel cquery --output=files` output, keeping only `.jar` paths.
///
/// Each line of output is a single file path. We filter to keep only lines
/// ending in `.jar` (case-insensitive) to exclude `.srcjar`, class dirs, etc.
fn parse_cquery_output(stdout: &[u8]) -> Vec<PathBuf> {
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.ok()?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            // Only keep .jar files (not .srcjar, .aar, etc.)
            if trimmed.to_ascii_lowercase().ends_with(".jar") {
                Some(PathBuf::from(trimmed))
            } else {
                None
            }
        })
        .collect()
}

// ── maven_install.json ──────────────────────────────────────────────────────

/// A single dependency entry from `maven_install.json`.
#[derive(Debug, serde::Deserialize)]
struct MavenInstallDependency {
    /// Maven coordinate, e.g. `com.google.guava:guava:33.0.0`.
    coord: String,
    /// Relative file path within the Coursier/repository cache.
    #[serde(default)]
    file: Option<String>,
}

/// Top-level structure of `maven_install.json` (only the fields we need).
#[derive(Debug, serde::Deserialize)]
struct MavenInstallJson {
    dependency_tree: Option<DependencyTree>,
}

#[derive(Debug, serde::Deserialize)]
struct DependencyTree {
    dependencies: Vec<MavenInstallDependency>,
}

/// Coordinate mapping: JAR filename → Maven coordinate string.
type CoordinatesMap = std::collections::HashMap<String, String>;

/// Try to load `maven_install.json` (from `rules_jvm_external`) and build a
/// mapping from JAR filename to Maven coordinates.
///
/// Returns an empty map on any error (file missing, parse error, etc.).
fn load_maven_install_json(project_root: &Path) -> CoordinatesMap {
    let candidates = [
        project_root.join("maven_install.json"),
        project_root.join("third_party/maven_install.json"),
    ];

    for path in &candidates {
        if let Some(map) = try_parse_maven_install(path) {
            info!(
                "Loaded {} coordinate mappings from {}",
                map.len(),
                path.display()
            );
            return map;
        }
    }

    debug!("No maven_install.json found; coordinate mapping unavailable");
    CoordinatesMap::new()
}

/// Parse a single `maven_install.json` file into a coordinate map.
fn try_parse_maven_install(path: &Path) -> Option<CoordinatesMap> {
    let content = std::fs::read_to_string(path).ok()?;
    let parsed: MavenInstallJson = serde_json::from_str(&content).ok()?;
    let tree = parsed.dependency_tree?;

    let mut map = CoordinatesMap::with_capacity(tree.dependencies.len());
    for dep in &tree.dependencies {
        // Build a filename from the coordinate for matching.
        // Also store the explicit `file` field's basename if present.
        if let Some(ref file_path) = dep.file
            && let Some(basename) = Path::new(file_path).file_name()
        {
            map.insert(basename.to_string_lossy().to_string(), dep.coord.clone());
        }
        // Also derive filename from coordinates: artifact-version.jar
        if let Some(derived) = derive_jar_filename_from_coord(&dep.coord) {
            map.insert(derived, dep.coord.clone());
        }
    }
    Some(map)
}

/// Derive `artifact-version.jar` from a Maven coordinate like `group:artifact:version`.
fn derive_jar_filename_from_coord(coord: &str) -> Option<String> {
    let parts: Vec<&str> = coord.split(':').collect();
    if parts.len() >= 3 {
        Some(format!("{}-{}.jar", parts[1], parts[2]))
    } else {
        None
    }
}

/// Parse Maven coordinates from a Coursier cache path.
///
/// Coursier cache paths follow the pattern:
/// `~/.cache/coursier/v1/https/repo1.maven.org/maven2/<group-path>/<artifact>/<version>/<artifact>-<version>.jar`
///
/// We extract `group:artifact:version` from this structure.
fn parse_coursier_coordinates(jar_path: &Path) -> Option<String> {
    let path_str = jar_path.to_str()?;

    // Look for the `/maven2/` segment that precedes the Maven layout.
    let maven2_idx = path_str.find("/maven2/")?;
    let after_maven2 = &path_str[maven2_idx + "/maven2/".len()..];

    // Split into path components.
    let components: Vec<&str> = after_maven2.split('/').collect();
    // Need at least: group-parts... / artifact / version / filename
    if components.len() < 3 {
        return None;
    }

    let filename = *components.last()?;
    let version = components[components.len() - 2];
    let artifact = components[components.len() - 3];
    let group_parts = &components[..components.len() - 3];

    if group_parts.is_empty() {
        return None;
    }

    // Verify filename matches expected pattern.
    let expected_prefix = format!("{artifact}-{version}");
    if !filename.starts_with(&expected_prefix) {
        return None;
    }

    let group = group_parts.join(".");
    Some(format!("{group}:{artifact}:{version}"))
}

// ── Entry construction ──────────────────────────────────────────────────────

/// Build `ClasspathEntry` records from JAR paths, enriching with coordinates
/// and source JAR locations where possible.
fn build_entries(jar_paths: &[PathBuf], coordinates_map: &CoordinatesMap) -> Vec<ClasspathEntry> {
    jar_paths
        .iter()
        .map(|jar_path| {
            let coordinates = resolve_coordinates(jar_path, coordinates_map);
            let source_jar = find_source_jar(jar_path);

            ClasspathEntry {
                jar_path: jar_path.clone(),
                coordinates,
                is_direct: false, // Bazel cquery returns the full transitive closure.
                source_jar,
            }
        })
        .collect()
}

/// Try to resolve Maven coordinates for a JAR path.
///
/// Strategy:
/// 1. Look up the JAR filename in the `maven_install.json` coordinate map
/// 2. Try parsing coordinates from a Coursier cache path structure
fn resolve_coordinates(jar_path: &Path, coordinates_map: &CoordinatesMap) -> Option<String> {
    // Strategy 1: Filename lookup in maven_install.json mappings.
    if let Some(filename) = jar_path.file_name() {
        let filename_str = filename.to_string_lossy();
        if let Some(coord) = coordinates_map.get(filename_str.as_ref()) {
            return Some(coord.clone());
        }
    }

    // Strategy 2: Parse from Coursier cache path.
    parse_coursier_coordinates(jar_path)
}

/// Find a source JAR alongside a main JAR.
///
/// Looks in two locations:
/// 1. Same directory: `artifact-version-sources.jar`
/// 2. Coursier cache: replace `.jar` with `-sources.jar` in the filename
fn find_source_jar(jar_path: &Path) -> Option<PathBuf> {
    let stem = jar_path.file_stem()?.to_string_lossy();
    let parent = jar_path.parent()?;

    // Try `<stem>-sources.jar` in the same directory.
    let sources_jar = parent.join(format!("{stem}-sources.jar"));
    if sources_jar.exists() {
        return Some(sources_jar);
    }

    // Try Coursier cache: look for `-sources.jar` variant.
    if let Some(coursier_sources) = find_coursier_source_jar(jar_path)
        && coursier_sources.exists()
    {
        return Some(coursier_sources);
    }

    None
}

/// Derive the Coursier cache path for a source JAR given the main JAR path.
///
/// In Coursier cache, source JARs live at the same path but with `-sources`
/// appended before `.jar`.
#[allow(clippy::case_sensitive_file_extension_comparisons)] // Known file extensions
fn find_coursier_source_jar(jar_path: &Path) -> Option<PathBuf> {
    let path_str = jar_path.to_str()?;
    if path_str.ends_with(".jar") && !path_str.ends_with("-sources.jar") {
        let sources_path = format!("{}-sources.jar", &path_str[..path_str.len() - 4]);
        Some(PathBuf::from(sources_path))
    } else {
        None
    }
}

// ── Cache fallback ──────────────────────────────────────────────────────────

/// Attempt to load a previously cached classpath when live resolution fails.
/// Read a previously cached Bazel classpath without invoking Bazel. Returns
/// `None` when no usable cache is present. Used when build-tool execution is
/// disabled (`--no-build-tool`).
pub(crate) fn read_cached_classpath(config: &ResolveConfig) -> Option<Vec<ResolvedClasspath>> {
    let cache_path = config.cache_path.as_ref()?;
    let cache_file = if cache_path.is_dir() {
        cache_path.join(BAZEL_CACHE_FILE)
    } else {
        cache_path.clone()
    };
    if !cache_file.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&cache_file).ok()?;
    let cached: Vec<ResolvedClasspath> = serde_json::from_str(&content).ok()?;
    (!cached.is_empty()).then_some(cached)
}

fn try_cache_fallback(
    config: &ResolveConfig,
    original_error: &ClasspathError,
) -> ClasspathResult<Vec<ResolvedClasspath>> {
    if let Some(ref cache_path) = config.cache_path {
        let cache_path = if cache_path.is_dir() {
            cache_path.join(BAZEL_CACHE_FILE)
        } else {
            cache_path.clone()
        };
        if cache_path.exists() {
            info!("Loading cached classpath from {}", cache_path.display());
            let content = std::fs::read_to_string(&cache_path).map_err(|e| {
                ClasspathError::CacheError(format!(
                    "Failed to read cache file {}: {e}",
                    cache_path.display()
                ))
            })?;
            let cached: Vec<ResolvedClasspath> = serde_json::from_str(&content).map_err(|e| {
                ClasspathError::CacheError(format!(
                    "Failed to parse cache file {}: {e}",
                    cache_path.display()
                ))
            })?;
            return Ok(cached);
        }
        warn!(
            "Cache file {} does not exist; cannot fall back",
            cache_path.display()
        );
    }

    Err(ClasspathError::ResolutionFailed(format!(
        "Bazel resolution failed and no cache available. Original error: {original_error}"
    )))
}

// ── Utility functions ───────────────────────────────────────────────────────

/// Find a binary on `$PATH` using `which`-style lookup.
fn which_binary(name: &str) -> Option<PathBuf> {
    // Use the `which` crate pattern: scan PATH entries.
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Run a command with a timeout, returning its output.
fn run_command_with_timeout(
    cmd: &mut Command,
    timeout_secs: u64,
) -> ClasspathResult<std::process::Output> {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ClasspathError::ResolutionFailed(format!("Failed to spawn command: {e}")))?;

    let timeout = Duration::from_secs(timeout_secs);

    // Wait with timeout using a polling approach.
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                // Process exited; collect output.
                return child.wait_with_output().map_err(|e| {
                    ClasspathError::ResolutionFailed(format!("Failed to collect output: {e}"))
                });
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    // Kill the process on timeout.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ClasspathError::ResolutionFailed(format!(
                        "Command timed out after {timeout_secs}s"
                    )));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(ClasspathError::ResolutionFailed(format!(
                    "Failed to check process status: {e}"
                )));
            }
        }
    }
}

/// Infer a module name from the project root directory name.
fn infer_module_name(project_root: &Path) -> String {
    project_root
        .file_name()
        .map_or_else(|| "root".to_string(), |n| n.to_string_lossy().to_string())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ── Test: parse_cquery_output filters to JARs only ──────────────────

    #[test]
    fn test_parse_cquery_output_filters_jars() {
        let output = b"\
bazel-out/k8-fastbuild/bin/external/maven/com/google/guava/guava/33.0.0/guava-33.0.0.jar
bazel-out/k8-fastbuild/bin/src/main/java/com/example/libapp.jar
bazel-out/k8-fastbuild/bin/src/main/java/com/example/libapp-class.jar
some/path/to/resource.txt
another/path/to/data.proto
";

        let result = parse_cquery_output(output);
        assert_eq!(result.len(), 3);
        assert!(
            result
                .iter()
                .all(|p| p.extension().is_some_and(|e| e == "jar"))
        );
    }

    #[test]
    fn test_parse_cquery_output_empty() {
        let result = parse_cquery_output(b"");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_cquery_output_filters_non_jar() {
        let output = b"\
/path/to/classes/
/path/to/resource.xml
/path/to/source.srcjar
/path/to/real.jar
";
        let result = parse_cquery_output(output);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PathBuf::from("/path/to/real.jar"));
    }

    #[test]
    fn test_parse_cquery_output_blank_lines_ignored() {
        let output = b"\
/path/a.jar

/path/b.jar

";
        let result = parse_cquery_output(output);
        assert_eq!(result.len(), 2);
    }

    // ── Test: maven_install.json parsing ─────────────────────────────────

    #[test]
    fn test_maven_install_json_parsing() {
        let tmp = TempDir::new().unwrap();
        let json = serde_json::json!({
            "dependency_tree": {
                "dependencies": [
                    {
                        "coord": "com.google.guava:guava:33.0.0",
                        "file": "v1/https/repo1.maven.org/maven2/com/google/guava/guava/33.0.0/guava-33.0.0.jar"
                    },
                    {
                        "coord": "org.slf4j:slf4j-api:2.0.9",
                        "file": "v1/https/repo1.maven.org/maven2/org/slf4j/slf4j-api/2.0.9/slf4j-api-2.0.9.jar"
                    }
                ]
            }
        });

        let path = tmp.path().join("maven_install.json");
        std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

        let map = load_maven_install_json(tmp.path());
        assert!(map.contains_key("guava-33.0.0.jar"));
        assert_eq!(map["guava-33.0.0.jar"], "com.google.guava:guava:33.0.0");
        assert!(map.contains_key("slf4j-api-2.0.9.jar"));
        assert_eq!(map["slf4j-api-2.0.9.jar"], "org.slf4j:slf4j-api:2.0.9");
    }

    #[test]
    fn test_maven_install_json_missing_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let map = load_maven_install_json(tmp.path());
        assert!(map.is_empty());
    }

    #[test]
    fn test_maven_install_json_malformed_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("maven_install.json");
        std::fs::write(&path, "{ invalid json }}}").unwrap();

        let map = load_maven_install_json(tmp.path());
        assert!(map.is_empty());
    }

    #[test]
    fn test_maven_install_json_no_dependency_tree() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("maven_install.json");
        std::fs::write(&path, r#"{"version": "1.0"}"#).unwrap();

        let map = load_maven_install_json(tmp.path());
        assert!(map.is_empty());
    }

    #[test]
    fn test_maven_install_json_third_party_location() {
        let tmp = TempDir::new().unwrap();
        let third_party = tmp.path().join("third_party");
        std::fs::create_dir_all(&third_party).unwrap();
        let json = serde_json::json!({
            "dependency_tree": {
                "dependencies": [
                    {
                        "coord": "junit:junit:4.13.2",
                        "file": "v1/https/repo1.maven.org/maven2/junit/junit/4.13.2/junit-4.13.2.jar"
                    }
                ]
            }
        });
        let path = third_party.join("maven_install.json");
        std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

        let map = load_maven_install_json(tmp.path());
        assert!(map.contains_key("junit-4.13.2.jar"));
    }

    // ── Test: coordinate derivation ─────────────────────────────────────

    #[test]
    fn test_derive_jar_filename_from_coord() {
        assert_eq!(
            derive_jar_filename_from_coord("com.google.guava:guava:33.0.0"),
            Some("guava-33.0.0.jar".to_string())
        );
        assert_eq!(
            derive_jar_filename_from_coord("org.slf4j:slf4j-api:2.0.9"),
            Some("slf4j-api-2.0.9.jar".to_string())
        );
        assert_eq!(derive_jar_filename_from_coord("invalid"), None);
        assert_eq!(derive_jar_filename_from_coord("group:artifact"), None);
    }

    #[test]
    fn test_parse_coursier_coordinates() {
        let path = PathBuf::from(
            "/home/user/.cache/coursier/v1/https/repo1.maven.org/maven2/com/google/guava/guava/33.0.0/guava-33.0.0.jar",
        );
        let coords = parse_coursier_coordinates(&path);
        assert_eq!(coords, Some("com.google.guava:guava:33.0.0".to_string()));
    }

    #[test]
    fn test_parse_coursier_coordinates_single_group() {
        let path = PathBuf::from(
            "/home/user/.cache/coursier/v1/https/repo1.maven.org/maven2/junit/junit/4.13.2/junit-4.13.2.jar",
        );
        let coords = parse_coursier_coordinates(&path);
        assert_eq!(coords, Some("junit:junit:4.13.2".to_string()));
    }

    #[test]
    fn test_parse_coursier_coordinates_not_coursier_path() {
        let path = PathBuf::from("/usr/local/lib/some.jar");
        let coords = parse_coursier_coordinates(&path);
        assert_eq!(coords, None);
    }

    // ── Test: missing bazel binary ──────────────────────────────────────

    #[test]
    fn test_missing_bazel_binary_error() {
        // Temporarily override PATH to ensure bazel is not found.
        let tmp = TempDir::new().unwrap();
        let original_path = std::env::var_os("PATH");

        // Set PATH to empty directory only.
        // SAFETY: This test is not run in parallel with other tests that depend
        // on PATH. We restore the original value immediately after the check.
        unsafe { std::env::set_var("PATH", tmp.path()) };
        let result = find_bazel_binary();
        // Restore PATH.
        if let Some(p) = original_path {
            unsafe { std::env::set_var("PATH", p) };
        }

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not found"),
            "Error should mention 'not found': {err_msg}"
        );
    }

    // ── Test: resolve with no bazel and no cache ────────────────────────

    #[test]
    fn test_resolve_no_bazel_no_cache_returns_error() {
        let tmp = TempDir::new().unwrap();
        let config = ResolveConfig {
            project_root: tmp.path().to_path_buf(),
            timeout_secs: 5,
            cache_path: None,
        };

        // This will fail because bazel is not installed in the test environment.
        let result = resolve_bazel_classpath(&config);
        // Should fail (no bazel, no cache).
        assert!(result.is_err());
    }

    // ── Test: cache fallback ────────────────────────────────────────────

    #[test]
    fn test_cache_fallback_loads_cached_classpath() {
        let tmp = TempDir::new().unwrap();
        let cache_path = tmp.path().join("classpath_cache.json");

        // Write a cached classpath.
        let cached = vec![ResolvedClasspath {
            module_name: "cached-project".to_string(),
            module_root: tmp.path().to_path_buf(),
            entries: vec![ClasspathEntry {
                jar_path: PathBuf::from("/cached/guava.jar"),
                coordinates: Some("com.google.guava:guava:33.0.0".to_string()),
                is_direct: false,
                source_jar: None,
            }],
        }];
        std::fs::write(&cache_path, serde_json::to_string(&cached).unwrap()).unwrap();

        let original_error = ClasspathError::ResolutionFailed("bazel not found".to_string());
        let config = ResolveConfig {
            project_root: tmp.path().to_path_buf(),
            timeout_secs: 5,
            cache_path: Some(cache_path),
        };

        let result = try_cache_fallback(&config, &original_error);
        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].module_name, "cached-project");
        assert_eq!(resolved[0].entries.len(), 1);
        assert_eq!(
            resolved[0].entries[0].coordinates,
            Some("com.google.guava:guava:33.0.0".to_string())
        );
    }

    #[test]
    fn test_cache_fallback_missing_cache_file() {
        let tmp = TempDir::new().unwrap();
        let cache_path = tmp.path().join("nonexistent.json");
        let original_error = ClasspathError::ResolutionFailed("bazel not found".to_string());
        let config = ResolveConfig {
            project_root: tmp.path().to_path_buf(),
            timeout_secs: 5,
            cache_path: Some(cache_path),
        };

        let result = try_cache_fallback(&config, &original_error);
        assert!(result.is_err());
    }

    #[test]
    fn test_cache_fallback_no_cache_configured() {
        let original_error = ClasspathError::ResolutionFailed("bazel not found".to_string());
        let config = ResolveConfig {
            project_root: PathBuf::from("/tmp"),
            timeout_secs: 5,
            cache_path: None,
        };

        let result = try_cache_fallback(&config, &original_error);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("no cache available"));
    }

    // ── Test: source JAR discovery ──────────────────────────────────────

    #[test]
    fn test_find_source_jar_same_directory() {
        let tmp = TempDir::new().unwrap();
        let main_jar = tmp.path().join("guava-33.0.0.jar");
        let sources_jar = tmp.path().join("guava-33.0.0-sources.jar");
        std::fs::write(&main_jar, b"").unwrap();
        std::fs::write(&sources_jar, b"").unwrap();

        let result = find_source_jar(&main_jar);
        assert_eq!(result, Some(sources_jar));
    }

    #[test]
    fn test_find_source_jar_not_present() {
        let tmp = TempDir::new().unwrap();
        let main_jar = tmp.path().join("guava-33.0.0.jar");
        std::fs::write(&main_jar, b"").unwrap();

        let result = find_source_jar(&main_jar);
        assert_eq!(result, None);
    }

    // ── Test: build_entries ─────────────────────────────────────────────

    #[test]
    fn test_build_entries_with_coordinates() {
        let jar_paths = vec![
            PathBuf::from("/some/path/guava-33.0.0.jar"),
            PathBuf::from("/some/path/unknown.jar"),
        ];
        let mut coords = CoordinatesMap::new();
        coords.insert(
            "guava-33.0.0.jar".to_string(),
            "com.google.guava:guava:33.0.0".to_string(),
        );

        let entries = build_entries(&jar_paths, &coords);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].coordinates,
            Some("com.google.guava:guava:33.0.0".to_string())
        );
        assert_eq!(entries[1].coordinates, None);
        // All entries from Bazel cquery are transitive.
        assert!(!entries[0].is_direct);
        assert!(!entries[1].is_direct);
    }

    // ── Test: infer_module_name ─────────────────────────────────────────

    #[test]
    fn test_infer_module_name() {
        assert_eq!(
            infer_module_name(Path::new("/home/user/my-project")),
            "my-project"
        );
        assert_eq!(infer_module_name(Path::new("/")), "root");
    }

    // ── Test: coursier source JAR derivation ────────────────────────────

    #[test]
    fn test_find_coursier_source_jar_derivation() {
        let jar = PathBuf::from("/cache/v1/guava-33.0.0.jar");
        let result = find_coursier_source_jar(&jar);
        assert_eq!(
            result,
            Some(PathBuf::from("/cache/v1/guava-33.0.0-sources.jar"))
        );
    }

    #[test]
    fn test_find_coursier_source_jar_already_sources() {
        let jar = PathBuf::from("/cache/v1/guava-33.0.0-sources.jar");
        let result = find_coursier_source_jar(&jar);
        assert_eq!(result, None);
    }
}
