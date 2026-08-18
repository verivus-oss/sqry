//! sbt classpath resolver.
//!
//! Resolves JVM classpath entries from sbt projects by:
//! 1. Executing `sbt -no-colors "print dependencyClasspath"` to get runtime classpath
//! 2. Parsing the output for JAR file paths (supports both `Attributed(...)` and
//!    colon-separated formats)
//! 3. Extracting Maven coordinates from Coursier cache paths
//! 4. Looking up source JARs in the Coursier cache
//! 5. Falling back to cached classpath on failure

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use log::{debug, info, warn};

use crate::{ClasspathError, ClasspathResult};

use super::{ClasspathEntry, ResolveConfig, ResolvedClasspath};

const SBT_CACHE_FILE: &str = "sbt-resolved-classpath.json";

// ── Public API ──────────────────────────────────────────────────────────────

/// Resolve classpath for an sbt project.
///
/// Strategy:
/// 1. Execute `sbt -no-colors "print dependencyClasspath"`
/// 2. Parse output for JAR paths
/// 3. On failure, fall back to Coursier cache scanning
#[allow(clippy::missing_errors_doc)] // Internal helper
pub fn resolve_sbt_classpath(config: &ResolveConfig) -> ClasspathResult<Vec<ResolvedClasspath>> {
    info!(
        "Resolving sbt classpath in {}",
        config.project_root.display()
    );

    match run_sbt_dependency_classpath(config) {
        Ok(jar_paths) => {
            info!("sbt returned {} JAR paths", jar_paths.len());
            let entries = build_entries(&jar_paths);
            let resolved = ResolvedClasspath {
                module_name: infer_module_name(&config.project_root),
                module_root: config.project_root.clone(),
                entries,
            };
            Ok(vec![resolved])
        }
        Err(e) => {
            warn!("sbt resolution failed: {e}. Attempting cache fallback.");
            try_cache_fallback(config, &e)
        }
    }
}

// ── sbt execution ───────────────────────────────────────────────────────────

/// Run `sbt -no-colors "print dependencyClasspath"` and parse JAR paths from output.
fn run_sbt_dependency_classpath(config: &ResolveConfig) -> ClasspathResult<Vec<PathBuf>> {
    let sbt_bin = find_sbt_binary()?;

    let mut cmd = Command::new(&sbt_bin);
    cmd.arg("-no-colors")
        .arg("print dependencyClasspath")
        .current_dir(&config.project_root)
        .stderr(std::process::Stdio::null());

    debug!(
        "Running: {} -no-colors \"print dependencyClasspath\"",
        sbt_bin.display()
    );

    let output = run_command_with_timeout(&mut cmd, config.timeout_secs)?;

    if !output.status.success() {
        return Err(ClasspathError::ResolutionFailed(format!(
            "sbt exited with status {}",
            output.status
        )));
    }

    let jars = parse_sbt_output(&output.stdout);
    Ok(jars)
}

/// Locate the `sbt` binary on `$PATH`.
fn find_sbt_binary() -> ClasspathResult<PathBuf> {
    which_binary("sbt").ok_or_else(|| {
        ClasspathError::ResolutionFailed(
            "sbt binary not found on PATH. Install sbt to resolve classpath.".to_string(),
        )
    })
}

/// Parse sbt `print dependencyClasspath` output.
///
/// sbt outputs classpath entries in one of these formats:
///
/// **Attributed format** (older sbt versions):
/// ```text
/// List(Attributed(/path/to/jar1.jar), Attributed(/path/to/jar2.jar))
/// ```
///
/// **Colon-separated format** (newer sbt versions):
/// ```text
/// /path/to/jar1.jar:/path/to/jar2.jar
/// ```
///
/// **One-per-line format** (some sbt plugins):
/// ```text
/// /path/to/jar1.jar
/// /path/to/jar2.jar
/// ```
///
/// We handle all three formats, filtering to `.jar` files only.
#[allow(clippy::manual_let_else)] // Match for error handling clarity
fn parse_sbt_output(stdout: &[u8]) -> Vec<PathBuf> {
    let mut jars = Vec::new();

    for line in stdout.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Skip sbt log/info lines (e.g., "[info] ...", "[success] ...").
        if is_sbt_log_line(trimmed) {
            continue;
        }

        // Try Attributed format first.
        if trimmed.starts_with("List(") || trimmed.contains("Attributed(") {
            jars.extend(parse_attributed_format(trimmed));
            continue;
        }

        // Try colon-separated format (only if line contains ':' and paths).
        if trimmed.contains(':') && trimmed.contains(".jar") {
            jars.extend(parse_colon_separated(trimmed));
            continue;
        }

        // One-per-line format: single path per line.
        if is_jar_path(trimmed) {
            jars.push(PathBuf::from(trimmed));
        }
    }

    jars
}

/// Parse `Attributed(...)` entries from a line.
///
/// Input: `List(Attributed(/path/to/a.jar), Attributed(/path/to/b.jar))`
/// Output: `["/path/to/a.jar", "/path/to/b.jar"]`
fn parse_attributed_format(line: &str) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let mut search_from = 0;

    while let Some(start) = line[search_from..].find("Attributed(") {
        let abs_start = search_from + start + "Attributed(".len();
        if let Some(end) = line[abs_start..].find(')') {
            let path_str = line[abs_start..abs_start + end].trim();
            if is_jar_path(path_str) {
                results.push(PathBuf::from(path_str));
            }
            search_from = abs_start + end + 1;
        } else {
            break;
        }
    }

    results
}

/// Parse colon-separated classpath entries.
///
/// Input: `/path/to/a.jar:/path/to/b.jar:/path/to/c.jar`
fn parse_colon_separated(line: &str) -> Vec<PathBuf> {
    line.split(':')
        .map(str::trim)
        .filter(|s| is_jar_path(s))
        .map(PathBuf::from)
        .collect()
}

/// Check whether a string looks like a JAR file path.
fn is_jar_path(s: &str) -> bool {
    !s.is_empty() && s.to_ascii_lowercase().ends_with(".jar")
}

/// Check whether a line is an sbt log/info/warning line that should be skipped.
fn is_sbt_log_line(line: &str) -> bool {
    line.starts_with("[info]")
        || line.starts_with("[warn]")
        || line.starts_with("[error]")
        || line.starts_with("[success]")
        || line.starts_with("[debug]")
}

// ── Coordinate extraction ───────────────────────────────────────────────────

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
fn build_entries(jar_paths: &[PathBuf]) -> Vec<ClasspathEntry> {
    jar_paths
        .iter()
        .map(|jar_path| {
            let coordinates = parse_coursier_coordinates(jar_path);
            let source_jar = find_source_jar(jar_path);

            ClasspathEntry {
                jar_path: jar_path.clone(),
                coordinates,
                is_direct: false, // sbt dependencyClasspath returns the full transitive closure.
                source_jar,
            }
        })
        .collect()
}

/// Find a source JAR alongside a main JAR.
///
/// Looks for `<stem>-sources.jar` in the same directory.
fn find_source_jar(jar_path: &Path) -> Option<PathBuf> {
    let stem = jar_path.file_stem()?.to_string_lossy();
    let parent = jar_path.parent()?;

    // Try `<stem>-sources.jar` in the same directory.
    let sources_jar = parent.join(format!("{stem}-sources.jar"));
    if sources_jar.exists() {
        return Some(sources_jar);
    }

    // Try Coursier-style: derive `-sources.jar` path.
    if let Some(coursier_sources) = derive_coursier_source_jar(jar_path)
        && coursier_sources.exists()
    {
        return Some(coursier_sources);
    }

    None
}

/// Derive the Coursier cache path for a source JAR given the main JAR path.
#[allow(clippy::case_sensitive_file_extension_comparisons)] // Known file extensions
fn derive_coursier_source_jar(jar_path: &Path) -> Option<PathBuf> {
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
/// Read a previously cached sbt classpath without invoking sbt. Returns `None`
/// when no usable cache is present. Used when build-tool execution is disabled
/// (`--no-build-tool`).
pub(crate) fn read_cached_classpath(config: &ResolveConfig) -> Option<Vec<ResolvedClasspath>> {
    let cache_path = config.cache_path.as_ref()?;
    let cache_file = if cache_path.is_dir() {
        cache_path.join(SBT_CACHE_FILE)
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
            cache_path.join(SBT_CACHE_FILE)
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
        "sbt resolution failed and no cache available. Original error: {original_error}"
    )))
}

// ── Utility functions ───────────────────────────────────────────────────────

/// Find a binary on `$PATH` using `which`-style lookup.
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

    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                return child.wait_with_output().map_err(|e| {
                    ClasspathError::ResolutionFailed(format!("Failed to collect output: {e}"))
                });
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
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

    // ── Test: parse sbt Attributed output ───────────────────────────────

    #[test]
    fn test_parse_attributed_format() {
        let line =
            "List(Attributed(/path/to/guava-33.0.0.jar), Attributed(/path/to/slf4j-api-2.0.9.jar))";
        let result = parse_attributed_format(line);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], PathBuf::from("/path/to/guava-33.0.0.jar"));
        assert_eq!(result[1], PathBuf::from("/path/to/slf4j-api-2.0.9.jar"));
    }

    #[test]
    fn test_parse_attributed_format_single() {
        let line = "List(Attributed(/only/one.jar))";
        let result = parse_attributed_format(line);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PathBuf::from("/only/one.jar"));
    }

    #[test]
    fn test_parse_attributed_format_filters_non_jar() {
        let line = "List(Attributed(/path/to/classes), Attributed(/path/to/real.jar))";
        let result = parse_attributed_format(line);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PathBuf::from("/path/to/real.jar"));
    }

    // ── Test: parse sbt colon-separated output ──────────────────────────

    #[test]
    fn test_parse_colon_separated() {
        let line = "/path/to/a.jar:/path/to/b.jar:/path/to/c.jar";
        let result = parse_colon_separated(line);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], PathBuf::from("/path/to/a.jar"));
        assert_eq!(result[1], PathBuf::from("/path/to/b.jar"));
        assert_eq!(result[2], PathBuf::from("/path/to/c.jar"));
    }

    #[test]
    fn test_parse_colon_separated_filters_non_jar() {
        let line = "/path/to/a.jar:/path/to/classes:/path/to/b.jar";
        let result = parse_colon_separated(line);
        assert_eq!(result.len(), 2);
    }

    // ── Test: parse full sbt output ─────────────────────────────────────

    #[test]
    fn test_parse_sbt_output_attributed() {
        let output = b"\
[info] Loading settings for project root from build.sbt ...
[info] Set current project to myproject
List(Attributed(/home/user/.cache/coursier/v1/https/repo1.maven.org/maven2/com/google/guava/guava/33.0.0/guava-33.0.0.jar), Attributed(/home/user/.cache/coursier/v1/https/repo1.maven.org/maven2/org/slf4j/slf4j-api/2.0.9/slf4j-api-2.0.9.jar))
[success] Total time: 1 s
";
        let result = parse_sbt_output(output);
        assert_eq!(result.len(), 2);
        assert!(result[0].to_str().unwrap().contains("guava-33.0.0.jar"));
        assert!(result[1].to_str().unwrap().contains("slf4j-api-2.0.9.jar"));
    }

    #[test]
    fn test_parse_sbt_output_colon_separated() {
        let output = b"\
[info] Loading project definition
/path/to/a.jar:/path/to/b.jar
[success] Done
";
        let result = parse_sbt_output(output);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_parse_sbt_output_one_per_line() {
        let output = b"\
/path/to/a.jar
/path/to/b.jar
/path/to/c.jar
";
        let result = parse_sbt_output(output);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_parse_sbt_output_empty() {
        let result = parse_sbt_output(b"");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_sbt_output_only_log_lines() {
        let output = b"\
[info] Loading settings
[info] Set current project
[success] Total time: 0 s
";
        let result = parse_sbt_output(output);
        assert!(result.is_empty());
    }

    // ── Test: sbt log line detection ────────────────────────────────────

    #[test]
    fn test_is_sbt_log_line() {
        assert!(is_sbt_log_line("[info] Loading settings"));
        assert!(is_sbt_log_line("[warn] Deprecated API"));
        assert!(is_sbt_log_line("[error] Compilation failed"));
        assert!(is_sbt_log_line("[success] Total time: 1 s"));
        assert!(is_sbt_log_line("[debug] Resolving dependencies"));
        assert!(!is_sbt_log_line("/path/to/jar.jar"));
        assert!(!is_sbt_log_line("List(Attributed(/path.jar))"));
    }

    // ── Test: Coursier coordinate extraction ────────────────────────────

    #[test]
    fn test_parse_coursier_coordinates() {
        let path = PathBuf::from(
            "/home/user/.cache/coursier/v1/https/repo1.maven.org/maven2/com/google/guava/guava/33.0.0/guava-33.0.0.jar",
        );
        let coords = parse_coursier_coordinates(&path);
        assert_eq!(coords, Some("com.google.guava:guava:33.0.0".to_string()));
    }

    #[test]
    fn test_parse_coursier_coordinates_scala_library() {
        let path = PathBuf::from(
            "/home/user/.cache/coursier/v1/https/repo1.maven.org/maven2/org/scala-lang/scala-library/2.13.12/scala-library-2.13.12.jar",
        );
        let coords = parse_coursier_coordinates(&path);
        assert_eq!(
            coords,
            Some("org.scala-lang:scala-library:2.13.12".to_string())
        );
    }

    #[test]
    fn test_parse_coursier_coordinates_not_coursier() {
        let path = PathBuf::from("/usr/local/lib/some.jar");
        assert_eq!(parse_coursier_coordinates(&path), None);
    }

    // ── Test: missing sbt binary ────────────────────────────────────────

    #[test]
    fn test_missing_sbt_binary_error() {
        let tmp = TempDir::new().unwrap();
        let original_path = std::env::var_os("PATH");

        // SAFETY: This test is not run in parallel with other tests that depend
        // on PATH. We restore the original value immediately after the check.
        unsafe { std::env::set_var("PATH", tmp.path()) };
        let result = find_sbt_binary();
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

    // ── Test: resolve with no sbt and no cache ──────────────────────────

    #[test]
    fn test_resolve_no_sbt_no_cache_returns_error() {
        let tmp = TempDir::new().unwrap();
        let config = ResolveConfig {
            project_root: tmp.path().to_path_buf(),
            timeout_secs: 5,
            cache_path: None,
        };

        let result = resolve_sbt_classpath(&config);
        assert!(result.is_err());
    }

    // ── Test: cache fallback ────────────────────────────────────────────

    #[test]
    fn test_cache_fallback_loads_cached_classpath() {
        let tmp = TempDir::new().unwrap();
        let cache_path = tmp.path().join("classpath_cache.json");

        let cached = vec![ResolvedClasspath {
            module_name: "cached-scala-project".to_string(),
            module_root: tmp.path().to_path_buf(),
            entries: vec![ClasspathEntry {
                jar_path: PathBuf::from("/cached/scala-library.jar"),
                coordinates: Some("org.scala-lang:scala-library:2.13.12".to_string()),
                is_direct: false,
                source_jar: None,
            }],
        }];
        std::fs::write(&cache_path, serde_json::to_string(&cached).unwrap()).unwrap();

        let original_error = ClasspathError::ResolutionFailed("sbt not found".to_string());
        let config = ResolveConfig {
            project_root: tmp.path().to_path_buf(),
            timeout_secs: 5,
            cache_path: Some(cache_path),
        };

        let result = try_cache_fallback(&config, &original_error);
        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].module_name, "cached-scala-project");
        assert_eq!(resolved[0].entries.len(), 1);
    }

    #[test]
    fn test_cache_fallback_missing_cache_file() {
        let tmp = TempDir::new().unwrap();
        let cache_path = tmp.path().join("nonexistent.json");
        let original_error = ClasspathError::ResolutionFailed("sbt not found".to_string());
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
        let original_error = ClasspathError::ResolutionFailed("sbt not found".to_string());
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
        let main_jar = tmp.path().join("scala-library-2.13.12.jar");
        let sources_jar = tmp.path().join("scala-library-2.13.12-sources.jar");
        std::fs::write(&main_jar, b"").unwrap();
        std::fs::write(&sources_jar, b"").unwrap();

        let result = find_source_jar(&main_jar);
        assert_eq!(result, Some(sources_jar));
    }

    #[test]
    fn test_find_source_jar_not_present() {
        let tmp = TempDir::new().unwrap();
        let main_jar = tmp.path().join("scala-library-2.13.12.jar");
        std::fs::write(&main_jar, b"").unwrap();

        let result = find_source_jar(&main_jar);
        assert_eq!(result, None);
    }

    // ── Test: build_entries enrichment ───────────────────────────────────

    #[test]
    fn test_build_entries_with_coursier_path() {
        let jar_paths = vec![
            PathBuf::from(
                "/home/user/.cache/coursier/v1/https/repo1.maven.org/maven2/com/google/guava/guava/33.0.0/guava-33.0.0.jar",
            ),
            PathBuf::from("/some/local/path/unknown.jar"),
        ];

        let entries = build_entries(&jar_paths);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].coordinates,
            Some("com.google.guava:guava:33.0.0".to_string())
        );
        assert_eq!(entries[1].coordinates, None);
        assert!(!entries[0].is_direct);
        assert!(!entries[1].is_direct);
    }

    // ── Test: infer_module_name ─────────────────────────────────────────

    #[test]
    fn test_infer_module_name() {
        assert_eq!(
            infer_module_name(Path::new("/home/user/my-scala-project")),
            "my-scala-project"
        );
        assert_eq!(infer_module_name(Path::new("/")), "root");
    }

    // ── Test: derive_coursier_source_jar ────────────────────────────────

    #[test]
    fn test_derive_coursier_source_jar() {
        let jar = PathBuf::from("/cache/v1/scala-library-2.13.12.jar");
        let result = derive_coursier_source_jar(&jar);
        assert_eq!(
            result,
            Some(PathBuf::from("/cache/v1/scala-library-2.13.12-sources.jar"))
        );
    }

    #[test]
    fn test_derive_coursier_source_jar_already_sources() {
        let jar = PathBuf::from("/cache/v1/scala-library-2.13.12-sources.jar");
        let result = derive_coursier_source_jar(&jar);
        assert_eq!(result, None);
    }
}
