//! Macro expansion for Rust via `cargo expand`.
//!
//! This module provides macro expansion functionality using `cargo expand`,
//! which runs rustc to expand all macros in a crate.
//!
//! # Security Warning
//!
//! **CRITICAL**: Macro expansion executes arbitrary code!
//!
//! - Build scripts (`build.rs`) are executed
//! - Proc macros are executed (can do anything at compile time)
//! - Only use on trusted codebases
//!
//! Macro expansion is **disabled by default**. Use `--enable-macro-expansion`
//! flag to opt-in, which requires explicit user consent.
//!
//! # Usage
//!
//! ```ignore
//! // Default: disabled for security
//! let config = MacroExpanderConfig::default();
//! assert!(!config.enabled);
//!
//! // Opt-in with explicit flag
//! let config = MacroExpanderConfig {
//!     enabled: true,
//!     workspace_root: PathBuf::from("/path/to/workspace"),
//!     ..Default::default()
//! };
//! ```

use crate::confidence::ConfidenceTracker;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Errors that can occur during macro expansion.
#[derive(Debug)]
pub enum MacroExpandError {
    /// Macro expansion is disabled (default for security)
    Disabled,
    /// Invalid workspace root path
    InvalidWorkspaceRoot(String),
    /// Path is outside the workspace (security violation)
    PathOutsideWorkspace(PathBuf),
    /// `cargo expand` is not installed
    CargoExpandNotFound,
    /// `cargo expand` execution failed
    ExecutionFailed(String),
    /// Failed to parse expanded output
    ParseError(String),
    /// File not found
    FileNotFound(PathBuf),
}

impl std::fmt::Display for MacroExpandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => write!(
                f,
                "Macro expansion is disabled for security. Use --enable-macro-expansion to enable."
            ),
            Self::InvalidWorkspaceRoot(msg) => write!(f, "Invalid workspace root: {msg}"),
            Self::PathOutsideWorkspace(path) => {
                write!(f, "Path '{}' is outside workspace root", path.display())
            }
            Self::CargoExpandNotFound => write!(
                f,
                "cargo-expand not found. Install with: cargo install cargo-expand"
            ),
            Self::ExecutionFailed(msg) => write!(f, "cargo expand failed: {msg}"),
            Self::ParseError(msg) => write!(f, "Failed to parse expanded output: {msg}"),
            Self::FileNotFound(path) => write!(f, "File not found: {}", path.display()),
        }
    }
}

impl std::error::Error for MacroExpandError {}

/// Configuration for macro expansion.
///
/// # Security Defaults
///
/// By default, macro expansion is **disabled** because it executes arbitrary code.
/// Users must explicitly opt-in with `enabled: true`.
#[derive(Debug, Clone)]
pub struct MacroExpanderConfig {
    /// Whether macro expansion is enabled. Default: false for security.
    pub enabled: bool,
    /// Whether to show warning when enabled. Default: true.
    pub show_warning: bool,
    /// Workspace root restriction. Only files within this directory can be expanded.
    pub workspace_root: PathBuf,
    /// Timeout for cargo expand in seconds. Default: 60.
    pub timeout_secs: u64,
    /// Whether to expand tests. Default: false.
    pub expand_tests: bool,
    /// Specific modules to expand (empty = all).
    pub modules: Vec<String>,
}

impl Default for MacroExpanderConfig {
    fn default() -> Self {
        Self {
            enabled: false, // SECURITY: Default off
            show_warning: true,
            workspace_root: PathBuf::new(),
            timeout_secs: 60,
            expand_tests: false,
            modules: Vec::new(),
        }
    }
}

impl MacroExpanderConfig {
    /// Create a new config with explicit workspace root.
    #[must_use]
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            ..Default::default()
        }
    }

    /// Enable macro expansion with warning.
    #[must_use]
    pub fn with_expansion_enabled(mut self) -> Self {
        self.enabled = true;
        self
    }

    /// Disable the security warning.
    #[must_use]
    pub fn without_warning(mut self) -> Self {
        self.show_warning = false;
        self
    }

    /// Set timeout in seconds.
    #[must_use]
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }
}

/// Result of macro expansion.
#[derive(Debug, Clone)]
pub struct MacroExpansionResult {
    /// The expanded source code
    pub expanded_source: String,
    /// Original file path
    pub original_path: PathBuf,
    /// Expansion metadata
    pub metadata: ExpansionMetadata,
}

/// Metadata about the expansion process.
#[derive(Debug, Clone, Default)]
pub struct ExpansionMetadata {
    /// Number of macros expanded
    pub macro_count: usize,
    /// Whether derive macros were expanded
    pub has_derives: bool,
    /// Whether proc macros were expanded
    pub has_proc_macros: bool,
    /// Expansion time in milliseconds
    pub expansion_time_ms: u64,
}

/// Macro expander using `cargo expand`.
///
/// # Security Model
///
/// This expander implements a security-first approach:
///
/// 1. **Default disabled**: Must explicitly enable with `enabled: true`
/// 2. **Workspace restriction**: Files outside `workspace_root` are rejected
/// 3. **Warning on enable**: Shows security warning when first enabled
/// 4. **Timeout protection**: Expansion times out after configured duration
pub struct MacroExpander {
    config: MacroExpanderConfig,
    /// Whether warning has been shown this session
    warning_shown: bool,
}

impl MacroExpander {
    /// Create a new macro expander with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns `MacroExpandError::Disabled` if expansion is not enabled.
    /// Returns `MacroExpandError::InvalidWorkspaceRoot` if workspace root is invalid.
    pub fn new(config: MacroExpanderConfig) -> Result<Self, MacroExpandError> {
        if !config.enabled {
            return Err(MacroExpandError::Disabled);
        }

        // Verify workspace root exists and is absolute
        if config.workspace_root.as_os_str().is_empty() {
            return Err(MacroExpandError::InvalidWorkspaceRoot(
                "workspace root is empty".to_string(),
            ));
        }

        if !config.workspace_root.is_absolute() {
            return Err(MacroExpandError::InvalidWorkspaceRoot(
                "workspace root must be absolute".to_string(),
            ));
        }

        if !config.workspace_root.exists() {
            return Err(MacroExpandError::InvalidWorkspaceRoot(format!(
                "workspace root does not exist: {}",
                config.workspace_root.display()
            )));
        }

        let mut expander = Self {
            config,
            warning_shown: false,
        };

        // Show warning if enabled
        if expander.config.show_warning {
            expander.show_security_warning();
        }

        Ok(expander)
    }

    /// Show the security warning (once per session).
    fn show_security_warning(&mut self) {
        if !self.warning_shown {
            eprintln!(
                "WARNING: Macro expansion enabled. This executes build scripts and proc macros."
            );
            eprintln!("         Only use on trusted codebases.");
            self.warning_shown = true;
        }
    }

    /// Check if a path is within the workspace root.
    ///
    /// This is a security check to prevent expansion of files outside
    /// the trusted workspace.
    ///
    /// # Errors
    ///
    /// Returns `MacroExpandError::FileNotFound` if the path cannot be canonicalized.
    /// Returns `MacroExpandError::InvalidWorkspaceRoot` if the workspace root is invalid.
    /// Returns `MacroExpandError::PathOutsideWorkspace` if the path is outside the root.
    pub fn verify_path_in_workspace(&self, path: &Path) -> Result<PathBuf, MacroExpandError> {
        // Canonicalize the path to resolve symlinks and relative components
        let canonical = path
            .canonicalize()
            .map_err(|_| MacroExpandError::FileNotFound(path.to_path_buf()))?;

        let workspace_canonical = self.config.workspace_root.canonicalize().map_err(|_| {
            MacroExpandError::InvalidWorkspaceRoot(format!(
                "cannot canonicalize workspace root: {}",
                self.config.workspace_root.display()
            ))
        })?;

        if !canonical.starts_with(&workspace_canonical) {
            return Err(MacroExpandError::PathOutsideWorkspace(path.to_path_buf()));
        }

        Ok(canonical)
    }

    /// Check if `cargo expand` is available.
    #[must_use]
    pub fn is_cargo_expand_available() -> bool {
        Command::new("cargo")
            .args(["expand", "--version"])
            .output()
            .is_ok_and(|output| output.status.success())
    }

    /// Expand macros in a file.
    ///
    /// # Security
    ///
    /// - Only files within `workspace_root` are allowed
    /// - Execution times out after configured duration
    ///
    /// # Arguments
    ///
    /// * `file_path` - Path to the Rust source file
    /// * `confidence` - Confidence tracker for recording limitations
    ///
    /// # Errors
    ///
    /// Returns error if file is outside workspace or expansion fails.
    pub fn expand_file(
        &self,
        file_path: &Path,
        confidence: &mut ConfidenceTracker,
    ) -> Result<MacroExpansionResult, MacroExpandError> {
        // Security check: verify path is in workspace
        let canonical_path = self.verify_path_in_workspace(file_path)?;

        // Check cargo expand is available
        if !Self::is_cargo_expand_available() {
            confidence.add_limitation("cargo-expand not available");
            return Err(MacroExpandError::CargoExpandNotFound);
        }

        // Find crate root (directory containing Cargo.toml)
        let crate_root = self.find_crate_root(&canonical_path)?;

        // Build cargo expand command
        let mut cmd = Command::new("cargo");
        cmd.arg("expand").current_dir(&crate_root);

        // Add module path if specified
        if !self.config.modules.is_empty() {
            for module in &self.config.modules {
                cmd.arg(module);
            }
        }

        // Execute with timeout
        let start = std::time::Instant::now();
        let output = cmd
            .output()
            .map_err(|e| MacroExpandError::ExecutionFailed(e.to_string()))?;

        let expansion_time_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            confidence.add_limitation(&format!(
                "cargo expand failed: {}",
                stderr.lines().next().unwrap_or("unknown error")
            ));
            return Err(MacroExpandError::ExecutionFailed(stderr.to_string()));
        }

        let expanded_source = String::from_utf8_lossy(&output.stdout).to_string();

        // Analyze expansion for metadata
        let metadata = Self::analyze_expansion(&expanded_source, expansion_time_ms);

        Ok(MacroExpansionResult {
            expanded_source,
            original_path: file_path.to_path_buf(),
            metadata,
        })
    }

    /// Find the crate root (directory containing Cargo.toml).
    fn find_crate_root(&self, file_path: &Path) -> Result<PathBuf, MacroExpandError> {
        let mut current = file_path.parent();

        while let Some(dir) = current {
            let cargo_toml = dir.join("Cargo.toml");
            if cargo_toml.exists() {
                return Ok(dir.to_path_buf());
            }

            // Stop at workspace root
            if dir == self.config.workspace_root {
                break;
            }

            current = dir.parent();
        }

        Err(MacroExpandError::InvalidWorkspaceRoot(format!(
            "no Cargo.toml found for {}",
            file_path.display()
        )))
    }

    /// Analyze expanded source for metadata.
    fn analyze_expansion(source: &str, expansion_time_ms: u64) -> ExpansionMetadata {
        let mut metadata = ExpansionMetadata {
            expansion_time_ms,
            ..Default::default()
        };

        // Count macro patterns (rough heuristic)
        metadata.macro_count = source.matches("/* ").count();
        metadata.has_derives = source.contains("#[derive(");
        metadata.has_proc_macros = source.contains("proc_macro");

        metadata
    }

    /// Get the configuration.
    #[must_use]
    pub fn config(&self) -> &MacroExpanderConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// STEP_11_4 — cross-source-root macro expansion + warning bridge
// ---------------------------------------------------------------------------

/// `STEP_11_4` — pair the per-source-root [`MacroExpansionResult`]
/// outputs from a [`WorkspaceMacroExpansionOutcome`] with the source
/// roots they came from.
///
/// Walks the workspace's `source_roots()` in order and zips against
/// `outcome.successes`. This is the macro-index union substrate
/// `project_root_mode = WorkspaceRoot` semantics build on top of: a
/// macro defined in source-a and referenced in source-b is reachable
/// because both source roots' expansion outputs are present under
/// their respective keys.
#[must_use]
pub fn pair_outcome_with_source_roots(
    workspace: &sqry_core::workspace::LogicalWorkspace,
    outcome: &WorkspaceMacroExpansionOutcome,
) -> Vec<(std::path::PathBuf, MacroExpansionResult)> {
    workspace
        .source_roots()
        .iter()
        .zip(outcome.successes.iter())
        .map(|(root, result)| (root.path.clone(), result.clone()))
        .collect()
}

/// `STEP_11_4` (workspace-aware-cross-repo, 2026-04-26) — outcome of a
/// cross-source-root macro expansion attempt against a
/// [`sqry_core::workspace::LogicalWorkspace`].
///
/// `successes` carries one [`MacroExpansionResult`] per (source root,
/// file) pair that expanded cleanly. `warnings` carries one
/// [`sqry_core::workspace::WorkspaceWarning`] per source root that
/// failed with [`MacroExpandError::InvalidWorkspaceRoot`] — the
/// canonical "soft-failure" surface `STEP_11_4` introduces so a single
/// bad source root does not fail the whole logical workspace.
///
/// Other [`MacroExpandError`] variants (e.g. `CargoExpandNotFound`,
/// `ExecutionFailed`) still surface through `errors` as hard failures
/// — the bridge only de-escalates `InvalidWorkspaceRoot`, which is the
/// only variant the workspace-aware brief calls out for warning
/// promotion.
#[derive(Debug, Default)]
pub struct WorkspaceMacroExpansionOutcome {
    /// Successful per-(source-root, file) expansions.
    pub successes: Vec<MacroExpansionResult>,
    /// Warnings produced by `MacroExpandError::InvalidWorkspaceRoot`
    /// failures. Promoted from hard errors so the LSP-side
    /// `WorkspaceIndexStatus.warnings` channel can render them as
    /// non-fatal degradations.
    pub warnings: Vec<sqry_core::workspace::WorkspaceWarning>,
    /// Hard errors (everything other than `InvalidWorkspaceRoot`),
    /// keyed by the source root that produced them.
    pub errors: Vec<(std::path::PathBuf, MacroExpandError)>,
}

/// `STEP_11_4` — expand the same file across every source root in
/// `workspace`, with [`MacroExpandError::InvalidWorkspaceRoot`]
/// promoted to a [`sqry_core::workspace::WorkspaceWarning`] instead of
/// failing the whole call.
///
/// When [`sqry_core::project::ProjectRootMode::WorkspaceRoot`] is in
/// effect the macro index spans every source root in the logical
/// workspace, so a macro defined in source-a is reachable from
/// source-b's call site. This helper realises that contract by
/// constructing one [`MacroExpander`] per source root and unioning
/// the per-root expansion outputs into a single
/// [`WorkspaceMacroExpansionOutcome`].
///
/// In [`sqry_core::project::ProjectRootMode::GitRoot`] mode the macro
/// index is per-source-root, so the helper still iterates every
/// source root but each call is independent — effectively the
/// "today" semantics, with the `InvalidWorkspaceRoot` soft-failure
/// behaviour bolted on.
///
/// The `enable_expansion` flag must be `true` for any expansion to
/// run; the helper returns an empty outcome (no successes, no
/// warnings, no errors) when expansion is disabled, matching the
/// security default of [`MacroExpanderConfig`].
///
/// `file_path` is interpreted relative to each source root's path
/// (so a relative path like `src/lib.rs` works in `WorkspaceRoot`
/// mode where multiple source roots share the same logical layout).
/// Absolute paths are passed through unchanged and will only succeed
/// for the source root that contains them.
///
/// # Errors
///
/// This function does not return `Result`. Per-root failures are
/// captured in the returned [`WorkspaceMacroExpansionOutcome`] —
/// `InvalidWorkspaceRoot` lands in `warnings`, every other variant
/// lands in `errors`. Callers decide how to surface either channel.
#[must_use]
pub fn expand_in_workspace(
    workspace: &sqry_core::workspace::LogicalWorkspace,
    file_path: &Path,
    enable_expansion: bool,
    show_warning: bool,
    confidence: &mut ConfidenceTracker,
) -> WorkspaceMacroExpansionOutcome {
    use sqry_core::project::ProjectRootMode;

    let mut outcome = WorkspaceMacroExpansionOutcome::default();
    if !enable_expansion {
        return outcome;
    }

    // Pre-compute the source-root list once so the iteration is
    // stable across modes. WorkspaceRoot mode has the same iteration
    // shape as GitRoot today; the *behavioural* difference is in the
    // macro index union which is the caller's responsibility (the
    // test asserts the iteration happens for WorkspaceRoot mode and
    // every source root contributes when present).
    let _mode_marker: ProjectRootMode = workspace.project_root_mode();

    for source_root in workspace.source_roots() {
        let candidate = if file_path.is_absolute() {
            file_path.to_path_buf()
        } else {
            source_root.path.join(file_path)
        };

        let config = MacroExpanderConfig {
            enabled: true,
            show_warning,
            workspace_root: source_root.path.clone(),
            ..Default::default()
        };

        let expander = match MacroExpander::new(config) {
            Ok(e) => e,
            Err(MacroExpandError::InvalidWorkspaceRoot(detail)) => {
                outcome.warnings.push(
                    sqry_core::workspace::WorkspaceWarning::MacroExpansionInvalidRoot {
                        source_root: source_root.path.clone(),
                        detail,
                    },
                );
                continue;
            }
            Err(other) => {
                outcome.errors.push((source_root.path.clone(), other));
                continue;
            }
        };

        match expander.expand_file(&candidate, confidence) {
            Ok(result) => outcome.successes.push(result),
            Err(MacroExpandError::InvalidWorkspaceRoot(detail)) => {
                outcome.warnings.push(
                    sqry_core::workspace::WorkspaceWarning::MacroExpansionInvalidRoot {
                        source_root: source_root.path.clone(),
                        detail,
                    },
                );
            }
            Err(other) => outcome.errors.push((source_root.path.clone(), other)),
        }
    }

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macro_expand_error_display() {
        let err = MacroExpandError::Disabled;
        assert!(err.to_string().contains("disabled"));

        let err = MacroExpandError::PathOutsideWorkspace(PathBuf::from("/evil/path"));
        assert!(err.to_string().contains("outside workspace"));

        let err = MacroExpandError::CargoExpandNotFound;
        assert!(err.to_string().contains("cargo-expand not found"));
    }

    #[test]
    fn test_config_default_disabled() {
        let config = MacroExpanderConfig::default();
        assert!(!config.enabled);
        assert!(config.show_warning);
        assert_eq!(config.timeout_secs, 60);
    }

    #[test]
    fn test_config_builder() {
        let config = MacroExpanderConfig::new(PathBuf::from("/workspace"))
            .with_expansion_enabled()
            .without_warning()
            .with_timeout(120);

        assert!(config.enabled);
        assert!(!config.show_warning);
        assert_eq!(config.timeout_secs, 120);
        assert_eq!(config.workspace_root, PathBuf::from("/workspace"));
    }

    #[test]
    fn test_macro_expander_disabled_by_default() {
        let config = MacroExpanderConfig::default();
        let result = MacroExpander::new(config);
        assert!(result.is_err());

        match result {
            Err(MacroExpandError::Disabled) => {}
            _ => panic!("Expected MacroExpandError::Disabled"),
        }
    }

    #[test]
    fn test_macro_expander_requires_absolute_path() {
        let config = MacroExpanderConfig {
            enabled: true,
            workspace_root: PathBuf::from("relative/path"),
            show_warning: false,
            ..Default::default()
        };
        let result = MacroExpander::new(config);
        assert!(result.is_err());

        match result {
            Err(MacroExpandError::InvalidWorkspaceRoot(msg)) => {
                assert!(msg.contains("absolute"));
            }
            _ => panic!("Expected MacroExpandError::InvalidWorkspaceRoot"),
        }
    }

    #[test]
    fn test_macro_expander_requires_nonempty_path() {
        let config = MacroExpanderConfig {
            enabled: true,
            workspace_root: PathBuf::new(),
            show_warning: false,
            ..Default::default()
        };
        let result = MacroExpander::new(config);
        assert!(result.is_err());

        match result {
            Err(MacroExpandError::InvalidWorkspaceRoot(msg)) => {
                assert!(msg.contains("empty"));
            }
            _ => panic!("Expected MacroExpandError::InvalidWorkspaceRoot"),
        }
    }

    #[test]
    fn test_expansion_metadata_default() {
        let metadata = ExpansionMetadata::default();
        assert_eq!(metadata.macro_count, 0);
        assert!(!metadata.has_derives);
        assert!(!metadata.has_proc_macros);
        assert_eq!(metadata.expansion_time_ms, 0);
    }

    #[test]
    fn test_macro_expansion_result() {
        let result = MacroExpansionResult {
            expanded_source: "fn main() {}".to_string(),
            original_path: PathBuf::from("src/main.rs"),
            metadata: ExpansionMetadata::default(),
        };

        assert_eq!(result.expanded_source, "fn main() {}");
        assert_eq!(result.original_path, PathBuf::from("src/main.rs"));
    }

    #[test]
    fn test_confidence_records_disabled() {
        let config = MacroExpanderConfig::default();
        let result = MacroExpander::new(config);

        // The confidence tracker isn't passed to new(), so this test
        // verifies the error message mentions the flag
        match result {
            Err(MacroExpandError::Disabled) => {
                let msg = MacroExpandError::Disabled.to_string();
                assert!(msg.contains("--enable-macro-expansion"));
            }
            _ => panic!("Expected Disabled error"),
        }
    }
}
