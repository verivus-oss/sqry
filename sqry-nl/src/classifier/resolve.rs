//! Five-level resolver for the intent classifier model directory.
//!
//! This module implements NL02 of the
//! `nl-classifier-load-and-harden` plan. It is the single canonical
//! lookup chain shared by the Translator, the CLI, the MCP server,
//! the LSP server, and (eventually) the daemon.
//!
//! The resolution chain (first hit wins) is:
//!
//! 1. `cli_override` — caller passes
//!    [`crate::TranslatorConfig::model_dir_override`].
//! 2. `legacy` — the pre-existing programmatic
//!    [`crate::TranslatorConfig::model_dir`] field, retained for
//!    backward compatibility.
//! 3. `env` — value of the `SQRY_NL_MODEL_DIR` environment variable
//!    looked up by the caller.
//! 4. XDG cache — `dirs::cache_dir().join("sqry/models")`, abstracted
//!    behind [`DirsLike`] so tests can inject a fake.
//! 5. Next-to-binary — `<exe_parent>/models`.
//!
//! A candidate **only counts as a hit** when both the directory and a
//! sibling `manifest.json` file exist. If a candidate fails this check
//! the resolver moves on to the next level — it does **not**
//! short-circuit with an error. After all five levels miss, the
//! resolver returns `None` and the caller decides whether to download
//! (NL03) or fall back to the rule-based classifier.
//!
//! The function is pure (the only I/O is the existence checks
//! performed via the supplied [`DirsLike`] instance and the standard
//! library's `Path::exists`). No global state, no logging.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

/// Which level of the 5-level resolution chain produced a hit.
///
/// NL04 uses this to decide [`TrustMode`] at the call site without the
/// resolver itself having to know about integrity policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolverLevel {
    /// Level 1 — `--model-dir` (or programmatic `model_dir_override`).
    CliOverride,
    /// Level 2 — legacy `TranslatorConfig::model_dir`.
    LegacyConfig,
    /// Level 3 — `SQRY_NL_MODEL_DIR` environment variable.
    EnvVar,
    /// Level 4 — XDG cache dir (`<cache>/sqry/models`).
    XdgCache,
    /// Level 5 — directory next to the running binary.
    NextToBinary,
}

/// Integrity trust mode derived from the resolver level.
///
/// Per FR-14: Levels 1-3 (operator-supplied paths) are [`TrustMode::Custom`]
/// — integrity rooted in a user-supplied `manifest.json`. Levels 4-5
/// (auto-managed cache + binary-adjacent install) are
/// [`TrustMode::Trusted`] — integrity rooted in the binary's baked-in
/// expected manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustMode {
    /// Integrity verified against the binary's baked-in expected
    /// manifest (`include_str!("../../models/manifest.json")`).
    Trusted,
    /// Integrity verified against a `manifest.json` shipped alongside
    /// the operator-supplied model directory. Translator init logs a
    /// loud `tracing::warn!` for this mode.
    Custom,
}

impl fmt::Display for ResolverLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            ResolverLevel::CliOverride => "cli_override",
            ResolverLevel::LegacyConfig => "legacy_config",
            ResolverLevel::EnvVar => "env_var",
            ResolverLevel::XdgCache => "xdg_cache",
            ResolverLevel::NextToBinary => "next_to_binary",
        };
        f.write_str(name)
    }
}

impl fmt::Display for TrustMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            TrustMode::Trusted => "Trusted",
            TrustMode::Custom => "Custom",
        };
        f.write_str(name)
    }
}

impl From<ResolverLevel> for TrustMode {
    fn from(level: ResolverLevel) -> Self {
        match level {
            ResolverLevel::CliOverride | ResolverLevel::LegacyConfig | ResolverLevel::EnvVar => {
                TrustMode::Custom
            }
            ResolverLevel::XdgCache | ResolverLevel::NextToBinary => TrustMode::Trusted,
        }
    }
}

/// Abstraction over `dirs::cache_dir()` so tests can inject a fake
/// cache root without touching the real user environment.
///
/// Production code should pass [`RealDirs`]; tests should pass a
/// hand-rolled mock that returns a `tempfile::TempDir` path.
pub trait DirsLike {
    /// Return the platform's user cache directory, mirroring the
    /// semantics of `dirs::cache_dir()`.
    fn cache_dir(&self) -> Option<PathBuf>;
}

/// Production implementation of [`DirsLike`] backed by the `dirs`
/// crate.
pub struct RealDirs;

impl DirsLike for RealDirs {
    fn cache_dir(&self) -> Option<PathBuf> {
        dirs::cache_dir()
    }
}

/// Resolve the model directory using the five-level chain documented
/// at the module level.
///
/// # Arguments
///
/// * `cli_override` — Level 1. Set when the operator passes
///   `--model-dir` (CLI), the equivalent MCP/LSP parameter, or
///   constructs a [`crate::TranslatorConfig`] with
///   `model_dir_override` populated.
/// * `legacy` — Level 2. Set when callers populate the legacy
///   [`crate::TranslatorConfig::model_dir`] field. Callers normalise
///   the legacy `Option<String>` to `Option<&Path>` via `Path::new`.
/// * `env` — Level 3. The caller looks up `SQRY_NL_MODEL_DIR` via
///   `std::env::var_os` and forwards the `OsStr` here.
/// * `dirs` — Level 4. Provides the platform user-cache directory
///   used to compute `<cache>/sqry/models`.
/// * `exe` — Level 5. The current executable path
///   (`std::env::current_exe`) used to compute `<exe-dir>/models`.
///
/// Returns `Some((PathBuf, ResolverLevel))` for the first level that
/// points at a directory containing a `manifest.json` file; otherwise
/// `None`. Callers map [`ResolverLevel`] to [`TrustMode`] via the
/// `From` impl on `TrustMode`.
pub fn resolve_model_dir(
    cli_override: Option<&Path>,
    legacy: Option<&Path>,
    env: Option<&OsStr>,
    dirs: &dyn DirsLike,
    exe: Option<&Path>,
) -> Option<(PathBuf, ResolverLevel)> {
    // Level 1: CLI / programmatic override.
    if let Some(p) = cli_override
        && is_valid_model_dir(p)
    {
        return Some((p.to_path_buf(), ResolverLevel::CliOverride));
    }

    // Level 2: legacy `TranslatorConfig::model_dir`.
    if let Some(p) = legacy
        && is_valid_model_dir(p)
    {
        return Some((p.to_path_buf(), ResolverLevel::LegacyConfig));
    }

    // Level 3: SQRY_NL_MODEL_DIR environment variable.
    if let Some(env_value) = env {
        let candidate = PathBuf::from(env_value);
        if is_valid_model_dir(&candidate) {
            return Some((candidate, ResolverLevel::EnvVar));
        }
    }

    // Level 4: XDG / platform cache directory.
    if let Some(cache_root) = dirs.cache_dir() {
        let candidate = cache_root.join("sqry/models");
        if is_valid_model_dir(&candidate) {
            return Some((candidate, ResolverLevel::XdgCache));
        }
    }

    // Level 5: directory next to the running binary.
    if let Some(exe_path) = exe
        && let Some(exe_parent) = exe_path.parent()
    {
        let candidate = exe_parent.join("models");
        if is_valid_model_dir(&candidate) {
            return Some((candidate, ResolverLevel::NextToBinary));
        }
    }

    None
}

/// A candidate counts as a hit only when both the directory and a
/// sibling `manifest.json` exist.
fn is_valid_model_dir(path: &Path) -> bool {
    path.exists() && path.join("manifest.json").exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Simple mock that hands out a fixed cache_dir.
    struct MockDirs {
        root: Option<PathBuf>,
    }

    impl DirsLike for MockDirs {
        fn cache_dir(&self) -> Option<PathBuf> {
            self.root.clone()
        }
    }

    /// Stage a directory with a manifest.json so it counts as a hit.
    fn stage_model_dir(parent: &Path, name: &str) -> PathBuf {
        let dir = parent.join(name);
        fs::create_dir_all(&dir).expect("create model dir");
        fs::write(dir.join("manifest.json"), "{}").expect("write manifest");
        dir
    }

    #[test]
    fn cli_override_wins_over_legacy() {
        let tmp = TempDir::new().unwrap();
        let cli = stage_model_dir(tmp.path(), "cli");
        let legacy = stage_model_dir(tmp.path(), "legacy");

        let dirs = MockDirs { root: None };
        let (resolved, level) =
            resolve_model_dir(Some(&cli), Some(&legacy), None, &dirs, None).expect("hit");

        assert_eq!(resolved, cli);
        assert_eq!(level, ResolverLevel::CliOverride);
        assert_eq!(TrustMode::from(level), TrustMode::Custom);
    }

    #[test]
    fn legacy_wins_over_env() {
        let tmp = TempDir::new().unwrap();
        let legacy = stage_model_dir(tmp.path(), "legacy");
        let env_dir = stage_model_dir(tmp.path(), "env");

        let dirs = MockDirs { root: None };
        let (resolved, level) =
            resolve_model_dir(None, Some(&legacy), Some(env_dir.as_os_str()), &dirs, None)
                .expect("hit");

        assert_eq!(resolved, legacy);
        assert_eq!(level, ResolverLevel::LegacyConfig);
        assert_eq!(TrustMode::from(level), TrustMode::Custom);
    }

    #[test]
    fn env_wins_over_xdg() {
        let tmp = TempDir::new().unwrap();
        let env_dir = stage_model_dir(tmp.path(), "env");
        // Stage an XDG candidate too — it must lose to env.
        let xdg_root = tmp.path().join("xdg-root");
        let _xdg_dir = stage_model_dir(&xdg_root, "sqry/models");

        let dirs = MockDirs {
            root: Some(xdg_root.clone()),
        };
        let (resolved, level) =
            resolve_model_dir(None, None, Some(env_dir.as_os_str()), &dirs, None).expect("hit");

        assert_eq!(resolved, env_dir);
        assert_eq!(level, ResolverLevel::EnvVar);
        assert_eq!(TrustMode::from(level), TrustMode::Custom);
    }

    #[test]
    fn xdg_wins_over_next_to_binary() {
        let tmp = TempDir::new().unwrap();

        // XDG candidate at <xdg_root>/sqry/models
        let xdg_root = tmp.path().join("xdg-root");
        let xdg_models_dir = xdg_root.join("sqry/models");
        fs::create_dir_all(&xdg_models_dir).unwrap();
        fs::write(xdg_models_dir.join("manifest.json"), "{}").unwrap();

        // Next-to-binary candidate at <exe_dir>/models
        let exe_dir = tmp.path().join("bin");
        fs::create_dir_all(&exe_dir).unwrap();
        let exe_path = exe_dir.join("sqry");
        fs::write(&exe_path, b"").unwrap();
        let exe_models = exe_dir.join("models");
        fs::create_dir_all(&exe_models).unwrap();
        fs::write(exe_models.join("manifest.json"), "{}").unwrap();

        let dirs = MockDirs {
            root: Some(xdg_root),
        };
        let (resolved, level) =
            resolve_model_dir(None, None, None, &dirs, Some(&exe_path)).expect("hit");

        assert_eq!(resolved, xdg_models_dir);
        assert_eq!(level, ResolverLevel::XdgCache);
        assert_eq!(TrustMode::from(level), TrustMode::Trusted);
    }

    #[test]
    fn next_to_binary_used_when_others_missing() {
        let tmp = TempDir::new().unwrap();

        let exe_dir = tmp.path().join("bin");
        fs::create_dir_all(&exe_dir).unwrap();
        let exe_path = exe_dir.join("sqry");
        fs::write(&exe_path, b"").unwrap();
        let exe_models = exe_dir.join("models");
        fs::create_dir_all(&exe_models).unwrap();
        fs::write(exe_models.join("manifest.json"), "{}").unwrap();

        let dirs = MockDirs { root: None };
        let (resolved, level) =
            resolve_model_dir(None, None, None, &dirs, Some(&exe_path)).expect("hit");

        assert_eq!(resolved, exe_models);
        assert_eq!(level, ResolverLevel::NextToBinary);
        assert_eq!(TrustMode::from(level), TrustMode::Trusted);
    }

    #[test]
    fn returns_none_when_all_missing() {
        let tmp = TempDir::new().unwrap();

        // CLI override path that does not exist on disk.
        let missing_cli = tmp.path().join("missing-cli");

        // Legacy path also missing.
        let missing_legacy = tmp.path().join("missing-legacy");

        // Env variable points at a missing path.
        let missing_env = tmp.path().join("missing-env");

        // XDG cache root exists but has no `sqry/models` subdir.
        let xdg_root = tmp.path().join("xdg-root-empty");
        fs::create_dir_all(&xdg_root).unwrap();

        // Exe parent exists but has no `models` subdir.
        let exe_dir = tmp.path().join("bin-empty");
        fs::create_dir_all(&exe_dir).unwrap();
        let exe_path = exe_dir.join("sqry");
        fs::write(&exe_path, b"").unwrap();

        let dirs = MockDirs {
            root: Some(xdg_root),
        };
        let resolved = resolve_model_dir(
            Some(&missing_cli),
            Some(&missing_legacy),
            Some(missing_env.as_os_str()),
            &dirs,
            Some(&exe_path),
        );

        assert!(resolved.is_none(), "expected None, got {resolved:?}");
    }

    #[test]
    fn path_without_manifest_is_skipped() {
        let tmp = TempDir::new().unwrap();

        // Level 1: directory exists but has no manifest.json -> skipped.
        let cli = tmp.path().join("cli-no-manifest");
        fs::create_dir_all(&cli).unwrap();

        // Level 2: legacy is fully valid -> should win.
        let legacy = stage_model_dir(tmp.path(), "legacy");

        let dirs = MockDirs { root: None };
        let (resolved, level) =
            resolve_model_dir(Some(&cli), Some(&legacy), None, &dirs, None).expect("hit");

        assert_eq!(
            resolved, legacy,
            "level 1 must be skipped (missing manifest) so level 2 wins"
        );
        assert_eq!(level, ResolverLevel::LegacyConfig);
    }
}
