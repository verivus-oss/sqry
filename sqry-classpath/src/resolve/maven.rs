//! Maven classpath resolution.
//!
//! Resolves classpath JARs from Maven projects via `mvn dependency:build-classpath`
//! with fallback to pom.xml parsing when Maven is unavailable.
//!
//! # Strategy
//!
//! 1. Execute `mvn dependency:build-classpath -DincludeScope=compile -Dmdep.outputFile=<temp>`
//! 2. Parse the output file for JAR paths (colon-separated on Unix, semicolon on Windows)
//! 3. On failure/timeout, fall back to pom.xml direct parsing (lossy)
//!
//! # Multi-module
//!
//! Detects child POMs via the `<modules>` element in the root pom.xml and
//! resolves each module independently.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use log::{debug, info, warn};
use serde::{Deserialize, Serialize};

use super::{ClasspathEntry, ResolveConfig, ResolvedClasspath};
use crate::{ClasspathError, ClasspathResult};

/// Cache file name for Maven-specific resolved classpath.
const MAVEN_CACHE_FILE: &str = "maven-resolved-classpath.json";

/// Platform-specific path separator for classpath strings.
#[cfg(unix)]
const CLASSPATH_SEPARATOR: char = ':';

/// Platform-specific path separator for classpath strings.
#[cfg(windows)]
const CLASSPATH_SEPARATOR: char = ';';

/// Resolve classpath for a Maven project.
///
/// Strategy:
/// 1. Execute `mvn dependency:build-classpath -DincludeScope=compile -Dmdep.outputFile=<temp>`
/// 2. Parse the output file for JAR paths (colon-separated on Unix, semicolon on Windows)
/// 3. On failure/timeout, fall back to pom.xml direct parsing (lossy)
///
/// Multi-module: detects child POMs and resolves per-module.
#[allow(clippy::missing_errors_doc)] // Internal helper
pub fn resolve_maven_classpath(config: &ResolveConfig) -> ClasspathResult<Vec<ResolvedClasspath>> {
    let pom_path = config.project_root.join("pom.xml");
    if !pom_path.exists() {
        return Err(ClasspathError::ResolutionFailed(
            "pom.xml not found in project root".to_string(),
        ));
    }

    let modules = detect_modules(&pom_path);
    let maven_repo = default_maven_repo();

    if modules.is_empty() {
        resolve_single_project(config, &maven_repo)
    } else {
        resolve_multi_module(config, &modules, &maven_repo)
    }
}

/// Resolve a single-module Maven project.
fn resolve_single_project(
    config: &ResolveConfig,
    maven_repo: &Path,
) -> ClasspathResult<Vec<ResolvedClasspath>> {
    match resolve_via_subprocess(&config.project_root, config.timeout_secs, maven_repo) {
        Ok(resolved) => {
            write_maven_cache(config, std::slice::from_ref(&resolved));
            Ok(vec![resolved])
        }
        Err(e) => {
            warn!("Maven subprocess resolution failed: {e}");
            try_cache_or_error(
                config,
                &[ModuleInfo::root(&config.project_root)],
                &e.to_string(),
            )
        }
    }
}

/// Resolve a multi-module Maven project.
fn resolve_multi_module(
    config: &ResolveConfig,
    modules: &[String],
    maven_repo: &Path,
) -> ClasspathResult<Vec<ResolvedClasspath>> {
    let module_infos: Vec<ModuleInfo> = modules
        .iter()
        .map(|m| ModuleInfo {
            name: m.clone(),
            root: config.project_root.join(m),
        })
        .collect();

    let mut results = Vec::new();
    let mut failed_modules = 0usize;

    for info in &module_infos {
        if !info.root.join("pom.xml").exists() {
            warn!("Module '{}' has no pom.xml, skipping", info.name);
            continue;
        }
        match resolve_module_via_subprocess(info, config.timeout_secs, maven_repo) {
            Ok(resolved) => results.push(resolved),
            Err(e) => {
                warn!("Maven resolution failed for module '{}': {e}", info.name);
                failed_modules += 1;
            }
        }
    }

    if failed_modules > 0 && results.is_empty() {
        return try_cache_or_error(
            config,
            &module_infos,
            "All Maven module subprocess resolutions failed",
        );
    }

    if failed_modules > 0 {
        warn!(
            "Maven resolution incomplete: {failed_modules}/{} modules failed; using partial classpath result",
            module_infos.len()
        );
    }

    if !results.is_empty() {
        write_maven_cache(config, &results);
    }
    Ok(results)
}

/// Information about a Maven module.
struct ModuleInfo {
    name: String,
    root: PathBuf,
}

impl ModuleInfo {
    fn root(project_root: &Path) -> Self {
        Self {
            name: String::new(),
            root: project_root.to_path_buf(),
        }
    }
}

/// Resolve a single module by invoking `mvn dependency:build-classpath`.
fn resolve_via_subprocess(
    module_root: &Path,
    timeout_secs: u64,
    maven_repo: &Path,
) -> ClasspathResult<ResolvedClasspath> {
    let classpath_output = run_maven_build_classpath(module_root, timeout_secs)?;
    let entries = parse_classpath_string(&classpath_output, maven_repo);

    Ok(ResolvedClasspath {
        module_name: String::new(),
        module_root: module_root.to_path_buf(),
        entries,
    })
}

/// Resolve a named module by invoking `mvn dependency:build-classpath`.
fn resolve_module_via_subprocess(
    info: &ModuleInfo,
    timeout_secs: u64,
    maven_repo: &Path,
) -> ClasspathResult<ResolvedClasspath> {
    let classpath_output = run_maven_build_classpath(&info.root, timeout_secs)?;
    let entries = parse_classpath_string(&classpath_output, maven_repo);

    Ok(ResolvedClasspath {
        module_name: info.name.clone(),
        module_root: info.root.clone(),
        entries,
    })
}

/// Execute `mvn dependency:build-classpath` and return the classpath string.
///
/// Writes output to a temporary file, reads it back, and cleans up.
fn run_maven_build_classpath(working_dir: &Path, timeout_secs: u64) -> ClasspathResult<String> {
    let temp_dir = tempfile::tempdir()
        .map_err(|e| ClasspathError::ResolutionFailed(format!("tempdir: {e}")))?;
    let output_file = temp_dir.path().join("classpath.txt");

    let mvn_cmd = find_mvn_command(working_dir).ok_or_else(|| {
        ClasspathError::ResolutionFailed("No Maven wrapper or installed mvn found".to_string())
    })?;

    let mut command = Command::new(&mvn_cmd);
    command
        .arg("dependency:build-classpath")
        .arg("-DincludeScope=compile")
        .arg(format!("-Dmdep.outputFile={}", output_file.display()))
        .arg("-q")
        .arg("--batch-mode")
        .current_dir(working_dir);

    debug!(
        "Running Maven: {} dependency:build-classpath in {}",
        mvn_cmd.display(),
        working_dir.display()
    );

    let output = run_command_with_timeout(&mut command, Duration::from_secs(timeout_secs))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ClasspathError::ResolutionFailed(format!(
            "mvn dependency:build-classpath failed (exit {}): {}",
            output.status,
            stderr.chars().take(500).collect::<String>()
        )));
    }

    // Read the output file.
    let classpath = std::fs::read_to_string(&output_file).map_err(|e| {
        ClasspathError::ResolutionFailed(format!(
            "Failed to read Maven classpath output file {}: {e}",
            output_file.display()
        ))
    })?;

    Ok(classpath.trim().to_string())
}

/// Find the Maven command to use.
///
/// Prefers `./mvnw` (Maven wrapper) if present, otherwise falls back to `mvn`.
fn find_mvn_command(working_dir: &Path) -> Option<PathBuf> {
    #[cfg(unix)]
    let wrapper = working_dir.join("mvnw");
    #[cfg(windows)]
    let wrapper = working_dir.join("mvnw.cmd");

    if wrapper.exists() {
        Some(wrapper)
    } else {
        which_binary(if cfg!(windows) { "mvn.cmd" } else { "mvn" })
    }
}

/// Run a command with a timeout.
///
/// Returns the process output or an error if the timeout is exceeded or
/// the process cannot be spawned (e.g., `mvn` not found).
fn run_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> ClasspathResult<std::process::Output> {
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ClasspathError::ResolutionFailed(format!("Failed to spawn mvn: {e}")))?;

    let start = std::time::Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return collect_child_output(child, status);
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ClasspathError::ResolutionFailed(format!(
                        "mvn timed out after {}s",
                        timeout.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(ClasspathError::ResolutionFailed(format!(
                    "Failed to check mvn process status: {e}"
                )));
            }
        }
    }
}

/// Collect stdout and stderr from a finished child process.
#[allow(clippy::unnecessary_wraps)] // Result for API consistency
fn collect_child_output(
    mut child: std::process::Child,
    status: std::process::ExitStatus,
) -> ClasspathResult<std::process::Output> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(ref mut out) = child.stdout {
        let _ = out.read_to_end(&mut stdout);
    }
    if let Some(ref mut err) = child.stderr {
        let _ = err.read_to_end(&mut stderr);
    }
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// Parse a classpath string (colon-separated on Unix, semicolon on Windows)
/// into `ClasspathEntry` instances.
///
/// Extracts Maven coordinates from the local repository path structure and
/// checks for corresponding source JARs.
#[must_use]
pub fn parse_classpath_string(classpath: &str, maven_repo: &Path) -> Vec<ClasspathEntry> {
    if classpath.is_empty() {
        return Vec::new();
    }

    classpath
        .split(CLASSPATH_SEPARATOR)
        .filter(|p| !p.is_empty())
        .map(|p| {
            let jar_path = PathBuf::from(p.trim());
            let coordinate = extract_coordinates_from_repo_path(&jar_path, maven_repo);
            let source_jar = find_source_jar(&jar_path);
            ClasspathEntry {
                jar_path,
                coordinates: coordinate,
                is_direct: true, // Maven build-classpath does not distinguish; mark all as direct.
                source_jar,
            }
        })
        .collect()
}

/// Extract Maven coordinates (`groupId:artifactId:version`) from a path
/// within the Maven local repository.
///
/// Maven stores JARs at:
/// `~/.m2/repository/<group-path>/<artifact>/<version>/<artifact>-<version>.jar`
///
/// Example:
/// `~/.m2/repository/com/google/guava/guava/33.0.0/guava-33.0.0.jar`
/// yields `com.google.guava:guava:33.0.0`.
#[must_use]
pub fn extract_coordinates_from_repo_path(jar_path: &Path, maven_repo: &Path) -> Option<String> {
    let jar_path_str = normalize_path(jar_path);
    let repo_str = normalize_path(maven_repo);

    // Check that the JAR is actually within the Maven repository.
    let relative = jar_path_str
        .strip_prefix(&repo_str)?
        .trim_start_matches('/');
    if relative.is_empty() {
        return None;
    }

    let parts: Vec<&str> = relative.split('/').collect();
    // Minimum: group(1+) / artifact / version / filename = 4 parts
    if parts.len() < 4 {
        return None;
    }

    // Last part is the filename, second-to-last is version, third-to-last is artifact.
    let version = parts[parts.len() - 2];
    let artifact_id = parts[parts.len() - 3];
    let group_parts = &parts[..parts.len() - 3];

    if group_parts.is_empty() {
        return None;
    }

    let group_id = group_parts.join(".");
    Some(format!("{group_id}:{artifact_id}:{version}"))
}

/// Normalize a path to a forward-slash string for comparison.
fn normalize_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Look for a source JAR alongside the binary JAR.
///
/// Maven source JARs use the pattern: `<artifact>-<version>-sources.jar`
/// in the same directory as the binary JAR.
#[allow(clippy::case_sensitive_file_extension_comparisons)] // Known file extensions in domain
fn find_source_jar(jar_path: &Path) -> Option<PathBuf> {
    let file_name = jar_path.file_name()?.to_str()?;
    if !file_name.ends_with(".jar") {
        return None;
    }

    let stem = file_name.strip_suffix(".jar")?;
    let source_name = format!("{stem}-sources.jar");
    let source_path = jar_path.with_file_name(source_name);

    if source_path.exists() {
        Some(source_path)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// POM.xml Fallback (Lossy)
// ---------------------------------------------------------------------------

/// A dependency extracted from pom.xml via simple parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PomDependency {
    /// Maven group ID.
    pub group_id: String,
    /// Maven artifact ID.
    pub artifact_id: String,
    /// Version string, if specified (may be absent when managed by parent).
    pub version: Option<String>,
    /// Dependency scope (compile, test, provided, runtime, system).
    pub scope: Option<String>,
}

/// Parse dependencies from a pom.xml file using simple string matching.
///
/// This is intentionally lossy: it does not resolve properties (`${...}`),
/// parent POMs, dependency management, or transitive dependencies. It exists
/// only as a fallback when `mvn` is not available.
#[must_use]
pub fn parse_pom_dependencies(pom_content: &str) -> Vec<PomDependency> {
    let mut deps = Vec::new();

    let mut search_from = 0;
    loop {
        let Some(start) = pom_content[search_from..].find("<dependency>") else {
            break;
        };
        let abs_start = search_from + start;

        let Some(end) = pom_content[abs_start..].find("</dependency>") else {
            break;
        };
        let abs_end = abs_start + end + "</dependency>".len();

        let block = &pom_content[abs_start..abs_end];
        search_from = abs_end;

        let Some(group_id) = extract_xml_element(block, "groupId") else {
            continue;
        };
        let Some(artifact_id) = extract_xml_element(block, "artifactId") else {
            continue;
        };

        // Skip property references we cannot resolve.
        if group_id.contains("${") || artifact_id.contains("${") {
            continue;
        }

        let version = extract_xml_element(block, "version");
        let scope = extract_xml_element(block, "scope");

        // Skip test-scoped dependencies.
        if scope.as_deref() == Some("test") {
            continue;
        }

        deps.push(PomDependency {
            group_id,
            artifact_id,
            version,
            scope,
        });
    }

    deps
}

/// Extract the text content of a simple XML element.
///
/// Looks for `<tag>content</tag>` and returns `content` (trimmed).
/// Does not handle attributes, CDATA, namespaces, or nested elements.
fn extract_xml_element(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)?;
    let content_start = start + open.len();
    let end = xml[content_start..].find(&close)?;
    let content = xml[content_start..content_start + end].trim();
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

/// Resolve classpath from pom.xml fallback (lossy).
///
/// Parses pom.xml directly and attempts to locate JARs in the Maven local
/// repository. This is lossy because it does not resolve:
/// - Transitive dependencies
/// - Property placeholders (`${...}`)
/// - Parent POM inheritance
/// - Dependency management version overrides
#[cfg(test)]
fn resolve_from_pom_fallback(
    module_root: &Path,
    module_name: &str,
    maven_repo: &Path,
) -> ClasspathResult<ResolvedClasspath> {
    let pom_path = module_root.join("pom.xml");
    let pom_content = std::fs::read_to_string(&pom_path).map_err(|e| {
        ClasspathError::ResolutionFailed(format!(
            "Failed to read pom.xml at {}: {e}",
            pom_path.display()
        ))
    })?;

    let deps = parse_pom_dependencies(&pom_content);
    let mut entries = Vec::new();

    let display_name = if module_name.is_empty() {
        "<root>"
    } else {
        module_name
    };

    for dep in &deps {
        let Some(version) = &dep.version else {
            warn!(
                "Skipping {}:{} in {} — no version (may be from dependencyManagement)",
                dep.group_id, dep.artifact_id, display_name
            );
            continue;
        };

        // Skip property references we cannot resolve.
        if version.contains("${") {
            warn!(
                "Skipping {}:{}:{} in {} — version contains property placeholder",
                dep.group_id, dep.artifact_id, version, display_name
            );
            continue;
        }

        let jar_path =
            construct_maven_jar_path(maven_repo, &dep.group_id, &dep.artifact_id, version);

        if jar_path.exists() {
            let source_jar = find_source_jar(&jar_path);
            let coordinates = format!("{}:{}:{}", dep.group_id, dep.artifact_id, version);
            entries.push(ClasspathEntry {
                jar_path,
                coordinates: Some(coordinates),
                is_direct: true,
                source_jar,
            });
        } else {
            warn!(
                "JAR not found in local repo for {}: {}:{}:{} (expected at {})",
                display_name,
                dep.group_id,
                dep.artifact_id,
                version,
                jar_path.display()
            );
        }
    }

    info!(
        "POM fallback for '{}': {} entries resolved",
        display_name,
        entries.len()
    );

    Ok(ResolvedClasspath {
        module_name: module_name.to_string(),
        module_root: module_root.to_path_buf(),
        entries,
    })
}

/// Construct the expected JAR path in the Maven local repository.
///
/// Format: `<repo>/<group-path>/<artifact>/<version>/<artifact>-<version>.jar`
#[must_use]
pub fn construct_maven_jar_path(
    maven_repo: &Path,
    group_id: &str,
    artifact_id: &str,
    version: &str,
) -> PathBuf {
    let group_path = group_id.replace('.', "/");
    maven_repo
        .join(group_path)
        .join(artifact_id)
        .join(version)
        .join(format!("{artifact_id}-{version}.jar"))
}

// ---------------------------------------------------------------------------
// Multi-module detection
// ---------------------------------------------------------------------------

/// Detect child modules from a pom.xml's `<modules>` element.
///
/// Returns a list of module directory names (relative to the POM's directory).
#[must_use]
pub fn detect_modules(pom_path: &Path) -> Vec<String> {
    let content = match std::fs::read_to_string(pom_path) {
        Ok(c) => c,
        Err(e) => {
            warn!("Could not read pom.xml at {}: {e}", pom_path.display());
            return Vec::new();
        }
    };

    // Find <modules>...</modules> block.
    let Some(modules_start) = content.find("<modules>") else {
        return Vec::new();
    };
    let Some(modules_end) = content[modules_start..].find("</modules>") else {
        return Vec::new();
    };
    let modules_block = &content[modules_start..modules_start + modules_end];

    // Extract each <module>name</module>.
    let mut modules = Vec::new();
    let mut search_from = 0;
    loop {
        let Some(start) = modules_block[search_from..].find("<module>") else {
            break;
        };
        let abs_start = search_from + start + "<module>".len();
        let Some(end) = modules_block[abs_start..].find("</module>") else {
            break;
        };
        let module_name = modules_block[abs_start..abs_start + end].trim();
        if !module_name.is_empty() {
            modules.push(module_name.to_string());
        }
        search_from = abs_start + end + "</module>".len();
    }

    debug!("Detected Maven modules: {modules:?}");
    modules
}

// ---------------------------------------------------------------------------
// Cache helpers
// ---------------------------------------------------------------------------

/// Write Maven-specific cache.
fn write_maven_cache(config: &ResolveConfig, entries: &[ResolvedClasspath]) {
    let dir = cache_dir(config);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!("Could not create Maven cache dir: {e}");
        return;
    }
    let cache_path = dir.join(MAVEN_CACHE_FILE);
    match serde_json::to_string_pretty(entries) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&cache_path, &json) {
                warn!("Could not write Maven cache: {e}");
            } else {
                debug!("Wrote Maven cache to {}", cache_path.display());
            }
        }
        Err(e) => warn!("Could not serialize Maven cache: {e}"),
    }
}

/// Read Maven-specific cache.
fn read_maven_cache(config: &ResolveConfig) -> Option<Vec<ResolvedClasspath>> {
    let cache_path = cache_dir(config).join(MAVEN_CACHE_FILE);
    let data = std::fs::read_to_string(&cache_path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Compute the cache directory path.
fn cache_dir(config: &ResolveConfig) -> PathBuf {
    config
        .cache_path
        .clone()
        .unwrap_or_else(|| config.project_root.join(".sqry").join("classpath"))
}

/// Try cache first, then POM fallback.
#[allow(clippy::unnecessary_wraps)] // Result for API consistency
fn try_cache_or_error(
    config: &ResolveConfig,
    module_infos: &[ModuleInfo],
    live_error: &str,
) -> ClasspathResult<Vec<ResolvedClasspath>> {
    // Try cache.
    if let Some(cached) = read_maven_cache(config) {
        info!("Using cached Maven classpath ({} modules)", cached.len());
        warn_if_cache_stale(config, &cached);
        return Ok(cached);
    }

    let module_summary = module_infos
        .iter()
        .map(|info| {
            if info.name.is_empty() {
                info.root.display().to_string()
            } else {
                format!("{} ({})", info.name, info.root.display())
            }
        })
        .collect::<Vec<_>>()
        .join(", ");

    Err(ClasspathError::ResolutionFailed(format!(
        "{live_error}. No Maven cache available for [{module_summary}]. Provide mvnw, install mvn, or use --classpath-file."
    )))
}

/// Get the default Maven local repository path.
fn default_maven_repo() -> PathBuf {
    #[cfg(unix)]
    let home = std::env::var_os("HOME").map(PathBuf::from);
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    #[cfg(not(any(unix, windows)))]
    let home: Option<PathBuf> = None;

    home.map_or_else(
        || PathBuf::from(".m2").join("repository"),
        |h| h.join(".m2").join("repository"),
    )
}

fn warn_if_cache_stale(config: &ResolveConfig, classpaths: &[ResolvedClasspath]) {
    if classpaths.is_empty() {
        return;
    }
    let cache_path = cache_dir(config).join(MAVEN_CACHE_FILE);
    let Ok(cache_meta) = std::fs::metadata(&cache_path) else {
        return;
    };
    let Ok(cache_mtime) = cache_meta.modified() else {
        return;
    };

    for cp in classpaths {
        let pom_path = cp.module_root.join("pom.xml");
        let Ok(meta) = std::fs::metadata(&pom_path) else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if modified > cache_mtime {
            warn!(
                "Using cached Maven classpath from {} even though {} is newer; cache may be stale",
                cache_path.display(),
                pom_path.display()
            );
            return;
        }
    }
}

fn which_binary(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // 1. Parse colon-separated classpath output
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_classpath_string_basic() {
        let repo = PathBuf::from("/home/user/.m2/repository");
        let classpath = "/home/user/.m2/repository/com/google/guava/guava/33.0.0/guava-33.0.0.jar\
            :/home/user/.m2/repository/org/slf4j/slf4j-api/2.0.9/slf4j-api-2.0.9.jar";

        let entries = parse_classpath_string(classpath, &repo);
        assert_eq!(entries.len(), 2);

        assert_eq!(
            entries[0].jar_path,
            PathBuf::from(
                "/home/user/.m2/repository/com/google/guava/guava/33.0.0/guava-33.0.0.jar"
            )
        );
        assert_eq!(
            entries[0].coordinates.as_deref(),
            Some("com.google.guava:guava:33.0.0")
        );

        assert_eq!(
            entries[1].jar_path,
            PathBuf::from(
                "/home/user/.m2/repository/org/slf4j/slf4j-api/2.0.9/slf4j-api-2.0.9.jar"
            )
        );
        assert_eq!(
            entries[1].coordinates.as_deref(),
            Some("org.slf4j:slf4j-api:2.0.9")
        );
    }

    #[test]
    fn test_parse_classpath_string_empty() {
        let repo = PathBuf::from("/home/user/.m2/repository");
        let entries = parse_classpath_string("", &repo);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_classpath_string_single_entry() {
        let repo = PathBuf::from("/home/user/.m2/repository");
        let classpath = "/home/user/.m2/repository/junit/junit/4.13.2/junit-4.13.2.jar";
        let entries = parse_classpath_string(classpath, &repo);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].coordinates.as_deref(),
            Some("junit:junit:4.13.2")
        );
    }

    #[test]
    fn test_parse_classpath_non_repo_path() {
        let repo = PathBuf::from("/home/user/.m2/repository");
        let classpath = "/opt/custom/lib/some.jar";
        let entries = parse_classpath_string(classpath, &repo);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].jar_path,
            PathBuf::from("/opt/custom/lib/some.jar")
        );
        assert!(entries[0].coordinates.is_none());
    }

    // -----------------------------------------------------------------------
    // 2. Extract coordinates from Maven repo path
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_coordinates_guava() {
        let repo = PathBuf::from("/home/user/.m2/repository");
        let jar = PathBuf::from(
            "/home/user/.m2/repository/com/google/guava/guava/33.0.0/guava-33.0.0.jar",
        );
        let coords = extract_coordinates_from_repo_path(&jar, &repo);
        assert_eq!(coords.as_deref(), Some("com.google.guava:guava:33.0.0"));
    }

    #[test]
    fn test_extract_coordinates_simple_group() {
        let repo = PathBuf::from("/home/user/.m2/repository");
        let jar = PathBuf::from("/home/user/.m2/repository/junit/junit/4.13.2/junit-4.13.2.jar");
        let coords = extract_coordinates_from_repo_path(&jar, &repo);
        assert_eq!(coords.as_deref(), Some("junit:junit:4.13.2"));
    }

    #[test]
    fn test_extract_coordinates_outside_repo() {
        let repo = PathBuf::from("/home/user/.m2/repository");
        let jar = PathBuf::from("/opt/lib/foo.jar");
        let coords = extract_coordinates_from_repo_path(&jar, &repo);
        assert!(coords.is_none());
    }

    #[test]
    fn test_extract_coordinates_too_short_path() {
        let repo = PathBuf::from("/home/user/.m2/repository");
        let jar = PathBuf::from("/home/user/.m2/repository/foo/bar");
        // Only 2 parts (foo/bar), need at least 4: group/artifact/version/file
        let coords = extract_coordinates_from_repo_path(&jar, &repo);
        assert!(coords.is_none());
    }

    #[test]
    fn test_extract_coordinates_deep_group() {
        let repo = PathBuf::from("/home/user/.m2/repository");
        let jar = PathBuf::from(
            "/home/user/.m2/repository/org/apache/commons/commons-lang3/3.14.0/commons-lang3-3.14.0.jar",
        );
        let coords = extract_coordinates_from_repo_path(&jar, &repo);
        assert_eq!(
            coords.as_deref(),
            Some("org.apache.commons:commons-lang3:3.14.0")
        );
    }

    #[test]
    fn test_extract_coordinates_repo_root_itself() {
        let repo = PathBuf::from("/home/user/.m2/repository");
        let jar = PathBuf::from("/home/user/.m2/repository");
        let coords = extract_coordinates_from_repo_path(&jar, &repo);
        assert!(coords.is_none());
    }

    // -----------------------------------------------------------------------
    // 3. Multi-module POM detection
    // -----------------------------------------------------------------------

    #[test]
    fn test_detect_modules_multi() {
        let tmp = TempDir::new().unwrap();
        let pom = r#"<?xml version="1.0" encoding="UTF-8"?>
<project>
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example</groupId>
  <artifactId>parent</artifactId>
  <version>1.0.0</version>
  <packaging>pom</packaging>
  <modules>
    <module>core</module>
    <module>web</module>
    <module>api</module>
  </modules>
</project>"#;
        let pom_path = tmp.path().join("pom.xml");
        std::fs::write(&pom_path, pom).unwrap();

        let modules = detect_modules(&pom_path);
        assert_eq!(modules, vec!["core", "web", "api"]);
    }

    #[test]
    fn test_detect_modules_none() {
        let tmp = TempDir::new().unwrap();
        let pom = r#"<?xml version="1.0"?>
<project>
  <groupId>com.example</groupId>
  <artifactId>single</artifactId>
  <version>1.0.0</version>
</project>"#;
        let pom_path = tmp.path().join("pom.xml");
        std::fs::write(&pom_path, pom).unwrap();

        let modules = detect_modules(&pom_path);
        assert!(modules.is_empty());
    }

    #[test]
    fn test_detect_modules_missing_pom() {
        let modules = detect_modules(Path::new("/nonexistent/pom.xml"));
        assert!(modules.is_empty());
    }

    #[test]
    fn test_detect_modules_whitespace_handling() {
        let tmp = TempDir::new().unwrap();
        let pom = r"<project>
  <modules>
    <module>  core  </module>
    <module>
      api
    </module>
  </modules>
</project>";
        let pom_path = tmp.path().join("pom.xml");
        std::fs::write(&pom_path, pom).unwrap();

        let modules = detect_modules(&pom_path);
        assert_eq!(modules, vec!["core", "api"]);
    }

    // -----------------------------------------------------------------------
    // 4. Offline fallback — construct paths in local repo
    // -----------------------------------------------------------------------

    #[test]
    fn test_construct_maven_jar_path() {
        let repo = PathBuf::from("/home/user/.m2/repository");
        let path = construct_maven_jar_path(&repo, "com.google.guava", "guava", "33.0.0");
        assert_eq!(
            path,
            PathBuf::from(
                "/home/user/.m2/repository/com/google/guava/guava/33.0.0/guava-33.0.0.jar"
            )
        );
    }

    #[test]
    fn test_construct_maven_jar_path_simple_group() {
        let repo = PathBuf::from("/home/user/.m2/repository");
        let path = construct_maven_jar_path(&repo, "junit", "junit", "4.13.2");
        assert_eq!(
            path,
            PathBuf::from("/home/user/.m2/repository/junit/junit/4.13.2/junit-4.13.2.jar")
        );
    }

    // -----------------------------------------------------------------------
    // 5. POM.xml dependency parsing
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_pom_dependencies_basic() {
        let pom = r"<project>
  <dependencies>
    <dependency>
      <groupId>com.google.guava</groupId>
      <artifactId>guava</artifactId>
      <version>33.0.0</version>
    </dependency>
    <dependency>
      <groupId>org.slf4j</groupId>
      <artifactId>slf4j-api</artifactId>
      <version>2.0.9</version>
    </dependency>
  </dependencies>
</project>";

        let deps = parse_pom_dependencies(pom);
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].group_id, "com.google.guava");
        assert_eq!(deps[0].artifact_id, "guava");
        assert_eq!(deps[0].version.as_deref(), Some("33.0.0"));
        assert_eq!(deps[1].group_id, "org.slf4j");
        assert_eq!(deps[1].artifact_id, "slf4j-api");
        assert_eq!(deps[1].version.as_deref(), Some("2.0.9"));
    }

    #[test]
    fn test_parse_pom_dependencies_skips_test_scope() {
        let pom = r"<project>
  <dependencies>
    <dependency>
      <groupId>com.google.guava</groupId>
      <artifactId>guava</artifactId>
      <version>33.0.0</version>
    </dependency>
    <dependency>
      <groupId>junit</groupId>
      <artifactId>junit</artifactId>
      <version>4.13.2</version>
      <scope>test</scope>
    </dependency>
  </dependencies>
</project>";

        let deps = parse_pom_dependencies(pom);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].artifact_id, "guava");
    }

    #[test]
    fn test_parse_pom_dependencies_skips_property_placeholders() {
        let pom = r"<project>
  <dependencies>
    <dependency>
      <groupId>${project.groupId}</groupId>
      <artifactId>internal-lib</artifactId>
      <version>1.0.0</version>
    </dependency>
    <dependency>
      <groupId>org.example</groupId>
      <artifactId>real-dep</artifactId>
      <version>2.0.0</version>
    </dependency>
  </dependencies>
</project>";

        let deps = parse_pom_dependencies(pom);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].group_id, "org.example");
    }

    #[test]
    fn test_parse_pom_dependencies_no_version() {
        let pom = r"<project>
  <dependencies>
    <dependency>
      <groupId>org.example</groupId>
      <artifactId>managed-dep</artifactId>
    </dependency>
  </dependencies>
</project>";

        let deps = parse_pom_dependencies(pom);
        assert_eq!(deps.len(), 1);
        assert!(deps[0].version.is_none());
    }

    #[test]
    fn test_parse_pom_dependencies_with_compile_scope() {
        let pom = r"<project>
  <dependencies>
    <dependency>
      <groupId>org.example</groupId>
      <artifactId>dep</artifactId>
      <version>1.0</version>
      <scope>compile</scope>
    </dependency>
  </dependencies>
</project>";

        let deps = parse_pom_dependencies(pom);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].scope.as_deref(), Some("compile"));
    }

    #[test]
    fn test_parse_pom_empty_dependencies() {
        let pom = r"<project>
  <dependencies>
  </dependencies>
</project>";

        let deps = parse_pom_dependencies(pom);
        assert!(deps.is_empty());
    }

    #[test]
    fn test_parse_pom_no_dependencies_element() {
        let pom = r"<project>
  <groupId>com.example</groupId>
</project>";

        let deps = parse_pom_dependencies(pom);
        assert!(deps.is_empty());
    }

    // -----------------------------------------------------------------------
    // 6. Malformed output handled gracefully
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_classpath_string_trailing_separator() {
        let repo = PathBuf::from("/home/user/.m2/repository");
        let classpath = "/home/user/.m2/repository/junit/junit/4.13.2/junit-4.13.2.jar:";
        let entries = parse_classpath_string(classpath, &repo);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_parse_classpath_string_leading_separator() {
        let repo = PathBuf::from("/home/user/.m2/repository");
        let classpath = ":/home/user/.m2/repository/junit/junit/4.13.2/junit-4.13.2.jar";
        let entries = parse_classpath_string(classpath, &repo);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_parse_classpath_string_double_separator() {
        let repo = PathBuf::from("/home/user/.m2/repository");
        let classpath = "/a.jar::/b.jar";
        let entries = parse_classpath_string(classpath, &repo);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_extract_xml_element_missing() {
        let xml = "<dependency><groupId>g</groupId></dependency>";
        assert!(extract_xml_element(xml, "artifactId").is_none());
    }

    #[test]
    fn test_extract_xml_element_empty() {
        let xml = "<dependency><groupId></groupId></dependency>";
        assert!(extract_xml_element(xml, "groupId").is_none());
    }

    // -----------------------------------------------------------------------
    // 7. Cache roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn test_cache_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let config = ResolveConfig {
            project_root: tmp.path().to_path_buf(),
            timeout_secs: 60,
            cache_path: Some(tmp.path().join("cache")),
        };

        let entries = vec![ResolvedClasspath {
            module_name: "core".to_string(),
            module_root: tmp.path().join("core"),
            entries: vec![ClasspathEntry {
                jar_path: PathBuf::from("/repo/guava/guava/33.0.0/guava-33.0.0.jar"),
                coordinates: Some("com.google.guava:guava:33.0.0".to_string()),
                is_direct: true,
                source_jar: None,
            }],
        }];

        write_maven_cache(&config, &entries);
        let loaded = read_maven_cache(&config);
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].module_name, "core");
        assert_eq!(loaded[0].entries.len(), 1);
        assert_eq!(
            loaded[0].entries[0].coordinates.as_deref(),
            Some("com.google.guava:guava:33.0.0")
        );
    }

    #[test]
    fn test_cache_read_missing_returns_none() {
        let tmp = TempDir::new().unwrap();
        let config = ResolveConfig {
            project_root: tmp.path().to_path_buf(),
            timeout_secs: 60,
            cache_path: Some(tmp.path().join("nonexistent-cache")),
        };
        assert!(read_maven_cache(&config).is_none());
    }

    // -----------------------------------------------------------------------
    // Source JAR discovery
    // -----------------------------------------------------------------------

    #[test]
    fn test_source_jar_found() {
        let tmp = TempDir::new().unwrap();
        let jar = tmp.path().join("guava-33.0.0.jar");
        let source = tmp.path().join("guava-33.0.0-sources.jar");
        std::fs::write(&jar, b"").unwrap();
        std::fs::write(&source, b"").unwrap();

        let result = find_source_jar(&jar);
        assert_eq!(result, Some(source));
    }

    #[test]
    fn test_source_jar_not_present() {
        let tmp = TempDir::new().unwrap();
        let jar = tmp.path().join("guava-33.0.0.jar");
        std::fs::write(&jar, b"").unwrap();

        let result = find_source_jar(&jar);
        assert!(result.is_none());
    }

    #[test]
    fn test_source_jar_non_jar_file() {
        let result = find_source_jar(Path::new("/some/file.txt"));
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // POM fallback integration
    // -----------------------------------------------------------------------

    #[test]
    fn test_resolve_from_pom_fallback_with_local_jars() {
        let tmp = TempDir::new().unwrap();

        // Set up a fake Maven repo with one JAR.
        let repo = tmp.path().join("repo");
        let jar_dir = repo.join("com/example/mylib/1.0.0");
        std::fs::create_dir_all(&jar_dir).unwrap();
        std::fs::write(jar_dir.join("mylib-1.0.0.jar"), b"fake jar").unwrap();

        // Create a pom.xml referencing the dependency.
        let pom = r"<project>
  <dependencies>
    <dependency>
      <groupId>com.example</groupId>
      <artifactId>mylib</artifactId>
      <version>1.0.0</version>
    </dependency>
    <dependency>
      <groupId>com.missing</groupId>
      <artifactId>nolib</artifactId>
      <version>2.0.0</version>
    </dependency>
  </dependencies>
</project>";
        std::fs::write(tmp.path().join("pom.xml"), pom).unwrap();

        let result = resolve_from_pom_fallback(tmp.path(), "", &repo).unwrap();
        // Should find mylib but not nolib.
        assert_eq!(result.entries.len(), 1);
        assert_eq!(
            result.entries[0].coordinates.as_deref(),
            Some("com.example:mylib:1.0.0")
        );
    }

    // -----------------------------------------------------------------------
    // Integration: resolve_maven_classpath (no real mvn)
    // -----------------------------------------------------------------------

    #[test]
    fn test_resolve_maven_classpath_no_pom() {
        let tmp = TempDir::new().unwrap();
        let config = ResolveConfig {
            project_root: tmp.path().to_path_buf(),
            timeout_secs: 10,
            cache_path: None,
        };

        let result = resolve_maven_classpath(&config);
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_maven_classpath_errors_when_mvn_missing_and_no_cache() {
        let tmp = TempDir::new().unwrap();

        // Create a pom.xml with a dependency.
        let pom = r"<project>
  <dependencies>
    <dependency>
      <groupId>org.example</groupId>
      <artifactId>dep</artifactId>
      <version>1.0.0</version>
    </dependency>
  </dependencies>
</project>";
        std::fs::write(tmp.path().join("pom.xml"), pom).unwrap();

        let config = ResolveConfig {
            project_root: tmp.path().to_path_buf(),
            timeout_secs: 5,
            cache_path: None,
        };

        let result = resolve_maven_classpath(&config);
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_maven_classpath_multimodule_errors_without_tooling() {
        let tmp = TempDir::new().unwrap();

        // Create root pom with modules.
        let root_pom = r"<project>
  <modules>
    <module>core</module>
    <module>web</module>
  </modules>
</project>";
        std::fs::write(tmp.path().join("pom.xml"), root_pom).unwrap();

        // Create module directories with their own poms.
        let core_dir = tmp.path().join("core");
        std::fs::create_dir_all(&core_dir).unwrap();
        std::fs::write(
            core_dir.join("pom.xml"),
            r"<project>
  <dependencies>
    <dependency>
      <groupId>org.example</groupId>
      <artifactId>core-dep</artifactId>
      <version>1.0.0</version>
    </dependency>
  </dependencies>
</project>",
        )
        .unwrap();

        let web_dir = tmp.path().join("web");
        std::fs::create_dir_all(&web_dir).unwrap();
        std::fs::write(
            web_dir.join("pom.xml"),
            r"<project>
  <dependencies>
    <dependency>
      <groupId>org.example</groupId>
      <artifactId>web-dep</artifactId>
      <version>2.0.0</version>
    </dependency>
  </dependencies>
</project>",
        )
        .unwrap();

        let config = ResolveConfig {
            project_root: tmp.path().to_path_buf(),
            timeout_secs: 5,
            cache_path: None,
        };

        let result = resolve_maven_classpath(&config);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // Maven wrapper detection
    // -----------------------------------------------------------------------

    #[test]
    fn test_find_mvn_command_no_wrapper() {
        let tmp = TempDir::new().unwrap();
        let cmd = find_mvn_command(tmp.path());
        assert!(
            cmd.is_none() || cmd.as_ref().is_some_and(|path| path.ends_with("mvn")),
            "expected no wrapper or mvn path, got: {cmd:?}"
        );
    }

    #[test]
    fn test_find_mvn_command_with_wrapper() {
        let tmp = TempDir::new().unwrap();
        #[cfg(unix)]
        let wrapper_name = "mvnw";
        #[cfg(windows)]
        let wrapper_name = "mvnw.cmd";
        std::fs::write(tmp.path().join(wrapper_name), b"#!/bin/sh\nexec mvn \"$@\"").unwrap();

        let cmd = find_mvn_command(tmp.path());
        assert!(
            cmd.as_ref()
                .is_some_and(|path| path.ends_with(wrapper_name)),
            "Expected wrapper path, got: {cmd:?}"
        );
    }

    // -----------------------------------------------------------------------
    // POM dependency edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_pom_dependencies_version_with_property() {
        let pom = r"<project>
  <dependencies>
    <dependency>
      <groupId>org.example</groupId>
      <artifactId>lib</artifactId>
      <version>${lib.version}</version>
    </dependency>
  </dependencies>
</project>";

        let deps = parse_pom_dependencies(pom);
        // version contains ${...} but groupId/artifactId are clean,
        // so the dependency should be included.
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].version.as_deref(), Some("${lib.version}"));
    }

    #[test]
    fn test_parse_pom_dependencies_multiple_scopes() {
        let pom = r"<project>
  <dependencies>
    <dependency>
      <groupId>a</groupId>
      <artifactId>compile-dep</artifactId>
      <version>1.0</version>
      <scope>compile</scope>
    </dependency>
    <dependency>
      <groupId>b</groupId>
      <artifactId>runtime-dep</artifactId>
      <version>1.0</version>
      <scope>runtime</scope>
    </dependency>
    <dependency>
      <groupId>c</groupId>
      <artifactId>provided-dep</artifactId>
      <version>1.0</version>
      <scope>provided</scope>
    </dependency>
    <dependency>
      <groupId>d</groupId>
      <artifactId>test-dep</artifactId>
      <version>1.0</version>
      <scope>test</scope>
    </dependency>
  </dependencies>
</project>";

        let deps = parse_pom_dependencies(pom);
        // test scope is skipped, others are included.
        assert_eq!(deps.len(), 3);
        assert_eq!(deps[0].artifact_id, "compile-dep");
        assert_eq!(deps[1].artifact_id, "runtime-dep");
        assert_eq!(deps[2].artifact_id, "provided-dep");
    }
}
