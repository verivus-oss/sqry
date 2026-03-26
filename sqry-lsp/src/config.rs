use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use log::LevelFilter;
use serde::Deserialize;
use serde_json::Value;
use sqry_core::project::ProjectRootMode;

use crate::LspOptions;

/// Session-wide configuration derived from CLI flags and runtime updates.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub sqry_path: PathBuf,
    pub index_root: Option<PathBuf>,
    pub search_limit: usize,
    pub search_timeout: Duration,
    pub log_level: LevelFilter,
    pub document_limits: DocumentLimits,
    pub call_hierarchy: CallHierarchyConfig,
    /// Project root resolution mode (per `PROJECT_ROOT_SPEC.md` Section 4.1).
    ///
    /// - `GitRoot` (default): Each git repository gets its own Project
    /// - `WorkspaceFolder`: Each VS Code workspace folder gets a Project
    /// - `WorkspaceRoot`: Single Project covering all workspace folders
    pub project_root_mode: ProjectRootMode,
}

/// Per-category size limits for document handling.
///
/// Different file types have different reasonable size limits:
/// - Source code files: typically small, 512 KB default
/// - Data files (JSON, XML): can be large, 10 MB default
/// - Binary files: rejected entirely, not a size limit issue
#[derive(Debug, Clone)]
pub struct DocumentLimits {
    /// Maximum size for source code files (Rust, JS, Python, etc.)
    pub source_max_bytes: usize,
    /// Maximum size for data files (JSON, XML, YAML, etc.)
    pub data_max_bytes: usize,
}

impl Default for DocumentLimits {
    fn default() -> Self {
        Self {
            source_max_bytes: 512 * 1024,     // 512 KB for source code
            data_max_bytes: 10 * 1024 * 1024, // 10 MB for JSON/XML/etc.
        }
    }
}

impl DocumentLimits {
    /// Get the appropriate size limit for a file based on its category.
    #[must_use]
    pub fn max_bytes_for_file(&self, path: &std::path::Path) -> Option<usize> {
        use crate::file_types::{FileCategory, classify_file};

        match classify_file(path) {
            FileCategory::Binary => None, // Binary files rejected entirely
            FileCategory::Data => Some(self.data_max_bytes),
            FileCategory::SourceCode | FileCategory::Unknown => Some(self.source_max_bytes),
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            sqry_path: PathBuf::from("sqry"),
            index_root: None,
            search_limit: 200,
            search_timeout: Duration::from_millis(5_000),
            log_level: LevelFilter::Warn,
            document_limits: DocumentLimits::default(),
            call_hierarchy: CallHierarchyConfig::default(),
            project_root_mode: ProjectRootMode::default(), // GitRoot per spec
        }
    }
}

impl SessionConfig {
    /// Build a configuration snapshot from CLI options and optional JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error if the log level is invalid, the config file cannot be read,
    /// or the JSON payload is invalid.
    pub fn from_options(options: &LspOptions) -> Result<Self> {
        let mut config = SessionConfig {
            log_level: parse_level(&options.log_level)
                .with_context(|| format!("invalid log level '{}'", options.log_level))?,
            ..SessionConfig::default()
        };

        if let Some(root) = &options.index_root {
            config.index_root = Some(canonicalize(root));
        }

        if let Some(path) = &options.config {
            let bytes = fs::read(path)
                .with_context(|| format!("failed to read config file {}", path.display()))?;
            if !bytes.is_empty() {
                let value: Value = serde_json::from_slice(&bytes)
                    .with_context(|| format!("failed to parse {}", path.display()))?;
                config.apply_settings(&value)?;
            }
        }

        Ok(config)
    }

    /// Apply a `workspace/didChangeConfiguration` payload and return the diff.
    ///
    /// # Errors
    ///
    /// Returns an error if the payload cannot be parsed or contains invalid values.
    #[allow(clippy::too_many_lines)] // Configuration merge includes all fields to keep diff logic in one place.
    pub fn apply_settings(&mut self, settings: &Value) -> Result<ConfigDiff> {
        if settings.is_null() {
            return Ok(ConfigDiff::default());
        }

        let raw: RawRoot = serde_json::from_value(settings.clone())
            .with_context(|| "invalid configuration payload".to_string())?;

        let mut diff = ConfigDiff::default();
        if let Some(sqry) = raw.sqry {
            self.apply_sqry_settings(&sqry, &mut diff)?;
        }

        Ok(diff)
    }

    fn apply_sqry_settings(&mut self, sqry: &RawSqry, diff: &mut ConfigDiff) -> Result<()> {
        self.apply_sqry_path(diff, sqry.path.as_deref());
        self.apply_index_root(diff, sqry.index_root.as_deref());

        if let Some(search) = sqry.search.as_ref() {
            self.apply_search_settings(diff, search);
        }

        if let Some(log) = sqry.log.as_ref() {
            self.apply_log_settings(diff, log)?;
        }

        if let Some(doc) = sqry.document.as_ref() {
            self.apply_document_settings(diff, doc);
        }

        if let Some(ch) = sqry.call_hierarchy.as_ref() {
            self.apply_call_hierarchy_settings(diff, ch);
        }

        self.apply_project_root_mode(diff, sqry.project_root_mode.as_deref())?;

        Ok(())
    }

    fn apply_sqry_path(&mut self, diff: &mut ConfigDiff, path: Option<&str>) {
        let Some(path) = path else {
            return;
        };
        let next = if path.trim().is_empty() {
            PathBuf::from("sqry")
        } else {
            PathBuf::from(path)
        };
        if next != self.sqry_path {
            self.sqry_path.clone_from(&next);
            diff.sqry_path = Some(next);
        }
    }

    fn apply_index_root(&mut self, diff: &mut ConfigDiff, index_root: Option<&str>) {
        let Some(index_root) = index_root else {
            return;
        };
        let next = if index_root.trim().is_empty() {
            None
        } else {
            Some(canonicalize(Path::new(index_root)))
        };
        if next != self.index_root {
            self.index_root.clone_from(&next);
            diff.index_root = next;
        }
    }

    fn apply_search_settings(&mut self, diff: &mut ConfigDiff, search: &RawSearch) {
        if let Some(limit) = search.limit {
            let limit = limit.max(1);
            if limit != self.search_limit {
                self.search_limit = limit;
                diff.search_limit = Some(limit);
            }
        }
        if let Some(timeout_ms) = search.timeout {
            let timeout = Duration::from_millis(timeout_ms.max(1));
            if timeout != self.search_timeout {
                self.search_timeout = timeout;
                diff.search_timeout = Some(timeout);
            }
        }
    }

    fn apply_log_settings(&mut self, diff: &mut ConfigDiff, log: &RawLog) -> Result<()> {
        let Some(level) = log.level.as_deref() else {
            return Ok(());
        };
        let level = parse_level(level)?;
        if level != self.log_level {
            self.log_level = level;
            diff.log_level = Some(level);
        }
        Ok(())
    }

    fn apply_document_settings(&mut self, diff: &mut ConfigDiff, doc: &RawDocument) {
        if let Some(source_max) = doc.source_max_bytes {
            let capped = source_max.max(1);
            if capped != self.document_limits.source_max_bytes {
                self.document_limits.source_max_bytes = capped;
                diff.document_source_max_bytes = Some(capped);
            }
        }
        if let Some(data_max) = doc.data_max_bytes {
            let capped = data_max.max(1);
            if capped != self.document_limits.data_max_bytes {
                self.document_limits.data_max_bytes = capped;
                diff.document_data_max_bytes = Some(capped);
            }
        }
        if let Some(max_bytes) = doc.max_bytes {
            let capped = max_bytes.max(1);
            if capped != self.document_limits.source_max_bytes {
                self.document_limits.source_max_bytes = capped;
                diff.document_source_max_bytes = Some(capped);
            }
        }
    }

    fn apply_call_hierarchy_settings(
        &mut self,
        diff: &mut ConfigDiff,
        call_hierarchy: &RawCallHierarchy,
    ) {
        if let Some(max_results) = call_hierarchy.max_results {
            let max_results = max_results.max(1);
            if max_results != self.call_hierarchy.max_results {
                self.call_hierarchy.max_results = max_results;
                diff.call_hierarchy_max_results = Some(max_results);
            }
        }
        if let Some(timeout_ms) = call_hierarchy.timeout_ms {
            let timeout = Duration::from_millis(timeout_ms.max(1));
            if timeout != self.call_hierarchy.timeout {
                self.call_hierarchy.timeout = timeout;
                diff.call_hierarchy_timeout = Some(timeout);
            }
        }
        if let Some(include_detail) = call_hierarchy.include_detail
            && include_detail != self.call_hierarchy.include_detail
        {
            self.call_hierarchy.include_detail = include_detail;
            diff.call_hierarchy_include_detail = Some(include_detail);
        }
    }

    fn apply_project_root_mode(&mut self, diff: &mut ConfigDiff, mode: Option<&str>) -> Result<()> {
        let Some(mode_str) = mode else {
            return Ok(());
        };
        let new_mode = ProjectRootMode::from_str_opt(mode_str).ok_or_else(|| {
            anyhow!(
                "unsupported projectRootMode '{mode_str}' (expected: gitRoot, workspaceFolder, or workspaceRoot)"
            )
        })?;
        if new_mode != self.project_root_mode {
            self.project_root_mode = new_mode;
            diff.project_root_mode = Some(new_mode);
        }
        Ok(())
    }
}

fn parse_level(value: &str) -> Result<LevelFilter> {
    match value.to_ascii_lowercase().as_str() {
        "error" => Ok(LevelFilter::Error),
        "warn" | "warning" => Ok(LevelFilter::Warn),
        "info" => Ok(LevelFilter::Info),
        "debug" => Ok(LevelFilter::Debug),
        "trace" => Ok(LevelFilter::Trace),
        other => Err(anyhow!("unsupported log level '{other}'")),
    }
}

fn canonicalize(path: &Path) -> PathBuf {
    match path.canonicalize() {
        Ok(p) => p,
        Err(_) => path.to_path_buf(),
    }
}

#[derive(Debug, Default, Clone)]
pub struct ConfigDiff {
    pub sqry_path: Option<PathBuf>,
    pub index_root: Option<PathBuf>,
    pub search_limit: Option<usize>,
    pub search_timeout: Option<Duration>,
    pub log_level: Option<LevelFilter>,
    pub document_source_max_bytes: Option<usize>,
    pub document_data_max_bytes: Option<usize>,
    pub call_hierarchy_max_results: Option<usize>,
    pub call_hierarchy_timeout: Option<Duration>,
    pub call_hierarchy_include_detail: Option<bool>,
    /// Project root mode changed (requires `ProjectManager` rebuild per spec Section 9.3).
    pub project_root_mode: Option<ProjectRootMode>,
}

impl ConfigDiff {
    /// Returns true if any document limits changed.
    #[must_use]
    pub fn document_limits_changed(&self) -> bool {
        self.document_source_max_bytes.is_some() || self.document_data_max_bytes.is_some()
    }
}

#[derive(Debug, Deserialize)]
struct RawRoot {
    #[serde(default)]
    sqry: Option<RawSqry>,
}

#[derive(Debug, Deserialize)]
struct RawSqry {
    #[serde(default)]
    path: Option<String>,
    #[serde(default, rename = "indexRoot")]
    index_root: Option<String>,
    #[serde(default)]
    search: Option<RawSearch>,
    #[serde(default)]
    log: Option<RawLog>,
    #[serde(default)]
    document: Option<RawDocument>,
    #[serde(default, rename = "callHierarchy")]
    call_hierarchy: Option<RawCallHierarchy>,
    /// Project root resolution mode: "gitRoot" | "workspaceFolder" | "workspaceRoot"
    #[serde(default, rename = "projectRootMode")]
    project_root_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawSearch {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    timeout: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RawLog {
    #[serde(default)]
    level: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(clippy::struct_field_names)] // Field names align with JSON schema and keep legacy keys obvious.
struct RawDocument {
    /// Legacy setting: maximum bytes for any document (applies to source code only)
    #[serde(default, rename = "maxBytes")]
    max_bytes: Option<usize>,
    /// Maximum bytes for source code files (Rust, JS, Python, etc.)
    #[serde(default, rename = "sourceMaxBytes")]
    source_max_bytes: Option<usize>,
    /// Maximum bytes for data files (JSON, XML, YAML, etc.)
    #[serde(default, rename = "dataMaxBytes")]
    data_max_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct RawCallHierarchy {
    #[serde(default, rename = "maxResults")]
    max_results: Option<usize>,
    #[serde(default, rename = "timeoutMs")]
    timeout_ms: Option<u64>,
    #[serde(default, rename = "includeDetail")]
    include_detail: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct CallHierarchyConfig {
    pub max_results: usize,
    pub timeout: Duration,
    pub include_detail: bool,
}

impl Default for CallHierarchyConfig {
    fn default() -> Self {
        Self {
            max_results: 200,
            timeout: Duration::from_millis(5_000),
            include_detail: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn defaults_populate_expected_values() {
        let config = SessionConfig::default();
        assert_eq!(config.sqry_path, PathBuf::from("sqry"));
        assert!(config.index_root.is_none());
        assert_eq!(config.search_limit, 200);
        assert_eq!(config.search_timeout, Duration::from_millis(5_000));
        assert_eq!(config.log_level, LevelFilter::Warn);
        assert_eq!(config.document_limits.source_max_bytes, 512 * 1024);
        assert_eq!(config.document_limits.data_max_bytes, 10 * 1024 * 1024);
        assert_eq!(config.call_hierarchy.max_results, 200);
        assert_eq!(config.call_hierarchy.timeout, Duration::from_millis(5_000));
        assert!(config.call_hierarchy.include_detail);
        // PROJECT_ROOT_SPEC.md Section 4.1: gitRoot is the default
        assert_eq!(config.project_root_mode, ProjectRootMode::GitRoot);
    }

    #[test]
    fn applies_json_settings() {
        let mut config = SessionConfig::default();
        let payload = serde_json::json!({
            "sqry": {
                "path": "/opt/sqry/bin/sqry",
                "indexRoot": "/tmp/project",
                "search": {
                    "limit": 42,
                    "timeout": 1500
                },
                "log": {
                    "level": "info"
                },
                "document": {
                    "sourceMaxBytes": 1024,
                    "dataMaxBytes": 2048
                },
                "callHierarchy": {
                    "maxResults": 75,
                    "timeoutMs": 2500,
                    "includeDetail": false
                }
            }
        });

        let diff = config.apply_settings(&payload).expect("apply config");
        assert_eq!(config.sqry_path, PathBuf::from("/opt/sqry/bin/sqry"));
        assert_eq!(config.index_root, Some(PathBuf::from("/tmp/project")));
        assert_eq!(config.search_limit, 42);
        assert_eq!(config.search_timeout, Duration::from_millis(1500));
        assert_eq!(config.log_level, LevelFilter::Info);
        assert_eq!(config.document_limits.source_max_bytes, 1024);
        assert_eq!(config.document_limits.data_max_bytes, 2048);
        assert_eq!(config.call_hierarchy.max_results, 75);
        assert_eq!(config.call_hierarchy.timeout, Duration::from_millis(2500));
        assert!(!config.call_hierarchy.include_detail);

        assert!(diff.sqry_path.is_some());
        assert!(diff.index_root.is_some());
        assert_eq!(diff.search_limit, Some(42));
        assert_eq!(diff.search_timeout, Some(Duration::from_millis(1500)));
        assert_eq!(diff.log_level, Some(LevelFilter::Info));
        assert_eq!(diff.document_source_max_bytes, Some(1024));
        assert_eq!(diff.document_data_max_bytes, Some(2048));
        assert_eq!(diff.call_hierarchy_max_results, Some(75));
        assert_eq!(
            diff.call_hierarchy_timeout,
            Some(Duration::from_millis(2500))
        );
        assert_eq!(diff.call_hierarchy_include_detail, Some(false));
    }

    #[test]
    fn legacy_max_bytes_sets_source_limit() {
        let mut config = SessionConfig::default();
        let payload = serde_json::json!({
            "sqry": {
                "document": {
                    "maxBytes": 4096
                }
            }
        });

        let diff = config.apply_settings(&payload).expect("apply config");
        assert_eq!(config.document_limits.source_max_bytes, 4096);
        // Data limit unchanged
        assert_eq!(config.document_limits.data_max_bytes, 10 * 1024 * 1024);
        assert_eq!(diff.document_source_max_bytes, Some(4096));
        assert!(diff.document_data_max_bytes.is_none());
    }

    #[test]
    fn index_root_canonicalizes_when_possible() {
        let dir = tempdir().expect("tempdir");
        let nested = dir.path().join("nested");
        fs::create_dir_all(&nested).expect("mkdir");

        let mut config = SessionConfig::default();
        let payload = serde_json::json!({
            "sqry": {
                "indexRoot": nested.display().to_string()
            }
        });

        config.apply_settings(&payload).expect("apply");
        assert_eq!(
            config.index_root,
            Some(nested.canonicalize().expect("canonicalize"))
        );
    }

    #[test]
    fn call_hierarchy_settings_apply() {
        let mut config = SessionConfig::default();
        let payload = serde_json::json!({
            "sqry": {
                "callHierarchy": {
                    "maxResults": 10,
                    "timeoutMs": 1000,
                    "includeDetail": false
                }
            }
        });

        let diff = config.apply_settings(&payload).expect("apply");
        assert_eq!(config.call_hierarchy.max_results, 10);
        assert_eq!(config.call_hierarchy.timeout, Duration::from_millis(1000));
        assert!(!config.call_hierarchy.include_detail);
        assert_eq!(diff.call_hierarchy_max_results, Some(10));
        assert_eq!(
            diff.call_hierarchy_timeout,
            Some(Duration::from_millis(1000))
        );
        assert_eq!(diff.call_hierarchy_include_detail, Some(false));
    }

    #[test]
    fn project_root_mode_applies_git_root() {
        let mut config = SessionConfig::default();
        let payload = serde_json::json!({
            "sqry": {
                "projectRootMode": "gitRoot"
            }
        });

        let diff = config.apply_settings(&payload).expect("apply config");
        assert_eq!(config.project_root_mode, ProjectRootMode::GitRoot);
        // No change since gitRoot is default
        assert!(diff.project_root_mode.is_none());
    }

    #[test]
    fn project_root_mode_applies_workspace_folder() {
        let mut config = SessionConfig::default();
        let payload = serde_json::json!({
            "sqry": {
                "projectRootMode": "workspaceFolder"
            }
        });

        let diff = config.apply_settings(&payload).expect("apply config");
        assert_eq!(config.project_root_mode, ProjectRootMode::WorkspaceFolder);
        assert_eq!(
            diff.project_root_mode,
            Some(ProjectRootMode::WorkspaceFolder)
        );
    }

    #[test]
    fn project_root_mode_applies_workspace_root() {
        let mut config = SessionConfig::default();
        let payload = serde_json::json!({
            "sqry": {
                "projectRootMode": "workspaceRoot"
            }
        });

        let diff = config.apply_settings(&payload).expect("apply config");
        assert_eq!(config.project_root_mode, ProjectRootMode::WorkspaceRoot);
        assert_eq!(diff.project_root_mode, Some(ProjectRootMode::WorkspaceRoot));
    }

    #[test]
    fn project_root_mode_case_insensitive() {
        // The shared parser from sqry-core supports case-insensitive parsing
        let mut config = SessionConfig::default();
        let payload = serde_json::json!({
            "sqry": {
                "projectRootMode": "WORKSPACEFOLDER"
            }
        });

        let diff = config.apply_settings(&payload).expect("apply config");
        assert_eq!(config.project_root_mode, ProjectRootMode::WorkspaceFolder);
        assert_eq!(
            diff.project_root_mode,
            Some(ProjectRootMode::WorkspaceFolder)
        );
    }

    #[test]
    fn project_root_mode_rejects_invalid() {
        let mut config = SessionConfig::default();
        let payload = serde_json::json!({
            "sqry": {
                "projectRootMode": "invalidMode"
            }
        });

        let err = config.apply_settings(&payload).unwrap_err();
        assert!(err.to_string().contains("unsupported projectRootMode"));
    }
}
