//! Gradle classpath resolver.
//!
//! Extracts classpath JARs from Gradle projects by writing a temporary init script
//! and executing `gradlew --init-script <script> sqryListClasspath`. Parses the
//! structured output lines to build [`ResolvedClasspath`] entries per module.
//!
//! ## Strategy
//!
//! 1. Write a temporary init script that adds a `sqryListClasspath` task to all projects.
//! 2. Locate `gradlew` (or `gradlew.bat` on Windows) in the project root.
//! 3. Execute the wrapper with the init script. Timeout defaults to 60 seconds.
//! 4. Parse `SQRY_CP:<module>:<group>:<name>:<version>:<path>` lines.
//! 5. On failure or timeout, fall back to a cached `resolved-classpath.json`.
//!
//! ## Security
//!
//! Only the project's own Gradle wrapper is executed — never a system-wide `gradle`
//! binary. This prevents supply-chain attacks via a rogue global installation.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use log::{debug, info, warn};

use crate::{ClasspathError, ClasspathResult};

use super::{ClasspathEntry, ResolveConfig, ResolvedClasspath};

/// The Groovy init script injected into the Gradle build.
///
/// Adds a `sqryListClasspath` task to every project that iterates resolved
/// artifacts from `compileClasspath` and prints structured lines.
const INIT_SCRIPT: &str = r#"allprojects {
    task sqryListClasspath {
        doLast {
            configurations.findAll { it.name == 'compileClasspath' || it.name == 'implementation' }
                .each { config ->
                    try {
                        config.resolvedConfiguration.resolvedArtifacts.each { artifact ->
                            println "SQRY_CP:${project.name}:${artifact.moduleVersion.id.group}:${artifact.moduleVersion.id.name}:${artifact.moduleVersion.id.version}:${artifact.file}"
                        }
                    } catch (Exception e) {
                        println "SQRY_CP_ERR:${project.name}:${e.message}"
                    }
                }
        }
    }
}
"#;

/// Output line prefix for successful classpath entries.
const CP_PREFIX: &str = "SQRY_CP:";

/// Output line prefix for per-module resolution errors.
const CP_ERR_PREFIX: &str = "SQRY_CP_ERR:";

/// Cache filename written inside `.sqry/classpath/`.
const CACHE_FILENAME: &str = "resolved-classpath.json";

/// Resolve classpath for a Gradle project.
///
/// Writes a temporary init script, executes `gradlew --init-script <script>
/// sqryListClasspath`, and parses the output for JAR paths. On failure or
/// timeout, falls back to a previously cached classpath if available.
///
/// Only the project-local `gradlew` wrapper is used — never system `gradle`.
#[allow(clippy::missing_errors_doc)] // Internal helper
pub fn resolve_gradle_classpath(config: &ResolveConfig) -> ClasspathResult<Vec<ResolvedClasspath>> {
    let wrapper = find_gradle_wrapper(&config.project_root)?;
    info!("Found Gradle wrapper at {}", wrapper.display());

    // Write the init script to a temp file that will be cleaned up on drop.
    let init_script_file = write_init_script()?;
    let init_script_path = init_script_file.path();

    debug!("Wrote init script to {}", init_script_path.display());

    // Build and execute the Gradle command.
    let output = execute_gradle(
        &wrapper,
        init_script_path,
        &config.project_root,
        config.timeout_secs,
    );

    match output {
        Ok(stdout) => {
            let classpaths = parse_gradle_output(&stdout);
            // Enrich with source JAR discovery.
            let classpaths = enrich_source_jars(classpaths);

            // Cache the result for future fallback.
            let cache_dir = resolve_cache_dir(config);
            if let Err(e) = write_cache(&cache_dir, &classpaths) {
                warn!("Failed to write classpath cache: {e}");
            }

            Ok(classpaths)
        }
        Err(e) => {
            warn!("Gradle resolution failed: {e}");
            warn!("Attempting to fall back to cached classpath");
            let cache_dir = resolve_cache_dir(config);
            read_cache(&cache_dir)
        }
    }
}

/// Locate the Gradle wrapper script in the project root.
///
/// On Windows, looks for `gradlew.bat`; on Unix, `gradlew`.
fn find_gradle_wrapper(project_root: &Path) -> ClasspathResult<PathBuf> {
    let wrapper_name = if cfg!(windows) {
        "gradlew.bat"
    } else {
        "gradlew"
    };

    let wrapper_path = project_root.join(wrapper_name);
    if wrapper_path.exists() {
        Ok(wrapper_path)
    } else {
        Err(ClasspathError::ResolutionFailed(format!(
            "Gradle wrapper '{}' not found in {}",
            wrapper_name,
            project_root.display()
        )))
    }
}

/// Write the init script to a temporary file.
fn write_init_script() -> ClasspathResult<tempfile::NamedTempFile> {
    use std::io::Write;

    let mut file = tempfile::Builder::new()
        .prefix("sqry-gradle-init-")
        .suffix(".gradle")
        .tempfile()
        .map_err(|e| {
            ClasspathError::ResolutionFailed(format!("Failed to create init script temp file: {e}"))
        })?;

    file.write_all(INIT_SCRIPT.as_bytes()).map_err(|e| {
        ClasspathError::ResolutionFailed(format!("Failed to write init script: {e}"))
    })?;

    file.flush().map_err(|e| {
        ClasspathError::ResolutionFailed(format!("Failed to flush init script: {e}"))
    })?;

    Ok(file)
}

/// Execute the Gradle wrapper with the init script and return stdout.
fn execute_gradle(
    wrapper: &Path,
    init_script: &Path,
    project_root: &Path,
    timeout_secs: u64,
) -> ClasspathResult<String> {
    let mut child = Command::new(wrapper)
        .args([
            "--init-script",
            &init_script.to_string_lossy(),
            "sqryListClasspath",
            "--quiet",
            "--no-daemon",
        ])
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            ClasspathError::ResolutionFailed(format!(
                "Failed to spawn Gradle wrapper {}: {e}",
                wrapper.display()
            ))
        })?;

    let timeout = Duration::from_secs(timeout_secs);
    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            if status.success() {
                let stdout = child
                    .stdout
                    .take()
                    .map(|s| {
                        std::io::BufReader::new(s)
                            .lines()
                            .map_while(Result::ok)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                Ok(stdout)
            } else {
                let stderr = child
                    .stderr
                    .take()
                    .map(|s| {
                        std::io::BufReader::new(s)
                            .lines()
                            .map_while(Result::ok)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                Err(ClasspathError::ResolutionFailed(format!(
                    "Gradle exited with status {status}: {stderr}"
                )))
            }
        }
        Ok(None) => {
            // Timeout — kill the process.
            let _ = child.kill();
            let _ = child.wait();
            Err(ClasspathError::ResolutionFailed(format!(
                "Gradle timed out after {timeout_secs}s"
            )))
        }
        Err(e) => Err(ClasspathError::ResolutionFailed(format!(
            "Failed to wait on Gradle process: {e}"
        ))),
    }
}

/// Parse structured output lines from the Gradle init script.
///
/// Expected format: `SQRY_CP:<module>:<group>:<name>:<version>:<path>`
///
/// Lines that do not match this format are silently skipped. Error lines
/// (`SQRY_CP_ERR:`) are logged as warnings.
pub(crate) fn parse_gradle_output(output: &str) -> Vec<ResolvedClasspath> {
    let mut modules: HashMap<String, Vec<ClasspathEntry>> = HashMap::new();

    for line in output.lines() {
        let trimmed = line.trim();

        if let Some(err_payload) = trimmed.strip_prefix(CP_ERR_PREFIX) {
            // Log error lines from Gradle but don't treat them as fatal.
            warn!("Gradle resolution error: {err_payload}");
            continue;
        }

        if let Some(payload) = trimmed.strip_prefix(CP_PREFIX)
            && let Some(entry) = parse_cp_line(payload)
        {
            modules.entry(entry.0).or_default().push(entry.1);
        }
        // All other lines are silently ignored (Gradle progress, warnings, etc.).
    }

    let mut result: Vec<ResolvedClasspath> = modules
        .into_iter()
        .map(|(module_name, entries)| ResolvedClasspath {
            module_name,
            entries,
        })
        .collect();

    // Sort by module name for deterministic output.
    result.sort_by(|a, b| a.module_name.cmp(&b.module_name));
    result
}

/// Parse a single classpath payload after stripping the `SQRY_CP:` prefix.
///
/// Expected: `<module>:<group>:<name>:<version>:<path>`
///
/// The path itself may contain colons (e.g., Windows drive letters like `C:\...`),
/// so we split into exactly 5 parts, where the last part captures everything
/// after the 4th colon.
fn parse_cp_line(payload: &str) -> Option<(String, ClasspathEntry)> {
    let mut parts = payload.splitn(5, ':');

    let module = parts.next()?;
    let group = parts.next()?;
    let name = parts.next()?;
    let version = parts.next()?;
    let path_str = parts.next()?;

    // Validate that we have non-empty components.
    if module.is_empty()
        || group.is_empty()
        || name.is_empty()
        || version.is_empty()
        || path_str.is_empty()
    {
        return None;
    }

    let coordinates = format!("{group}:{name}:{version}");
    let jar_path = PathBuf::from(path_str);

    Some((
        module.to_string(),
        ClasspathEntry {
            jar_path,
            coordinates: Some(coordinates),
            is_direct: true,
            source_jar: None,
        },
    ))
}

/// Enrich classpath entries with source JAR paths by probing the Gradle cache.
///
/// For each entry with Maven coordinates, looks for a `-sources.jar` in the
/// standard Gradle module cache layout:
/// `~/.gradle/caches/modules-2/files-2.1/<group>/<name>/<version>/`
fn enrich_source_jars(classpaths: Vec<ResolvedClasspath>) -> Vec<ResolvedClasspath> {
    classpaths
        .into_iter()
        .map(|mut cp| {
            for entry in &mut cp.entries {
                if let Some(source_jar) = find_source_jar(entry) {
                    entry.source_jar = Some(source_jar);
                }
            }
            cp
        })
        .collect()
}

/// Attempt to find a source JAR for a classpath entry in the Gradle cache.
fn find_source_jar(entry: &ClasspathEntry) -> Option<PathBuf> {
    let coords = entry.coordinates.as_ref()?;
    let mut coord_parts = coords.splitn(3, ':');
    let group = coord_parts.next()?;
    let name = coord_parts.next()?;
    let version = coord_parts.next()?;

    let gradle_cache = gradle_cache_dir()?;
    let module_dir = gradle_cache
        .join("caches")
        .join("modules-2")
        .join("files-2.1")
        .join(group)
        .join(name)
        .join(version);

    if !module_dir.is_dir() {
        return None;
    }

    let source_jar_name = format!("{name}-{version}-sources.jar");

    // The Gradle cache stores files under hash subdirectories, so we need to
    // walk one level of hash dirs.
    let entries = std::fs::read_dir(&module_dir).ok()?;
    for hash_dir_entry in entries.flatten() {
        if hash_dir_entry.file_type().ok()?.is_dir() {
            let candidate = hash_dir_entry.path().join(&source_jar_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Return the Gradle user home directory.
///
/// Checks `GRADLE_USER_HOME` environment variable first, then falls back to
/// `~/.gradle`.
fn gradle_cache_dir() -> Option<PathBuf> {
    if let Ok(gradle_home) = std::env::var("GRADLE_USER_HOME") {
        let path = PathBuf::from(gradle_home);
        if path.is_dir() {
            return Some(path);
        }
    }

    home_dir().map(|home| home.join(".gradle"))
}

/// Portable home directory lookup (avoids pulling in the `dirs` crate).
fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// Determine the cache directory for resolved classpath data.
fn resolve_cache_dir(config: &ResolveConfig) -> PathBuf {
    config
        .cache_path
        .clone()
        .unwrap_or_else(|| config.project_root.join(".sqry").join("classpath"))
}

/// Write resolved classpaths to the cache directory as JSON.
fn write_cache(cache_dir: &Path, classpaths: &[ResolvedClasspath]) -> ClasspathResult<()> {
    std::fs::create_dir_all(cache_dir)?;

    let cache_path = cache_dir.join(CACHE_FILENAME);
    let json = serde_json::to_string_pretty(classpaths)
        .map_err(|e| ClasspathError::CacheError(format!("Failed to serialize classpath: {e}")))?;

    std::fs::write(&cache_path, json)?;

    debug!("Wrote classpath cache to {}", cache_path.display());
    Ok(())
}

/// Read previously cached classpath data. Returns an empty vec with a warning
/// if no cache exists.
fn read_cache(cache_dir: &Path) -> ClasspathResult<Vec<ResolvedClasspath>> {
    let cache_path = cache_dir.join(CACHE_FILENAME);

    if !cache_path.exists() {
        warn!(
            "No cached classpath found at {}; returning empty classpath",
            cache_path.display()
        );
        return Ok(Vec::new());
    }

    let json = std::fs::read_to_string(&cache_path)?;
    let classpaths: Vec<ResolvedClasspath> = serde_json::from_str(&json).map_err(|e| {
        ClasspathError::CacheError(format!("Failed to deserialize classpath cache: {e}"))
    })?;

    info!(
        "Loaded {} modules from classpath cache at {}",
        classpaths.len(),
        cache_path.display()
    );

    Ok(classpaths)
}

/// Extension trait for [`std::process::Child`] providing timeout-aware waiting.
///
/// Uses a polling loop with short sleeps rather than platform-specific APIs,
/// trading a small amount of latency for portability.
trait WaitTimeout {
    /// Wait for the child process to exit, returning `Ok(None)` if the timeout
    /// expires before the process finishes.
    fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>>;
}

impl WaitTimeout for std::process::Child {
    fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(100);

        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            std::thread::sleep(poll_interval);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // parse_gradle_output tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_valid_output_single_module() {
        let output = "\
SQRY_CP:app:com.google.guava:guava:33.0.0:/home/user/.gradle/caches/modules-2/files-2.1/com.google.guava/guava/33.0.0/abc123/guava-33.0.0.jar
SQRY_CP:app:org.slf4j:slf4j-api:2.0.9:/home/user/.gradle/caches/modules-2/files-2.1/org.slf4j/slf4j-api/2.0.9/def456/slf4j-api-2.0.9.jar";

        let result = parse_gradle_output(output);
        assert_eq!(result.len(), 1);

        let module = &result[0];
        assert_eq!(module.module_name, "app");
        assert_eq!(module.entries.len(), 2);

        assert_eq!(
            module.entries[0].coordinates.as_deref(),
            Some("com.google.guava:guava:33.0.0")
        );
        assert_eq!(
            module.entries[0].jar_path,
            PathBuf::from(
                "/home/user/.gradle/caches/modules-2/files-2.1/com.google.guava/guava/33.0.0/abc123/guava-33.0.0.jar"
            )
        );

        assert_eq!(
            module.entries[1].coordinates.as_deref(),
            Some("org.slf4j:slf4j-api:2.0.9")
        );
    }

    #[test]
    fn test_parse_multi_module_output() {
        let output = "\
SQRY_CP:app:com.google.guava:guava:33.0.0:/path/to/guava.jar
SQRY_CP:lib:org.apache.commons:commons-lang3:3.14.0:/path/to/commons-lang3.jar
SQRY_CP:app:org.slf4j:slf4j-api:2.0.9:/path/to/slf4j-api.jar
SQRY_CP:lib:com.fasterxml.jackson.core:jackson-core:2.16.0:/path/to/jackson-core.jar";

        let result = parse_gradle_output(output);
        assert_eq!(result.len(), 2);

        let app = result.iter().find(|m| m.module_name == "app").unwrap();
        assert_eq!(app.entries.len(), 2);

        let lib = result.iter().find(|m| m.module_name == "lib").unwrap();
        assert_eq!(lib.entries.len(), 2);
    }

    #[test]
    fn test_parse_empty_output() {
        let result = parse_gradle_output("");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_output_with_noise() {
        let output = "\
Downloading https://services.gradle.org/distributions/gradle-8.5-bin.zip
...........10%...........20%...........30%...........40%...........50%
> Task :app:sqryListClasspath
SQRY_CP:app:com.google.guava:guava:33.0.0:/path/to/guava.jar
BUILD SUCCESSFUL in 5s
1 actionable task: 1 executed";

        let result = parse_gradle_output(output);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].entries.len(), 1);
        assert_eq!(
            result[0].entries[0].coordinates.as_deref(),
            Some("com.google.guava:guava:33.0.0")
        );
    }

    #[test]
    fn test_parse_malformed_lines_skipped() {
        let output = "\
SQRY_CP:app:com.google.guava:guava:33.0.0:/path/to/guava.jar
SQRY_CP:broken:only_three_parts
SQRY_CP:::::/path/empty_fields
SQRY_CP:app:org.slf4j:slf4j-api:2.0.9:/path/to/slf4j-api.jar
SQRY_CP:";

        let result = parse_gradle_output(output);
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].entries.len(),
            2,
            "Only valid lines should produce entries"
        );
    }

    #[test]
    fn test_parse_error_lines_logged() {
        let output = "\
SQRY_CP:app:com.google.guava:guava:33.0.0:/path/to/guava.jar
SQRY_CP_ERR:lib:Could not resolve configuration 'compileClasspath'
SQRY_CP:app:org.slf4j:slf4j-api:2.0.9:/path/to/slf4j-api.jar";

        let result = parse_gradle_output(output);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].entries.len(), 2);
        // Error lines are logged but don't produce entries.
    }

    #[test]
    fn test_parse_windows_path_with_colon() {
        // The path contains a colon from the drive letter — the parser must
        // handle this by splitting into at most 5 parts.
        let output =
            "SQRY_CP:app:com.google.guava:guava:33.0.0:C:\\Users\\dev\\.gradle\\caches\\guava.jar";

        let result = parse_gradle_output(output);
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].entries[0].jar_path,
            PathBuf::from("C:\\Users\\dev\\.gradle\\caches\\guava.jar")
        );
    }

    // -----------------------------------------------------------------------
    // source JAR path construction tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_source_jar_path_construction() {
        let tmp = TempDir::new().unwrap();

        // Simulate a Gradle cache structure.
        let module_dir = tmp
            .path()
            .join("caches/modules-2/files-2.1/com.google.guava/guava/33.0.0/abc123");
        std::fs::create_dir_all(&module_dir).unwrap();
        let source_jar = module_dir.join("guava-33.0.0-sources.jar");
        std::fs::write(&source_jar, b"fake jar").unwrap();

        // Set GRADLE_USER_HOME so `gradle_cache_dir()` finds our temp dir.
        // Safety: tests are run with --test-threads=1 for env var isolation,
        // or this test is self-contained enough that the RAII guard suffices.
        let _guard = EnvGuard::set("GRADLE_USER_HOME", tmp.path().to_str().unwrap());

        let entry = ClasspathEntry {
            jar_path: PathBuf::from("/path/to/guava-33.0.0.jar"),
            coordinates: Some("com.google.guava:guava:33.0.0".to_string()),
            is_direct: true,
            source_jar: None,
        };

        let found = find_source_jar(&entry);
        assert_eq!(found, Some(source_jar));
    }

    #[test]
    fn test_source_jar_not_found() {
        let tmp = TempDir::new().unwrap();
        let _guard = EnvGuard::set("GRADLE_USER_HOME", tmp.path().to_str().unwrap());

        let entry = ClasspathEntry {
            jar_path: PathBuf::from("/path/to/guava-33.0.0.jar"),
            coordinates: Some("com.google.guava:guava:33.0.0".to_string()),
            is_direct: true,
            source_jar: None,
        };

        let found = find_source_jar(&entry);
        assert!(found.is_none());
    }

    #[test]
    fn test_source_jar_no_coordinates() {
        let entry = ClasspathEntry {
            jar_path: PathBuf::from("/path/to/something.jar"),
            coordinates: None,
            is_direct: true,
            source_jar: None,
        };

        let found = find_source_jar(&entry);
        assert!(found.is_none());
    }

    // -----------------------------------------------------------------------
    // Cache roundtrip tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_cache_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let cache_dir = tmp.path().join("cache");

        let classpaths = vec![
            ResolvedClasspath {
                module_name: "app".to_string(),
                entries: vec![ClasspathEntry {
                    jar_path: PathBuf::from("/path/to/guava.jar"),
                    coordinates: Some("com.google.guava:guava:33.0.0".to_string()),
                    is_direct: true,
                    source_jar: None,
                }],
            },
            ResolvedClasspath {
                module_name: "lib".to_string(),
                entries: vec![ClasspathEntry {
                    jar_path: PathBuf::from("/path/to/commons.jar"),
                    coordinates: Some("org.apache.commons:commons-lang3:3.14.0".to_string()),
                    is_direct: true,
                    source_jar: Some(PathBuf::from("/path/to/commons-sources.jar")),
                }],
            },
        ];

        write_cache(&cache_dir, &classpaths).expect("cache write should succeed");

        let loaded = read_cache(&cache_dir).expect("cache read should succeed");
        assert_eq!(loaded.len(), 2);

        let app = loaded.iter().find(|m| m.module_name == "app").unwrap();
        assert_eq!(app.entries.len(), 1);
        assert_eq!(
            app.entries[0].coordinates.as_deref(),
            Some("com.google.guava:guava:33.0.0")
        );

        let lib = loaded.iter().find(|m| m.module_name == "lib").unwrap();
        assert_eq!(
            lib.entries[0].source_jar,
            Some(PathBuf::from("/path/to/commons-sources.jar"))
        );
    }

    #[test]
    fn test_cache_read_missing_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let cache_dir = tmp.path().join("nonexistent");

        let result = read_cache(&cache_dir).expect("should succeed with empty vec");
        assert!(result.is_empty());
    }

    // -----------------------------------------------------------------------
    // gradlew detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_missing_gradlew_error() {
        let tmp = TempDir::new().unwrap();
        let result = find_gradle_wrapper(tmp.path());
        assert!(result.is_err());

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not found"),
            "Error message should mention 'not found': {msg}"
        );
    }

    #[test]
    fn test_gradlew_found() {
        let tmp = TempDir::new().unwrap();
        let wrapper_name = if cfg!(windows) {
            "gradlew.bat"
        } else {
            "gradlew"
        };
        std::fs::write(tmp.path().join(wrapper_name), "#!/bin/sh\n").unwrap();

        let result = find_gradle_wrapper(tmp.path());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), tmp.path().join(wrapper_name));
    }

    // -----------------------------------------------------------------------
    // init script writing test
    // -----------------------------------------------------------------------

    #[test]
    fn test_init_script_content() {
        let file = write_init_script().expect("should create init script");
        let content = std::fs::read_to_string(file.path()).unwrap();
        assert!(content.contains("sqryListClasspath"));
        assert!(content.contains("SQRY_CP:"));
        assert!(content.contains("compileClasspath"));
        assert!(content.contains("resolvedConfiguration"));
    }

    // -----------------------------------------------------------------------
    // resolve_cache_dir tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_resolve_cache_dir_default() {
        let config = ResolveConfig {
            project_root: PathBuf::from("/my/project"),
            timeout_secs: 60,
            cache_path: None,
        };
        let dir = resolve_cache_dir(&config);
        assert_eq!(dir, PathBuf::from("/my/project/.sqry/classpath"));
    }

    #[test]
    fn test_resolve_cache_dir_override() {
        let config = ResolveConfig {
            project_root: PathBuf::from("/my/project"),
            timeout_secs: 60,
            cache_path: Some(PathBuf::from("/custom/cache")),
        };
        let dir = resolve_cache_dir(&config);
        assert_eq!(dir, PathBuf::from("/custom/cache"));
    }

    // -----------------------------------------------------------------------
    // parse_cp_line unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_cp_line_valid() {
        let result = parse_cp_line("app:com.google.guava:guava:33.0.0:/path/to/guava.jar");
        assert!(result.is_some());
        let (module, entry) = result.unwrap();
        assert_eq!(module, "app");
        assert_eq!(
            entry.coordinates.as_deref(),
            Some("com.google.guava:guava:33.0.0")
        );
        assert_eq!(entry.jar_path, PathBuf::from("/path/to/guava.jar"));
        assert!(entry.is_direct);
        assert!(entry.source_jar.is_none());
    }

    #[test]
    fn test_parse_cp_line_too_few_parts() {
        assert!(parse_cp_line("app:group:name").is_none());
        assert!(parse_cp_line("app:group:name:version").is_none());
        assert!(parse_cp_line("").is_none());
    }

    #[test]
    fn test_parse_cp_line_empty_fields() {
        assert!(parse_cp_line(":group:name:version:/path").is_none());
        assert!(parse_cp_line("app::name:version:/path").is_none());
        assert!(parse_cp_line("app:group::version:/path").is_none());
        assert!(parse_cp_line("app:group:name::/path").is_none());
        assert!(parse_cp_line("app:group:name:version:").is_none());
    }

    // -----------------------------------------------------------------------
    // Helper: environment variable guard for tests
    // -----------------------------------------------------------------------

    /// RAII guard that sets an environment variable and restores the original
    /// value when dropped.
    struct EnvGuard {
        key: String,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            // Safety: test-only, scoped via RAII guard.
            unsafe {
                std::env::set_var(key, value);
            }
            Self {
                key: key.to_string(),
                original,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // Safety: test-only, restoring original env state.
            unsafe {
                match &self.original {
                    Some(val) => std::env::set_var(&self.key, val),
                    None => std::env::remove_var(&self.key),
                }
            }
        }
    }
}
