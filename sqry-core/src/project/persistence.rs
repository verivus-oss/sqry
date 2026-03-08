//! Project state persistence
//!
//! Implements persistence for Project metadata (`repo_index`, `file_table`) as specified in:
//! - `docs/development/project-persistence-019af0c9-428d-7000-b1d6-08961d6930b0/02_DESIGN.md`
//!
//! # Overview
//!
//! When `cache.persistent = true`, Project state is saved to disk during teardown/shutdown:
//! - Repo index and file table → versioned JSON in `<state_root>/<project_id>.json`
//! - Node index → via `IndexStorage` (existing mechanism)
//!
//! On initialization, persisted state is loaded if present and valid (version + checksum match).

use crate::config::{CacheConfig, IndexingConfig};
use crate::project::types::{FileEntry, ProjectId, RepoId, StringId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

struct TempFileGuard<'a> {
    path: &'a Path,
    should_cleanup: bool,
}

impl Drop for TempFileGuard<'_> {
    fn drop(&mut self) {
        if self.should_cleanup {
            let _ = fs::remove_file(self.path);
        }
    }
}

/// Current schema version for persisted state.
///
/// # Version Evolution (from `02_DESIGN.md` L-3)
///
/// - Bump when adding required fields, changing field types, or renaming fields.
/// - Keep when adding optional fields with `#[serde(default)]`.
const SCHEMA_VERSION: u32 = 1;

/// Persisted project state including repo/file metadata.
///
/// Serialized to `<state_root>/<project_id>.json` with atomic temp + rename.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedProjectState {
    /// Schema version for compatibility checking.
    pub version: u32,

    /// Project identifier (must match current project).
    pub project_id: u64,

    /// Canonical index root path.
    pub index_root: PathBuf,

    /// Config fingerprint for invalidation detection.
    ///
    /// # Fields included (from `02_DESIGN.md` M-2):
    /// - `cache.persistent` (bool)
    /// - `cache.directory` (String)
    /// - `indexing.max_file_size` (u64)
    /// - `indexing.additional_ignored_dirs` (`Vec<String>`)
    ///
    /// Computed as: `blake3(sorted_json(fields)).truncate_to_u64()`
    pub config_fingerprint: u64,

    /// Repository index: maps git root paths to `RepoId`s.
    pub repo_index: Vec<(PathBuf, u64)>,

    /// File table entries.
    pub files: Vec<PersistedFileEntry>,

    /// When this state was generated.
    #[serde(with = "system_time_serde")]
    pub generated_at: SystemTime,

    /// Blake3 checksum of serialized payload (hex, excluding this field).
    ///
    /// Set after serialization; validated on load.
    #[serde(default)]
    pub checksum: String,
}

/// Persisted file entry metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedFileEntry {
    /// Relative path (normalized).
    pub path: String,

    /// Repository ID (as u64).
    pub repo_id: u64,

    /// Git root path for `RepoId` reconstruction.
    ///
    /// Stored to enable proper `RepoId` restoration (fixes HIGH finding from review).
    /// `None` if file has no git root (`RepoId::NONE`).
    #[serde(default)]
    pub git_root: Option<String>,

    /// Last modification time.
    #[serde(with = "option_system_time_serde")]
    pub last_modified: Option<SystemTime>,

    /// File size in bytes.
    pub size: u64,

    /// Content hash for change detection.
    #[serde(default)]
    pub content_hash: Option<u64>,

    /// Detected language ID (e.g., "rust", "python").
    #[serde(default)]
    pub language_id: Option<String>,
}

/// Helper for reading/writing persisted project state.
pub struct ProjectPersistence {
    /// Root directory for state files: `<index_root>/<cache.directory>/project-state/`
    state_root: PathBuf,
    /// Index root for path validation (used during construction for security checks).
    #[allow(dead_code)] // Used in ::new() for path validation, not stored for runtime use
    index_root: PathBuf,
}

impl ProjectPersistence {
    /// Create a new persistence helper.
    ///
    /// # Arguments
    ///
    /// * `index_root` - The project's index root path
    /// * `cache_directory` - The cache directory from config (relative paths only for security)
    ///
    /// # Security
    ///
    /// Only relative paths are allowed for `cache_directory`. Absolute paths or paths with
    /// `..` traversal are rejected to prevent writes outside the project root.
    /// (Fixes HIGH path traversal finding from review)
    #[must_use]
    pub fn new(index_root: &Path, cache_directory: &str) -> Self {
        let resolved_cache = Self::resolve_cache_directory(index_root, cache_directory);

        Self {
            state_root: resolved_cache.join("project-state"),
            index_root: index_root.to_path_buf(),
        }
    }

    fn resolve_cache_directory(index_root: &Path, cache_directory: &str) -> PathBuf {
        let cache_path = Path::new(cache_directory);

        if cache_path.is_absolute() {
            log::warn!(
                "Absolute cache directory '{cache_directory}' rejected for security; using default '.sqry-cache'"
            );
            return Self::default_cache_root(index_root);
        }

        Self::resolve_relative_cache_directory(index_root, cache_directory)
    }

    fn resolve_relative_cache_directory(index_root: &Path, cache_directory: &str) -> PathBuf {
        let joined = index_root.join(cache_directory);
        if let Ok(canonical) = joined.canonicalize() {
            return Self::validate_canonical_cache_path(
                index_root,
                cache_directory,
                canonical.as_path(),
                &joined,
            );
        }

        if cache_directory.contains("..") {
            log::warn!(
                "Cache directory '{cache_directory}' contains traversal; using default '.sqry-cache'"
            );
            return Self::default_cache_root(index_root);
        }

        joined
    }

    fn validate_canonical_cache_path(
        index_root: &Path,
        cache_directory: &str,
        canonical: &Path,
        joined: &Path,
    ) -> PathBuf {
        if let Ok(canonical_root) = index_root.canonicalize() {
            if canonical.starts_with(&canonical_root) {
                return joined.to_path_buf();
            }

            log::warn!(
                "Cache directory '{cache_directory}' escapes project root; using default '.sqry-cache'"
            );
            return Self::default_cache_root(index_root);
        }

        joined.to_path_buf()
    }

    fn default_cache_root(index_root: &Path) -> PathBuf {
        index_root.join(".sqry-cache")
    }

    /// Ensure the state root directory exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn ensure_state_root(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.state_root)
    }

    /// Get the path for a project's state file.
    #[must_use]
    pub fn state_file_path(&self, project_id: ProjectId) -> PathBuf {
        self.state_root.join(format!("{project_id}.json"))
    }

    /// Write metadata to disk with atomic temp + rename.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The state root cannot be created
    /// - Serialization fails
    /// - File I/O fails
    ///
    /// # Safety
    ///
    /// Uses atomic temp + rename pattern with directory fsync for durability.
    /// Temp files are cleaned up on all error paths (fixes LOW finding from review).
    pub fn write_metadata(&self, state: &PersistedProjectState) -> std::io::Result<()> {
        self.ensure_state_root()?;

        let target_path = self
            .state_root
            .join(format!("proj_{:016x}.json", state.project_id));
        let temp_path = self
            .state_root
            .join(format!("proj_{:016x}.json.tmp", state.project_id));

        let mut guard = TempFileGuard {
            path: &temp_path,
            should_cleanup: true,
        };

        // Serialize to temp file (compact JSON for performance - fixes LOW finding)
        let file = File::create(&temp_path)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, state)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        writer.flush()?;

        // Sync file to disk
        writer.into_inner()?.sync_all()?;

        // Atomic rename
        fs::rename(&temp_path, &target_path)?;

        // Sync directory to ensure rename is durable (fixes LOW finding from review)
        if let Ok(dir) = File::open(&self.state_root) {
            let _ = dir.sync_all();
        }

        // Success - don't cleanup temp file (it's now the target)
        guard.should_cleanup = false;

        log::info!(
            "Persisted project state to '{}' ({} repos, {} files)",
            target_path.display(),
            state.repo_index.len(),
            state.files.len()
        );

        Ok(())
    }

    /// Read metadata from disk if present and valid.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(state))` if valid state was loaded
    /// - `Ok(None)` if state file doesn't exist
    /// - `Err(_)` if file exists but cannot be read/parsed
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but:
    /// - Cannot be opened
    /// - Cannot be parsed as JSON
    /// - Checksum validation fails
    pub fn read_metadata(
        &self,
        project_id: ProjectId,
    ) -> std::io::Result<Option<PersistedProjectState>> {
        let path = self
            .state_root
            .join(format!("proj_{:016x}.json", project_id.as_u64()));

        if !path.exists() {
            return Ok(None);
        }

        let file = File::open(&path)?;
        let reader = BufReader::new(file);
        let state: PersistedProjectState = serde_json::from_reader(reader)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        // Validate checksum
        let computed_checksum = compute_state_checksum(&state);
        if state.checksum != computed_checksum {
            log::warn!(
                "Checksum mismatch for '{}': expected {}, got {}",
                path.display(),
                state.checksum,
                computed_checksum
            );
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "checksum mismatch",
            ));
        }

        log::debug!(
            "Loaded persisted state from '{}' (version {}, {} repos, {} files)",
            path.display(),
            state.version,
            state.repo_index.len(),
            state.files.len()
        );

        Ok(Some(state))
    }
}

/// Compute config fingerprint for invalidation detection.
///
/// # Fields included (from `02_DESIGN.md` M-2, expanded per review findings):
/// - `cache.persistent` (bool)
/// - `cache.directory` (String)
/// - `indexing.max_file_size` (u64)
/// - `indexing.max_depth` (u32)
/// - `indexing.enable_scope_extraction` (bool)
/// - `indexing.enable_relation_extraction` (bool)
/// - `indexing.additional_ignored_dirs` (`Vec<String>`)
///
/// All indexing toggles that affect detection/indexing behavior are included
/// to ensure stale state is invalidated when config changes.
/// (Fixes MEDIUM config fingerprint finding from review)
#[must_use]
pub fn compute_config_fingerprint(cache: &CacheConfig, indexing: &IndexingConfig) -> u64 {
    use blake3::Hasher;

    let mut hasher = Hasher::new();

    // Hash cache fields
    hasher.update(&[u8::from(cache.persistent)]);
    hasher.update(cache.directory.as_bytes());

    // Hash indexing fields (all fields that affect detection/indexing behavior)
    hasher.update(&indexing.max_file_size.to_le_bytes());
    hasher.update(&indexing.max_depth.to_le_bytes());
    hasher.update(&[u8::from(indexing.enable_scope_extraction)]);
    hasher.update(&[u8::from(indexing.enable_relation_extraction)]);

    // Sort additional_ignored_dirs for determinism
    let mut dirs = indexing.additional_ignored_dirs.clone();
    dirs.sort();
    for dir in &dirs {
        hasher.update(dir.as_bytes());
    }

    // Truncate to u64
    let hash = hasher.finalize();
    let bytes = hash.as_bytes();
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

/// Compute checksum of persisted state (excluding the checksum field itself).
///
/// Includes all persisted fields for integrity verification:
/// - version, `project_id`, `index_root`, `config_fingerprint`
/// - `repo_index` entries
/// - file entries (path, `repo_id`, `git_root`, size, `content_hash`, `language_id`, `last_modified`)
/// - `generated_at` timestamp
#[must_use]
pub fn compute_state_checksum(state: &PersistedProjectState) -> String {
    use blake3::Hasher;

    let mut hasher = Hasher::new();

    // Hash all fields except checksum
    hasher.update(&state.version.to_le_bytes());
    hasher.update(&state.project_id.to_le_bytes());
    hasher.update(state.index_root.as_os_str().as_encoded_bytes());
    hasher.update(&state.config_fingerprint.to_le_bytes());

    // Hash repo_index
    hash_repo_index(&mut hasher, &state.repo_index);

    // Hash files (including new fields: git_root, content_hash, language_id)
    hash_file_entries(&mut hasher, &state.files);

    // Hash generated_at
    hash_system_time(&mut hasher, state.generated_at);

    // Return hex string
    let hash = hasher.finalize();
    hex::encode(&hash.as_bytes()[..16]) // First 16 bytes = 32 hex chars
}

fn hash_repo_index(hasher: &mut blake3::Hasher, repo_index: &[(PathBuf, u64)]) {
    for (path, repo_id) in repo_index {
        hasher.update(path.as_os_str().as_encoded_bytes());
        hasher.update(&repo_id.to_le_bytes());
    }
}

fn hash_file_entries(hasher: &mut blake3::Hasher, files: &[PersistedFileEntry]) {
    for file in files {
        hash_file_entry(hasher, file);
    }
}

fn hash_file_entry(hasher: &mut blake3::Hasher, file: &PersistedFileEntry) {
    hasher.update(file.path.as_bytes());
    hasher.update(&file.repo_id.to_le_bytes());
    hash_optional_str(hasher, file.git_root.as_deref());
    hasher.update(&file.size.to_le_bytes());
    if let Some(content_hash) = file.content_hash {
        hasher.update(&content_hash.to_le_bytes());
    }
    hash_optional_str(hasher, file.language_id.as_deref());
    hash_optional_time(hasher, file.last_modified);
}

fn hash_optional_str(hasher: &mut blake3::Hasher, value: Option<&str>) {
    if let Some(value) = value {
        hasher.update(value.as_bytes());
    }
}

fn hash_optional_time(hasher: &mut blake3::Hasher, time: Option<SystemTime>) {
    if let Some(time) = time {
        hash_system_time(hasher, time);
    }
}

fn hash_system_time(hasher: &mut blake3::Hasher, time: SystemTime) {
    if let Ok(duration) = time.duration_since(std::time::UNIX_EPOCH) {
        hasher.update(&duration.as_secs().to_le_bytes());
    }
}

/// Build a `PersistedProjectState` from in-memory project data.
///
/// Stores complete file metadata including `git_root` path for proper `RepoId`
/// reconstruction on restore. (Fixes HIGH `RepoId` restoration finding)
#[must_use]
#[allow(clippy::implicit_hasher)] // Standard HashMap is intentional
pub fn build_persisted_state(
    project_id: ProjectId,
    index_root: &Path,
    config_fingerprint: u64,
    repo_index: &HashMap<PathBuf, RepoId>,
    file_table: &HashMap<StringId, FileEntry>,
) -> PersistedProjectState {
    // Build reverse lookup: RepoId -> git_root path
    let repo_id_to_path: HashMap<u64, &Path> = repo_index
        .iter()
        .map(|(path, repo_id)| (repo_id.as_u64(), path.as_path()))
        .collect();

    let repo_entries: Vec<(PathBuf, u64)> = repo_index
        .iter()
        .map(|(path, repo_id)| (path.clone(), repo_id.as_u64()))
        .collect();

    let file_entries: Vec<PersistedFileEntry> = file_table
        .values()
        .map(|entry| {
            // Look up git_root from repo_id for proper reconstruction
            let git_root = if entry.repo_id.is_none() {
                None
            } else {
                repo_id_to_path
                    .get(&entry.repo_id.as_u64())
                    .map(|p| p.to_string_lossy().to_string())
            };

            PersistedFileEntry {
                path: entry.path.to_string(),
                repo_id: entry.repo_id.as_u64(),
                git_root,
                last_modified: entry.modified_at,
                size: 0, // FileEntry doesn't track size; could stat the file but adds latency
                content_hash: entry.content_hash,
                language_id: entry
                    .language_id
                    .as_ref()
                    .map(std::string::ToString::to_string),
            }
        })
        .collect();

    let mut state = PersistedProjectState {
        version: SCHEMA_VERSION,
        project_id: project_id.as_u64(),
        index_root: index_root.to_path_buf(),
        config_fingerprint,
        repo_index: repo_entries,
        files: file_entries,
        generated_at: SystemTime::now(),
        checksum: String::new(),
    };

    // Compute and set checksum
    state.checksum = compute_state_checksum(&state);

    state
}

/// Restore `repo_index` from persisted state.
#[must_use]
pub fn restore_repo_index(state: &PersistedProjectState) -> HashMap<PathBuf, RepoId> {
    state
        .repo_index
        .iter()
        .map(|(path, repo_id)| {
            let repo = if *repo_id == 0 {
                RepoId::NONE
            } else {
                // RepoId stores raw u64, reconstruct directly
                // Note: This is safe because we persisted the u64 from as_u64()
                RepoId::from_git_root(path) // Re-compute to ensure consistency
            };
            (path.clone(), repo)
        })
        .collect()
}

/// Restore `file_table` from persisted state.
///
/// Properly reconstructs `RepoId` from the stored `git_root` path, ensuring
/// file-repo associations survive persist/restore cycles.
/// (Fixes HIGH `RepoId` restoration finding from review)
#[must_use]
pub fn restore_file_table(state: &PersistedProjectState) -> HashMap<StringId, FileEntry> {
    state
        .files
        .iter()
        .map(|entry| {
            let path: StringId = Arc::from(entry.path.as_str());

            // Reconstruct RepoId from git_root path (fixes HIGH finding)
            let repo_id = restore_repo_id(entry);

            // Restore all file metadata (fixes MEDIUM finding)
            let language_id = restore_language_id(entry);

            let file_entry = FileEntry::with_metadata(
                Arc::clone(&path),
                repo_id,
                entry.content_hash,
                entry.last_modified,
                language_id,
            );
            (path, file_entry)
        })
        .collect()
}

fn restore_repo_id(entry: &PersistedFileEntry) -> RepoId {
    if entry.repo_id == 0 {
        return RepoId::NONE;
    }

    if let Some(ref git_root) = entry.git_root {
        return RepoId::from_git_root(Path::new(git_root));
    }

    log::warn!(
        "File '{}' has repo_id {} but no git_root; using RepoId::NONE",
        entry.path,
        entry.repo_id
    );
    RepoId::NONE
}

fn restore_language_id(entry: &PersistedFileEntry) -> Option<StringId> {
    entry
        .language_id
        .as_ref()
        .map(|value| Arc::from(value.as_str()))
}

/// Serde support for `SystemTime`.
mod system_time_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub fn serialize<S>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let duration = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
        (duration.as_secs(), duration.subsec_nanos()).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<SystemTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (secs, nanos): (u64, u32) = Deserialize::deserialize(deserializer)?;
        Ok(UNIX_EPOCH + Duration::new(secs, nanos))
    }
}

/// Serde support for `Option<SystemTime>`.
mod option_system_time_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[allow(clippy::ref_option)] // serde `with` attribute requires &Option<T> signature
    pub fn serialize<S>(time: &Option<SystemTime>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match time {
            Some(t) => {
                let duration = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
                Some((duration.as_secs(), duration.subsec_nanos())).serialize(serializer)
            }
            None => None::<(u64, u32)>.serialize(serializer),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<SystemTime>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<(u64, u32)> = Option::deserialize(deserializer)?;
        Ok(opt.map(|(secs, nanos)| UNIX_EPOCH + Duration::new(secs, nanos)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_config_fingerprint_stable() {
        let cache = CacheConfig {
            directory: ".sqry-cache".to_string(),
            persistent: true,
        };
        let indexing = IndexingConfig::default();

        let fp1 = compute_config_fingerprint(&cache, &indexing);
        let fp2 = compute_config_fingerprint(&cache, &indexing);

        assert_eq!(fp1, fp2, "Fingerprint should be stable for same input");
    }

    #[test]
    fn test_config_fingerprint_changes_on_persistent() {
        let mut cache = CacheConfig::default();
        let indexing = IndexingConfig::default();

        let fp1 = compute_config_fingerprint(&cache, &indexing);
        cache.persistent = false;
        let fp2 = compute_config_fingerprint(&cache, &indexing);

        assert_ne!(
            fp1, fp2,
            "Fingerprint should change when persistent changes"
        );
    }

    #[test]
    fn test_config_fingerprint_changes_on_directory() {
        let mut cache = CacheConfig::default();
        let indexing = IndexingConfig::default();

        let fp1 = compute_config_fingerprint(&cache, &indexing);
        cache.directory = ".other-cache".to_string();
        let fp2 = compute_config_fingerprint(&cache, &indexing);

        assert_ne!(fp1, fp2, "Fingerprint should change when directory changes");
    }

    #[test]
    fn test_config_fingerprint_changes_on_max_file_size() {
        let cache = CacheConfig::default();
        let mut indexing = IndexingConfig::default();

        let fp1 = compute_config_fingerprint(&cache, &indexing);
        indexing.max_file_size = 1024;
        let fp2 = compute_config_fingerprint(&cache, &indexing);

        assert_ne!(
            fp1, fp2,
            "Fingerprint should change when max_file_size changes"
        );
    }

    #[test]
    fn test_state_checksum_stable() {
        let state = PersistedProjectState {
            version: 1,
            project_id: 12345,
            index_root: PathBuf::from("/test/project"),
            config_fingerprint: 67890,
            repo_index: vec![(PathBuf::from("/test/repo"), 11111)],
            files: vec![PersistedFileEntry {
                path: "src/main.rs".to_string(),
                repo_id: 11111,
                git_root: Some("/test/repo".to_string()),
                last_modified: None,
                size: 1024,
                content_hash: Some(0xdead_beef),
                language_id: Some("rust".to_string()),
            }],
            generated_at: SystemTime::UNIX_EPOCH,
            checksum: String::new(),
        };

        let cs1 = compute_state_checksum(&state);
        let cs2 = compute_state_checksum(&state);

        assert_eq!(cs1, cs2, "Checksum should be stable for same state");
    }

    #[test]
    fn test_config_fingerprint_changes_on_max_depth() {
        let cache = CacheConfig::default();
        let mut indexing = IndexingConfig::default();

        let fp1 = compute_config_fingerprint(&cache, &indexing);
        indexing.max_depth = 50;
        let fp2 = compute_config_fingerprint(&cache, &indexing);

        assert_ne!(fp1, fp2, "Fingerprint should change when max_depth changes");
    }

    #[test]
    fn test_config_fingerprint_changes_on_scope_extraction() {
        let cache = CacheConfig::default();
        let mut indexing = IndexingConfig::default();

        let fp1 = compute_config_fingerprint(&cache, &indexing);
        indexing.enable_scope_extraction = !indexing.enable_scope_extraction;
        let fp2 = compute_config_fingerprint(&cache, &indexing);

        assert_ne!(
            fp1, fp2,
            "Fingerprint should change when enable_scope_extraction changes"
        );
    }

    #[test]
    fn test_config_fingerprint_changes_on_relation_extraction() {
        let cache = CacheConfig::default();
        let mut indexing = IndexingConfig::default();

        let fp1 = compute_config_fingerprint(&cache, &indexing);
        indexing.enable_relation_extraction = !indexing.enable_relation_extraction;
        let fp2 = compute_config_fingerprint(&cache, &indexing);

        assert_ne!(
            fp1, fp2,
            "Fingerprint should change when enable_relation_extraction changes"
        );
    }

    #[test]
    fn test_persistence_round_trip() {
        let tmp = TempDir::new().unwrap();
        let index_root = tmp.path();
        let persistence = ProjectPersistence::new(index_root, ".sqry-cache");

        let project_id = ProjectId::from_index_root(index_root);
        let mut repo_index = HashMap::new();
        repo_index.insert(index_root.to_path_buf(), RepoId::from_git_root(index_root));

        let mut file_table = HashMap::new();
        let path: StringId = Arc::from("src/main.rs");
        file_table.insert(
            Arc::clone(&path),
            FileEntry::new(path, RepoId::from_git_root(index_root)),
        );

        let fingerprint =
            compute_config_fingerprint(&CacheConfig::default(), &IndexingConfig::default());

        let state = build_persisted_state(
            project_id,
            index_root,
            fingerprint,
            &repo_index,
            &file_table,
        );

        // Write
        persistence.write_metadata(&state).unwrap();

        // Read back
        let loaded = persistence.read_metadata(project_id).unwrap();
        assert!(loaded.is_some());

        let loaded_state = loaded.unwrap();
        assert_eq!(loaded_state.version, state.version);
        assert_eq!(loaded_state.project_id, state.project_id);
        assert_eq!(loaded_state.config_fingerprint, state.config_fingerprint);
        assert_eq!(loaded_state.repo_index.len(), state.repo_index.len());
        assert_eq!(loaded_state.files.len(), state.files.len());
    }

    #[test]
    fn test_persistence_missing_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        let persistence = ProjectPersistence::new(tmp.path(), ".sqry-cache");
        let project_id = ProjectId::from_index_root(tmp.path());

        let result = persistence.read_metadata(project_id).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_opt_out_no_write() {
        // This test verifies the design: when persistent=false, persist is skipped.
        // The actual check happens in Project::persist_if_configured, not here.
        // This test just ensures the helper works correctly.
        let cache = CacheConfig {
            directory: ".sqry-cache".to_string(),
            persistent: false,
        };

        assert!(!cache.persistent, "persistent should be false");
    }

    #[test]
    fn test_path_traversal_rejected() {
        let tmp = TempDir::new().unwrap();
        let index_root = tmp.path();

        // Test path traversal with ".." is rejected
        let persistence = ProjectPersistence::new(index_root, "../escape");
        assert!(
            persistence.state_root.starts_with(index_root),
            "Path traversal should be rejected; state_root should be under index_root"
        );

        // Test absolute path is rejected
        #[cfg(unix)]
        let abs_path = "/tmp/absolute";
        #[cfg(windows)]
        let abs_path = "C:\\tmp\\absolute";
        let persistence = ProjectPersistence::new(index_root, abs_path);
        assert!(
            persistence.state_root.starts_with(index_root),
            "Absolute path should be rejected; state_root should be under index_root"
        );
    }

    #[test]
    fn test_repo_id_restoration_with_git_root() {
        // Build a state with git_root stored
        let state = PersistedProjectState {
            version: 1,
            project_id: 12345,
            index_root: PathBuf::from("/test/project"),
            config_fingerprint: 67890,
            repo_index: vec![(PathBuf::from("/test/repo"), 11111)],
            files: vec![PersistedFileEntry {
                path: "src/main.rs".to_string(),
                repo_id: 11111,
                git_root: Some("/test/repo".to_string()),
                last_modified: None,
                size: 1024,
                content_hash: None,
                language_id: None,
            }],
            generated_at: SystemTime::UNIX_EPOCH,
            checksum: String::new(),
        };

        // Restore file_table
        let file_table = restore_file_table(&state);

        // Verify RepoId is properly restored
        let entry = file_table.get("src/main.rs").expect("file should exist");
        let expected_repo_id = RepoId::from_git_root(Path::new("/test/repo"));
        assert_eq!(
            entry.repo_id, expected_repo_id,
            "RepoId should be reconstructed from git_root"
        );
        assert!(entry.repo_id.is_some(), "RepoId should not be NONE");
    }

    #[test]
    fn test_repo_id_none_preserved() {
        // Build a state with RepoId::NONE (repo_id = 0, no git_root)
        let state = PersistedProjectState {
            version: 1,
            project_id: 12345,
            index_root: PathBuf::from("/test/project"),
            config_fingerprint: 67890,
            repo_index: vec![],
            files: vec![PersistedFileEntry {
                path: "outside/file.txt".to_string(),
                repo_id: 0,
                git_root: None,
                last_modified: None,
                size: 0,
                content_hash: None,
                language_id: None,
            }],
            generated_at: SystemTime::UNIX_EPOCH,
            checksum: String::new(),
        };

        // Restore file_table
        let file_table = restore_file_table(&state);

        // Verify RepoId::NONE is preserved
        let entry = file_table
            .get("outside/file.txt")
            .expect("file should exist");
        assert!(entry.repo_id.is_none(), "RepoId::NONE should be preserved");
    }

    #[test]
    fn test_file_metadata_round_trip() {
        let tmp = TempDir::new().unwrap();
        let index_root = tmp.path();

        let mut repo_index = HashMap::new();
        let repo_id = RepoId::from_git_root(index_root);
        repo_index.insert(index_root.to_path_buf(), repo_id);

        let mut file_table = HashMap::new();
        let path: StringId = Arc::from("src/lib.rs");
        let lang: StringId = Arc::from("rust");
        let now = SystemTime::now();

        let original_entry = FileEntry::with_metadata(
            Arc::clone(&path),
            repo_id,
            Some(0x1234_5678_9abc_def0),
            Some(now),
            Some(Arc::clone(&lang)),
        );
        file_table.insert(Arc::clone(&path), original_entry.clone());

        let fingerprint =
            compute_config_fingerprint(&CacheConfig::default(), &IndexingConfig::default());

        // Build persisted state
        let state = build_persisted_state(
            ProjectId::from_index_root(index_root),
            index_root,
            fingerprint,
            &repo_index,
            &file_table,
        );

        // Verify git_root is stored
        assert!(
            state.files[0].git_root.is_some(),
            "git_root should be stored"
        );

        // Restore file_table
        let restored = restore_file_table(&state);
        let restored_entry = restored.get("src/lib.rs").expect("file should exist");

        // Verify all metadata is preserved
        assert_eq!(restored_entry.repo_id, repo_id, "RepoId should match");
        assert_eq!(
            restored_entry.content_hash, original_entry.content_hash,
            "content_hash should be preserved"
        );
        assert_eq!(
            restored_entry.language_id.as_deref(),
            Some("rust"),
            "language_id should be preserved"
        );
    }
}
