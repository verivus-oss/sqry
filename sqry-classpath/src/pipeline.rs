//! Classpath pipeline orchestration.
//!
//! Coordinates the full classpath analysis pipeline:
//! detect → resolve → scan/cache → build index → emit graph nodes.
//!
//! This module is the single integration point called from the CLI when the
//! `jvm-classpath` feature is enabled.

// Classpath scan metrics fit in u32; casts are intentional
#![allow(clippy::cast_possible_truncation)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use log::{debug, info, warn};
use rayon::prelude::*;

use crate::bytecode::scan_jar;
use crate::detect::{BuildSystem, discover_build_roots};
use crate::graph::provenance::{ClasspathProvenance, ClasspathScope};
use crate::resolve::{ClasspathEntry, ResolveConfig, ResolvedClasspath};
use crate::stub::cache::StubCache;
use crate::stub::index::ClasspathIndex;
use crate::stub::model::ClassStub;
use crate::{ClasspathError, ClasspathResult};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the classpath pipeline.
#[derive(Debug, Clone)]
pub struct ClasspathConfig {
    /// Whether classpath analysis is enabled.
    pub enabled: bool,
    /// Depth of classpath analysis.
    pub depth: ClasspathDepth,
    /// Override build system (from `--build-system` flag).
    pub build_system_override: Option<String>,
    /// Manual classpath file (from `--classpath-file` flag).
    ///
    /// When set, skips build system detection and resolution entirely.
    /// The file should contain one JAR path per line.
    pub classpath_file: Option<PathBuf>,
    /// Whether to force classpath resolution even if cached.
    pub force: bool,
    /// Subprocess timeout in seconds for build tool resolution.
    pub timeout_secs: u64,
}

/// Depth of classpath analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClasspathDepth {
    /// Only direct dependencies.
    Shallow,
    /// All transitive dependencies.
    Full,
}

impl Default for ClasspathConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            depth: ClasspathDepth::Full,
            build_system_override: None,
            classpath_file: None,
            force: false,
            timeout_secs: 60,
        }
    }
}

// ---------------------------------------------------------------------------
// Pipeline result
// ---------------------------------------------------------------------------

/// Result of the classpath pipeline.
#[derive(Debug)]
pub struct ClasspathPipelineResult {
    /// The built classpath index.
    pub index: ClasspathIndex,
    /// Provenance information for each JAR.
    pub provenance: Vec<ClasspathProvenance>,
    /// Resolved classpaths grouped by module/root scope.
    pub resolved_classpaths: Vec<ResolvedClasspath>,
    /// Number of JARs scanned.
    pub jars_scanned: usize,
    /// Number of classes parsed.
    pub classes_parsed: usize,
    /// Whether results came from cache.
    pub from_cache: bool,
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Run the full classpath pipeline: detect → resolve → scan/cache → build index.
///
/// This is the main entry point called from the CLI when classpath analysis
/// is enabled. The returned [`ClasspathPipelineResult`] contains the
/// [`ClasspathIndex`] and provenance data needed by the graph emitter.
///
/// # Steps
///
/// 1. **Detect** the build system (or use the override / manual file).
/// 2. **Resolve** the classpath via the appropriate build tool resolver.
/// 3. **Scan** each JAR file for `.class` entries, using the [`StubCache`]
///    for incremental re-use. JARs are scanned in parallel via rayon.
/// 4. **Build** a merged [`ClasspathIndex`] from all collected stubs.
/// 5. **Persist** the index and provenance to `.sqry/classpath/` for
///    subsequent builds that skip the resolve step.
///
/// # Errors
///
/// Returns [`ClasspathError`] if detection, resolution, scanning, or
/// persistence fails.
pub fn run_classpath_pipeline(
    project_root: &Path,
    config: &ClasspathConfig,
) -> ClasspathResult<ClasspathPipelineResult> {
    info!("Starting classpath pipeline for {}", project_root.display());

    // ── Step 1: Resolve classpath entries ───────────────────────────────
    let resolved_classpaths = if let Some(ref classpath_file) = config.classpath_file {
        resolve_from_manual_file(project_root, classpath_file)?
    } else {
        resolve_from_build_system(project_root, config)?
    };

    // Flatten all entries across modules.
    let all_entries: Vec<&ClasspathEntry> = resolved_classpaths
        .iter()
        .flat_map(|cp| &cp.entries)
        .collect();

    // Apply depth filtering.
    let entries_to_scan: Vec<&ClasspathEntry> = match config.depth {
        ClasspathDepth::Full => all_entries,
        ClasspathDepth::Shallow => all_entries.into_iter().filter(|e| e.is_direct).collect(),
    };

    info!(
        "Classpath resolved: {} entries ({} after depth filtering)",
        resolved_classpaths
            .iter()
            .map(|cp| cp.entries.len())
            .sum::<usize>(),
        entries_to_scan.len(),
    );

    // Deduplicate by JAR path (same JAR may appear in multiple modules).
    let unique_jar_paths = deduplicate_jar_paths(&entries_to_scan);
    info!("{} unique JAR files to scan", unique_jar_paths.len());

    // ── Step 2: Scan JARs (parallel, with stub cache) ──────────────────
    let stub_cache = StubCache::new(project_root);
    let scan_results = scan_jars_parallel(&unique_jar_paths, &stub_cache, config.force);

    let mut all_stubs: Vec<ClassStub> = Vec::new();
    let mut jars_scanned: usize = 0;
    let mut jars_from_cache: usize = 0;

    for result in &scan_results {
        match result {
            JarScanOutcome::Scanned { jar_path, stubs } => {
                let jar_str = jar_path.display().to_string();
                for stub in stubs {
                    let mut s = stub.clone();
                    // Ensure source_jar is set even if scan_jar already set it,
                    // and for cached stubs that may predate the field.
                    if s.source_jar.is_none() {
                        s.source_jar = Some(jar_str.clone());
                    }
                    all_stubs.push(s);
                }
                jars_scanned += 1;
            }
            JarScanOutcome::Cached { jar_path, stubs } => {
                let jar_str = jar_path.display().to_string();
                for stub in stubs {
                    let mut s = stub.clone();
                    if s.source_jar.is_none() {
                        s.source_jar = Some(jar_str.clone());
                    }
                    all_stubs.push(s);
                }
                jars_from_cache += 1;
            }
            JarScanOutcome::Failed { jar_path, error } => {
                warn!("Failed to scan JAR {}: {error}", jar_path.display());
            }
        }
    }

    let classes_parsed = all_stubs.len();
    info!(
        "Scanned {} JARs ({} from cache, {} fresh), {} classes total",
        jars_scanned + jars_from_cache,
        jars_from_cache,
        jars_scanned,
        classes_parsed,
    );

    // ── Step 3: Build provenance ───────────────────────────────────────
    let provenance = build_provenance(&resolved_classpaths, config.depth);

    // ── Step 4: Build index ────────────────────────────────────────────
    let index = ClasspathIndex::build(all_stubs);
    info!(
        "Built classpath index: {} classes, {} packages",
        index.classes.len(),
        index.package_index.len(),
    );

    // ── Step 5: Persist index and provenance ───────────────────────────
    let sqry_classpath_dir = project_root.join(".sqry").join("classpath");
    persist_artifacts(&sqry_classpath_dir, &index, &provenance)?;

    Ok(ClasspathPipelineResult {
        index,
        provenance,
        resolved_classpaths,
        jars_scanned: jars_scanned + jars_from_cache,
        classes_parsed,
        from_cache: jars_from_cache > 0 && jars_scanned == 0,
    })
}

// ---------------------------------------------------------------------------
// Resolution strategies
// ---------------------------------------------------------------------------

/// Read a manual classpath file (one JAR path per line).
///
/// Lines that are empty or start with `#` are skipped (comments).
fn resolve_from_manual_file(
    project_root: &Path,
    classpath_file: &Path,
) -> ClasspathResult<Vec<ResolvedClasspath>> {
    info!("Reading manual classpath from {}", classpath_file.display());

    let file = std::fs::File::open(classpath_file).map_err(|e| {
        ClasspathError::ResolutionFailed(format!(
            "Cannot open classpath file {}: {e}",
            classpath_file.display()
        ))
    })?;

    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    for line in reader.lines() {
        let line = line.map_err(|e| {
            ClasspathError::ResolutionFailed(format!(
                "Error reading classpath file {}: {e}",
                classpath_file.display()
            ))
        })?;
        let trimmed = line.trim();

        // Skip empty lines and comments.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let jar_path = PathBuf::from(trimmed);
        if !jar_path.exists() {
            warn!(
                "Classpath file entry does not exist: {}",
                jar_path.display()
            );
            // Still include it — the scanner will report the error.
        }

        entries.push(ClasspathEntry {
            jar_path,
            coordinates: None,
            is_direct: true, // Manual entries treated as direct.
            source_jar: None,
        });
    }

    info!("Manual classpath file: {} entries", entries.len());

    Ok(vec![ResolvedClasspath {
        module_name: "manual".to_string(),
        module_root: project_root.to_path_buf(),
        entries,
    }])
}

/// Detect the build system and resolve the classpath via the appropriate resolver.
fn resolve_from_build_system(
    project_root: &Path,
    config: &ClasspathConfig,
) -> ClasspathResult<Vec<ResolvedClasspath>> {
    let detected_roots =
        discover_build_roots(project_root, config.build_system_override.as_deref());
    if detected_roots.is_empty() {
        return Err(ClasspathError::DetectionFailed(
            "No JVM build system detected. Use --build-system to specify one, \
             or --classpath-file to provide a manual classpath."
                .to_string(),
        ));
    }

    info!("Discovered {} JVM build roots", detected_roots.len());
    let mut resolved = Vec::new();
    for detection in detected_roots {
        let Some(build_system) = detection.build_system else {
            continue;
        };
        info!(
            "Resolving {:?} classpath in {}",
            build_system,
            detection.project_root.display()
        );

        let resolve_config = ResolveConfig {
            project_root: detection.project_root.clone(),
            timeout_secs: config.timeout_secs,
            cache_path: Some(detection.project_root.join(".sqry").join("classpath")),
        };

        let mut root_resolved = match build_system {
            BuildSystem::Gradle => {
                crate::resolve::gradle::resolve_gradle_classpath(&resolve_config)
            }
            BuildSystem::Maven => crate::resolve::maven::resolve_maven_classpath(&resolve_config),
            BuildSystem::Bazel => crate::resolve::bazel::resolve_bazel_classpath(&resolve_config),
            BuildSystem::Sbt => crate::resolve::sbt::resolve_sbt_classpath(&resolve_config),
        }?;
        resolved.append(&mut root_resolved);
    }

    resolved.sort_by(|a, b| {
        a.module_root
            .cmp(&b.module_root)
            .then_with(|| a.module_name.cmp(&b.module_name))
    });
    Ok(resolved)
}

// ---------------------------------------------------------------------------
// JAR scanning
// ---------------------------------------------------------------------------

/// Outcome of scanning a single JAR file.
enum JarScanOutcome {
    /// JAR was freshly scanned and parsed.
    Scanned {
        #[allow(dead_code)] // Used in tests for pattern matching.
        jar_path: PathBuf,
        stubs: Vec<ClassStub>,
    },
    /// Stubs were loaded from the stub cache (JAR hash matched).
    Cached {
        #[allow(dead_code)] // Used in tests for pattern matching.
        jar_path: PathBuf,
        stubs: Vec<ClassStub>,
    },
    /// JAR could not be scanned.
    Failed { jar_path: PathBuf, error: String },
}

/// Deduplicate JAR paths, preserving the first occurrence.
fn deduplicate_jar_paths(entries: &[&ClasspathEntry]) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::new();

    for entry in entries {
        if seen.insert(&entry.jar_path) {
            unique.push(entry.jar_path.clone());
        }
    }

    unique
}

/// Scan JAR files in parallel using rayon, with stub cache for incremental builds.
///
/// Each JAR is either loaded from the stub cache (if the JAR's SHA-256 hash
/// matches a cached entry) or freshly scanned. Freshly scanned stubs are
/// written to the cache for future use.
fn scan_jars_parallel(
    jar_paths: &[PathBuf],
    stub_cache: &StubCache,
    force: bool,
) -> Vec<JarScanOutcome> {
    jar_paths
        .par_iter()
        .map(|jar_path| scan_single_jar(jar_path, stub_cache, force))
        .collect()
}

/// Scan a single JAR file, using the stub cache when possible.
fn scan_single_jar(jar_path: &Path, stub_cache: &StubCache, force: bool) -> JarScanOutcome {
    // Try cache first (unless force is set).
    if !force && let Some(cached_stubs) = stub_cache.get(jar_path) {
        debug!(
            "Cache hit for {} ({} stubs)",
            jar_path.display(),
            cached_stubs.len()
        );
        return JarScanOutcome::Cached {
            jar_path: jar_path.to_path_buf(),
            stubs: cached_stubs,
        };
    }

    // Fresh scan.
    match scan_jar(jar_path) {
        Ok(stubs) => {
            debug!("Scanned {} ({} classes)", jar_path.display(), stubs.len());

            // Write to cache (non-fatal on error).
            if let Err(e) = stub_cache.put(jar_path, &stubs) {
                warn!("Failed to cache stubs for {}: {e}", jar_path.display());
            }

            JarScanOutcome::Scanned {
                jar_path: jar_path.to_path_buf(),
                stubs,
            }
        }
        Err(e) => JarScanOutcome::Failed {
            jar_path: jar_path.to_path_buf(),
            error: e.to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// Provenance construction
// ---------------------------------------------------------------------------

/// Build provenance records from classpath entries.
fn build_provenance(
    resolved_classpaths: &[ResolvedClasspath],
    depth: ClasspathDepth,
) -> Vec<ClasspathProvenance> {
    let mut by_jar: std::collections::HashMap<PathBuf, ClasspathProvenance> =
        std::collections::HashMap::new();

    for classpath in resolved_classpaths {
        for entry in &classpath.entries {
            if matches!(depth, ClasspathDepth::Shallow) && !entry.is_direct {
                continue;
            }

            let provenance =
                by_jar
                    .entry(entry.jar_path.clone())
                    .or_insert_with(|| ClasspathProvenance {
                        jar_path: entry.jar_path.clone(),
                        coordinates: entry.coordinates.clone(),
                        is_direct: entry.is_direct,
                        scopes: Vec::new(),
                    });

            if provenance.coordinates.is_none() {
                provenance.coordinates.clone_from(&entry.coordinates);
            }
            provenance.is_direct &= entry.is_direct;

            let scope = ClasspathScope {
                module_name: classpath.module_name.clone(),
                module_root: classpath.module_root.clone(),
                is_direct: entry.is_direct,
            };
            if !provenance.scopes.iter().any(|existing| existing == &scope) {
                provenance.scopes.push(scope);
            }
        }
    }

    let mut result: Vec<_> = by_jar.into_values().collect();
    result.sort_by(|a, b| a.jar_path.cmp(&b.jar_path));
    result
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Persist the classpath index and provenance to `.sqry/classpath/`.
fn persist_artifacts(
    classpath_dir: &Path,
    index: &ClasspathIndex,
    provenance: &[ClasspathProvenance],
) -> ClasspathResult<()> {
    std::fs::create_dir_all(classpath_dir).map_err(|e| {
        ClasspathError::IndexError(format!(
            "Cannot create classpath directory {}: {e}",
            classpath_dir.display()
        ))
    })?;

    // Persist index.
    let index_path = classpath_dir.join("index.sqry");
    index.save(&index_path)?;
    info!("Saved classpath index to {}", index_path.display());

    // Persist provenance.
    let provenance_path = classpath_dir.join("provenance.json");
    let provenance_json = serde_json::to_string_pretty(provenance)
        .map_err(|e| ClasspathError::IndexError(format!("Cannot serialize provenance: {e}")))?;
    std::fs::write(&provenance_path, provenance_json).map_err(|e| {
        ClasspathError::IndexError(format!(
            "Cannot write provenance to {}: {e}",
            provenance_path.display()
        ))
    })?;
    info!("Saved provenance to {}", provenance_path.display());

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;

    /// Build a minimal valid .class file for testing.
    fn build_minimal_class(class_name: &str) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Magic
        bytes.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
        // Minor version
        bytes.extend_from_slice(&0u16.to_be_bytes());
        // Major version (52 = Java 8)
        bytes.extend_from_slice(&52u16.to_be_bytes());

        // Constant pool: 5 entries
        let class_bytes = class_name.as_bytes();
        let object_bytes = b"java/lang/Object";

        let cp_count: u16 = 5;
        bytes.extend_from_slice(&cp_count.to_be_bytes());

        // #1: CONSTANT_Utf8 <class_name>
        bytes.push(1);
        bytes.extend_from_slice(&(class_bytes.len() as u16).to_be_bytes());
        bytes.extend_from_slice(class_bytes);

        // #2: CONSTANT_Class -> #1
        bytes.push(7);
        bytes.extend_from_slice(&1u16.to_be_bytes());

        // #3: CONSTANT_Utf8 "java/lang/Object"
        bytes.push(1);
        bytes.extend_from_slice(&(object_bytes.len() as u16).to_be_bytes());
        bytes.extend_from_slice(object_bytes);

        // #4: CONSTANT_Class -> #3
        bytes.push(7);
        bytes.extend_from_slice(&3u16.to_be_bytes());

        // Access flags: ACC_PUBLIC | ACC_SUPER
        bytes.extend_from_slice(&0x0021u16.to_be_bytes());
        // This class: #2
        bytes.extend_from_slice(&2u16.to_be_bytes());
        // Super class: #4
        bytes.extend_from_slice(&4u16.to_be_bytes());
        // Interfaces count: 0
        bytes.extend_from_slice(&0u16.to_be_bytes());
        // Fields count: 0
        bytes.extend_from_slice(&0u16.to_be_bytes());
        // Methods count: 0
        bytes.extend_from_slice(&0u16.to_be_bytes());
        // Attributes count: 0
        bytes.extend_from_slice(&0u16.to_be_bytes());

        bytes
    }

    /// Create an in-memory JAR (ZIP) file containing test classes.
    fn build_test_jar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for (name, data) in entries {
                writer.start_file(*name, options).unwrap();
                writer.write_all(data).unwrap();
            }
            writer.finish().unwrap();
        }
        buf
    }

    /// Write a test JAR file to disk and return its path.
    fn write_test_jar(dir: &Path, name: &str, classes: &[(&str, &[u8])]) -> PathBuf {
        let jar_bytes = build_test_jar(classes);
        let jar_path = dir.join(name);
        std::fs::write(&jar_path, &jar_bytes).unwrap();
        jar_path
    }

    // ── Default config tests ───────────────────────────────────────────

    #[test]
    fn test_default_config() {
        let config = ClasspathConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.depth, ClasspathDepth::Full);
        assert!(config.build_system_override.is_none());
        assert!(config.classpath_file.is_none());
        assert!(!config.force);
        assert_eq!(config.timeout_secs, 60);
    }

    // ── Manual classpath file tests ────────────────────────────────────

    #[test]
    fn test_resolve_from_manual_file_basic() {
        let tmp = TempDir::new().unwrap();

        // Create some fake JAR files.
        let jar_a = tmp.path().join("a.jar");
        let jar_b = tmp.path().join("b.jar");
        std::fs::write(&jar_a, b"fake jar a").unwrap();
        std::fs::write(&jar_b, b"fake jar b").unwrap();

        // Write classpath file.
        let cp_file = tmp.path().join("classpath.txt");
        std::fs::write(
            &cp_file,
            format!("{}\n{}\n", jar_a.display(), jar_b.display()),
        )
        .unwrap();

        let result = resolve_from_manual_file(tmp.path(), &cp_file).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].module_name, "manual");
        assert_eq!(result[0].module_root, tmp.path());
        assert_eq!(result[0].entries.len(), 2);
        assert!(result[0].entries[0].is_direct);
        assert!(result[0].entries[1].is_direct);
    }

    #[test]
    fn test_resolve_from_manual_file_skips_comments_and_blanks() {
        let tmp = TempDir::new().unwrap();
        let jar_a = tmp.path().join("a.jar");
        std::fs::write(&jar_a, b"fake jar a").unwrap();

        let cp_file = tmp.path().join("classpath.txt");
        std::fs::write(
            &cp_file,
            format!(
                "# This is a comment\n\n{}\n\n# Another comment\n",
                jar_a.display()
            ),
        )
        .unwrap();

        let result = resolve_from_manual_file(tmp.path(), &cp_file).unwrap();
        assert_eq!(result[0].entries.len(), 1);
    }

    #[test]
    fn test_resolve_from_manual_file_nonexistent_file() {
        let result =
            resolve_from_manual_file(Path::new("/tmp"), Path::new("/nonexistent/classpath.txt"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Cannot open classpath file"));
    }

    #[test]
    fn test_resolve_from_manual_file_nonexistent_jars_included() {
        let tmp = TempDir::new().unwrap();
        let cp_file = tmp.path().join("classpath.txt");
        std::fs::write(&cp_file, "/nonexistent/jar.jar\n").unwrap();

        let result = resolve_from_manual_file(tmp.path(), &cp_file).unwrap();
        assert_eq!(result[0].entries.len(), 1);
        assert_eq!(
            result[0].entries[0].jar_path,
            PathBuf::from("/nonexistent/jar.jar")
        );
    }

    // ── Deduplication tests ────────────────────────────────────────────

    #[test]
    fn test_deduplicate_jar_paths() {
        let entries = vec![
            ClasspathEntry {
                jar_path: PathBuf::from("/a.jar"),
                coordinates: None,
                is_direct: true,
                source_jar: None,
            },
            ClasspathEntry {
                jar_path: PathBuf::from("/b.jar"),
                coordinates: None,
                is_direct: true,
                source_jar: None,
            },
            ClasspathEntry {
                jar_path: PathBuf::from("/a.jar"),
                coordinates: None,
                is_direct: false,
                source_jar: None,
            },
        ];
        let refs: Vec<&ClasspathEntry> = entries.iter().collect();
        let unique = deduplicate_jar_paths(&refs);
        assert_eq!(unique.len(), 2);
        assert_eq!(unique[0], PathBuf::from("/a.jar"));
        assert_eq!(unique[1], PathBuf::from("/b.jar"));
    }

    // ── Provenance construction tests ──────────────────────────────────

    #[test]
    fn test_build_provenance() {
        let classpaths = vec![ResolvedClasspath {
            module_name: "app".to_string(),
            module_root: PathBuf::from("/repo/app"),
            entries: vec![
                ClasspathEntry {
                    jar_path: PathBuf::from("/guava.jar"),
                    coordinates: Some("com.google.guava:guava:33.0.0".to_string()),
                    is_direct: true,
                    source_jar: None,
                },
                ClasspathEntry {
                    jar_path: PathBuf::from("/commons.jar"),
                    coordinates: None,
                    is_direct: false,
                    source_jar: None,
                },
            ],
        }];
        let prov = build_provenance(&classpaths, ClasspathDepth::Full);

        assert_eq!(prov.len(), 2);
        assert_eq!(prov[0].jar_path, PathBuf::from("/commons.jar"));
        assert_eq!(
            prov[1].coordinates,
            Some("com.google.guava:guava:33.0.0".to_string())
        );
        assert!(!prov[0].is_direct);
        assert!(prov[1].is_direct);
        assert!(prov[0].coordinates.is_none());
        assert_eq!(prov[1].scopes[0].module_root, PathBuf::from("/repo/app"));
    }

    #[test]
    fn test_build_provenance_mixed_directness_same_jar_is_conservative() {
        let shared_jar = PathBuf::from("/shared.jar");
        let classpaths = vec![
            ResolvedClasspath {
                module_name: "app".to_string(),
                module_root: PathBuf::from("/repo/app"),
                entries: vec![ClasspathEntry {
                    jar_path: shared_jar.clone(),
                    coordinates: Some("com.example:shared:1.0.0".to_string()),
                    is_direct: true,
                    source_jar: None,
                }],
            },
            ResolvedClasspath {
                module_name: "worker".to_string(),
                module_root: PathBuf::from("/repo/worker"),
                entries: vec![ClasspathEntry {
                    jar_path: shared_jar.clone(),
                    coordinates: Some("com.example:shared:1.0.0".to_string()),
                    is_direct: false,
                    source_jar: None,
                }],
            },
        ];
        let prov = build_provenance(&classpaths, ClasspathDepth::Full);

        assert_eq!(prov.len(), 1);
        assert_eq!(prov[0].jar_path, shared_jar);
        assert!(
            !prov[0].is_direct,
            "aggregate directness should be conservative when scopes disagree"
        );
        assert!(
            prov[0].has_direct_scope(),
            "per-scope metadata should retain the direct scope"
        );
        assert_eq!(prov[0].scopes.len(), 2);
    }

    // ── Scan + cache integration tests ─────────────────────────────────

    #[test]
    fn test_scan_single_jar_fresh() {
        let tmp = TempDir::new().unwrap();
        let class_a = build_minimal_class("com/example/Foo");
        let jar_path = write_test_jar(
            tmp.path(),
            "test.jar",
            &[("com/example/Foo.class", &class_a)],
        );

        let cache = StubCache::new(tmp.path());
        let outcome = scan_single_jar(&jar_path, &cache, false);

        match outcome {
            JarScanOutcome::Scanned { stubs, .. } => {
                assert_eq!(stubs.len(), 1);
                assert_eq!(stubs[0].fqn, "com.example.Foo");
            }
            other => panic!("Expected Scanned, got {:?}", outcome_name(&other)),
        }
    }

    #[test]
    fn test_scan_single_jar_cached() {
        let tmp = TempDir::new().unwrap();
        let class_a = build_minimal_class("com/example/Bar");
        let jar_path = write_test_jar(
            tmp.path(),
            "test.jar",
            &[("com/example/Bar.class", &class_a)],
        );

        let cache = StubCache::new(tmp.path());

        // First scan populates cache.
        let outcome = scan_single_jar(&jar_path, &cache, false);
        assert!(matches!(outcome, JarScanOutcome::Scanned { .. }));

        // Second scan should hit cache.
        let outcome = scan_single_jar(&jar_path, &cache, false);
        match outcome {
            JarScanOutcome::Cached { stubs, .. } => {
                assert_eq!(stubs.len(), 1);
                assert_eq!(stubs[0].fqn, "com.example.Bar");
            }
            other => panic!("Expected Cached, got {:?}", outcome_name(&other)),
        }
    }

    #[test]
    fn test_scan_single_jar_force_bypasses_cache() {
        let tmp = TempDir::new().unwrap();
        let class_a = build_minimal_class("com/example/Baz");
        let jar_path = write_test_jar(
            tmp.path(),
            "test.jar",
            &[("com/example/Baz.class", &class_a)],
        );

        let cache = StubCache::new(tmp.path());

        // Populate cache.
        let _ = scan_single_jar(&jar_path, &cache, false);

        // Force should bypass cache.
        let outcome = scan_single_jar(&jar_path, &cache, true);
        assert!(
            matches!(outcome, JarScanOutcome::Scanned { .. }),
            "force=true should bypass cache"
        );
    }

    #[test]
    fn test_scan_single_jar_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let cache = StubCache::new(tmp.path());
        let outcome = scan_single_jar(Path::new("/nonexistent.jar"), &cache, false);
        assert!(
            matches!(outcome, JarScanOutcome::Failed { .. }),
            "Should fail for nonexistent JAR"
        );
    }

    // ── Parallel scan tests ────────────────────────────────────────────

    #[test]
    #[allow(clippy::match_same_arms)] // Arms separated for documentation clarity
    #[allow(clippy::match_wildcard_for_single_variants)] // Wildcard covers future variants
    fn test_scan_jars_parallel_multiple() {
        let tmp = TempDir::new().unwrap();
        let class_a = build_minimal_class("com/example/A");
        let class_b = build_minimal_class("com/example/B");

        let jar_a = write_test_jar(tmp.path(), "a.jar", &[("com/example/A.class", &class_a)]);
        let jar_b = write_test_jar(tmp.path(), "b.jar", &[("com/example/B.class", &class_b)]);

        let cache = StubCache::new(tmp.path());
        let results = scan_jars_parallel(&[jar_a, jar_b], &cache, false);

        assert_eq!(results.len(), 2);
        let total_stubs: usize = results
            .iter()
            .filter_map(|r| match r {
                #[allow(clippy::match_same_arms)] // Pipeline stage arms separated for traceability
                JarScanOutcome::Scanned { stubs, .. } | JarScanOutcome::Cached { stubs, .. } => {
                    Some(stubs.len())
                }
                _ => None,
            })
            .sum();
        assert_eq!(total_stubs, 2);
    }

    // ── Persistence tests ──────────────────────────────────────────────

    #[test]
    fn test_persist_artifacts_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let classpath_dir = tmp.path().join("classpath");

        let index = ClasspathIndex::build(vec![]);
        let provenance = vec![ClasspathProvenance {
            jar_path: PathBuf::from("/test.jar"),
            coordinates: Some("test:test:1.0".to_string()),
            is_direct: true,
            scopes: vec![ClasspathScope {
                module_name: "manual".to_string(),
                module_root: tmp.path().to_path_buf(),
                is_direct: true,
            }],
        }];

        persist_artifacts(&classpath_dir, &index, &provenance).unwrap();

        // Verify index file exists and is loadable.
        let index_path = classpath_dir.join("index.sqry");
        assert!(index_path.exists());
        let loaded_index = ClasspathIndex::load(&index_path).unwrap();
        assert_eq!(loaded_index.classes.len(), 0);

        // Verify provenance file exists and is valid JSON.
        let prov_path = classpath_dir.join("provenance.json");
        assert!(prov_path.exists());
        let prov_json = std::fs::read_to_string(&prov_path).unwrap();
        let loaded_prov: Vec<ClasspathProvenance> = serde_json::from_str(&prov_json).unwrap();
        assert_eq!(loaded_prov.len(), 1);
        assert_eq!(
            loaded_prov[0].coordinates,
            Some("test:test:1.0".to_string())
        );
    }

    // ── Depth filtering tests ──────────────────────────────────────────

    #[test]
    fn test_depth_shallow_filters_transitive() {
        let tmp = TempDir::new().unwrap();

        let class_d = build_minimal_class("com/example/Direct");
        let class_t = build_minimal_class("com/example/Transitive");

        let jar_d = write_test_jar(
            tmp.path(),
            "direct.jar",
            &[("com/example/Direct.class", &class_d)],
        );
        let jar_t = write_test_jar(
            tmp.path(),
            "transitive.jar",
            &[("com/example/Transitive.class", &class_t)],
        );

        // Write a manual classpath file.
        let cp_file = tmp.path().join("classpath.txt");
        std::fs::write(
            &cp_file,
            format!("{}\n{}\n", jar_d.display(), jar_t.display()),
        )
        .unwrap();

        // Manually create resolved classpaths with mixed direct/transitive.
        let entries = [
            ClasspathEntry {
                jar_path: jar_d,
                coordinates: None,
                is_direct: true,
                source_jar: None,
            },
            ClasspathEntry {
                jar_path: jar_t,
                coordinates: None,
                is_direct: false,
                source_jar: None,
            },
        ];
        let all_refs: Vec<&ClasspathEntry> = entries.iter().collect();

        // Full depth should include both.
        let full: Vec<&ClasspathEntry> = all_refs.clone();
        assert_eq!(full.len(), 2);

        // Shallow depth should only include direct.
        let shallow: Vec<&ClasspathEntry> = all_refs.into_iter().filter(|e| e.is_direct).collect();
        assert_eq!(shallow.len(), 1);
        assert!(shallow[0].is_direct);
    }

    // ── Full pipeline test with manual file ────────────────────────────

    #[test]
    fn test_full_pipeline_with_manual_file() {
        let tmp = TempDir::new().unwrap();

        let class_a = build_minimal_class("com/example/Alpha");
        let class_b = build_minimal_class("com/example/Beta");

        let jar_path = write_test_jar(
            tmp.path(),
            "deps.jar",
            &[
                ("com/example/Alpha.class", &class_a),
                ("com/example/Beta.class", &class_b),
            ],
        );

        // Write classpath file.
        let cp_file = tmp.path().join("classpath.txt");
        std::fs::write(&cp_file, format!("{}\n", jar_path.display())).unwrap();

        let config = ClasspathConfig {
            enabled: true,
            depth: ClasspathDepth::Full,
            build_system_override: None,
            classpath_file: Some(cp_file),
            force: false,
            timeout_secs: 30,
        };

        let result = run_classpath_pipeline(tmp.path(), &config).unwrap();
        assert_eq!(result.jars_scanned, 1);
        assert_eq!(result.classes_parsed, 2);
        assert_eq!(result.index.classes.len(), 2);
        assert!(result.index.lookup_fqn("com.example.Alpha").is_some());
        assert!(result.index.lookup_fqn("com.example.Beta").is_some());
        assert_eq!(result.provenance.len(), 1);

        // Verify persistence.
        let index_path = tmp.path().join(".sqry/classpath/index.sqry");
        assert!(index_path.exists());
        let prov_path = tmp.path().join(".sqry/classpath/provenance.json");
        assert!(prov_path.exists());
    }

    #[test]
    fn test_pipeline_no_build_system_returns_error() {
        let tmp = TempDir::new().unwrap();
        let config = ClasspathConfig {
            enabled: true,
            ..ClasspathConfig::default()
        };

        let result = run_classpath_pipeline(tmp.path(), &config);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("No JVM build system detected"),
            "Expected detection error, got: {err}"
        );
    }

    // ── Helper for test output ─────────────────────────────────────────

    fn outcome_name(outcome: &JarScanOutcome) -> &'static str {
        match outcome {
            JarScanOutcome::Scanned { .. } => "Scanned",
            JarScanOutcome::Cached { .. } => "Cached",
            JarScanOutcome::Failed { .. } => "Failed",
        }
    }
}
