//! One resolver for the `sqry` binary a test drives as a subprocess
//! (surface parity W4, design W4-D13).
//!
//! The test trees of `sqry-cli`, `sqry-lsp`, `sqry-mcp` and `sqry-daemon`
//! used to resolve the binary themselves, each with its own helper. They
//! disagreed on which environment variable they read and in which order,
//! and not one of them read `CARGO_TARGET_DIR`: most fell back to a
//! hard-coded `target/debug` and `target/release` under the workspace root,
//! so a run with an out-of-tree target directory found nothing and every
//! test that drove the binary from the LSP, MCP and daemon trees panicked at
//! once, in an environment CI does not use. One of them skipped instead of
//! failing when it found nothing, which reports a pass over an absent
//! subject.
//!
//! [`sqry_binary`] is the one resolver. It reads, in order:
//!
//! 1. `SQRY_E2E_SQRY_BIN`, when it names an existing file (the
//!    installed-binary override a release smoke check sets);
//! 2. `CARGO_BIN_EXE_sqry`, when it is set (cargo sets it only for
//!    integration tests of the package that declares the binary, which is
//!    why the helpers outside `sqry-cli` existed at all);
//! 3. for each target root in order, `CARGO_TARGET_DIR` when it is set and
//!    then the workspace root's own `target`, and for each of those the
//!    profiles `debug` then `release`, taking the first `sqry` that is a
//!    file;
//! 4. nothing, in which case it panics naming every variable it read and
//!    every candidate it tried.
//!
//! It panics rather than skipping on purpose. A harness that reports
//! success when its subject is absent has reported nothing, so a missing
//! binary has to be loud.
//!
//! The workspace root is the parent of this crate's own
//! `CARGO_MANIFEST_DIR`, which is the workspace root by construction and
//! does not depend on the calling crate's guess.
//!
//! A relative `CARGO_TARGET_DIR` is taken relative to the workspace root.
//! Cargo reads it relative to the directory it was invoked from, but a test
//! process runs with its package directory as the working directory, so
//! resolving it there would name a directory cargo never wrote to; the
//! workspace root is where a workspace-wide invocation starts.
//!
//! What this resolver does not see: a target directory given only as
//! `cargo test --target-dir <DIR>` or as `build.target-dir` in a cargo
//! configuration file, because cargo passes neither to the test process.
//! Such a run fails loudly, naming every candidate, rather than silently.

use std::fmt;
use std::path::{Path, PathBuf};

/// Every environment variable [`sqry_binary`] reads, in the order it reads
/// them. The failure message names all of them, so a reader of a failed run
/// can see what was consulted rather than guess.
pub const SQRY_BINARY_ENV_VARS: [&str; 3] = [
    "SQRY_E2E_SQRY_BIN",
    "CARGO_BIN_EXE_sqry",
    "CARGO_TARGET_DIR",
];

/// The build profiles searched under each target root, in order.
pub const SQRY_BINARY_PROFILES: [&str; 2] = ["debug", "release"];

/// No `sqry` binary was found. Carries what was consulted so the failure
/// names the variables and the candidates rather than a bare "not found".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqryBinaryNotFound {
    /// Every environment variable that was read, in read order.
    pub variables_read: Vec<String>,
    /// Every filesystem candidate that was tried, in try order.
    pub candidates: Vec<PathBuf>,
}

impl fmt::Display for SqryBinaryNotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "could not locate the sqry binary.")?;
        writeln!(f, "environment variables read, in order:")?;
        for name in &self.variables_read {
            writeln!(f, "  - {name}")?;
        }
        writeln!(f, "candidates tried, in order:")?;
        for candidate in &self.candidates {
            writeln!(f, "  - {}", candidate.display())?;
        }
        write!(
            f,
            "run `cargo build --bin sqry` first, or set SQRY_E2E_SQRY_BIN to an existing file."
        )
    }
}

impl std::error::Error for SqryBinaryNotFound {}

/// The file name of the `sqry` binary on this platform.
fn binary_file_name() -> String {
    format!("sqry{}", std::env::consts::EXE_SUFFIX)
}

/// The workspace root: the parent of this crate's manifest directory.
#[must_use]
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sqry-core lives one level under the workspace root")
        .to_path_buf()
}

/// Resolve the binary from explicit inputs, so a test can drive every
/// branch without touching the process environment.
///
/// `e2e_override` is the `SQRY_E2E_SQRY_BIN` value, `cargo_bin_exe` the
/// `CARGO_BIN_EXE_sqry` value, `cargo_target_dir` the `CARGO_TARGET_DIR`
/// value (joined onto `workspace_root` when it is relative), and
/// `workspace_root` the root whose own `target` directory is the last place
/// searched.
///
/// # Errors
///
/// [`SqryBinaryNotFound`] when no branch produced a file, carrying every
/// variable read and every candidate tried.
pub fn resolve_sqry_binary(
    e2e_override: Option<&Path>,
    cargo_bin_exe: Option<&Path>,
    cargo_target_dir: Option<&Path>,
    workspace_root: &Path,
) -> Result<PathBuf, SqryBinaryNotFound> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(path) = e2e_override {
        candidates.push(path.to_path_buf());
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
    }

    if let Some(path) = cargo_bin_exe {
        candidates.push(path.to_path_buf());
        return Ok(path.to_path_buf());
    }

    let file_name = binary_file_name();
    let mut roots: Vec<PathBuf> = Vec::with_capacity(2);
    if let Some(dir) = cargo_target_dir {
        if dir.is_relative() {
            roots.push(workspace_root.join(dir));
        } else {
            roots.push(dir.to_path_buf());
        }
    }
    roots.push(workspace_root.join("target"));

    for root in roots {
        for profile in SQRY_BINARY_PROFILES {
            let candidate = root.join(profile).join(&file_name);
            let found = candidate.is_file();
            candidates.push(candidate.clone());
            if found {
                return Ok(candidate);
            }
        }
    }

    Err(SqryBinaryNotFound {
        variables_read: SQRY_BINARY_ENV_VARS
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        candidates,
    })
}

/// Read the three environment variables and resolve.
///
/// # Errors
///
/// [`SqryBinaryNotFound`] when no branch produced a file.
pub fn try_sqry_binary() -> Result<PathBuf, SqryBinaryNotFound> {
    let e2e = std::env::var_os(SQRY_BINARY_ENV_VARS[0]).map(PathBuf::from);
    let cargo_bin_exe = std::env::var_os(SQRY_BINARY_ENV_VARS[1]).map(PathBuf::from);
    let cargo_target_dir = std::env::var_os(SQRY_BINARY_ENV_VARS[2]).map(PathBuf::from);
    resolve_sqry_binary(
        e2e.as_deref(),
        cargo_bin_exe.as_deref(),
        cargo_target_dir.as_deref(),
        &workspace_root(),
    )
}

/// Panic with the resolver's own failure text, which names every variable
/// read and every candidate tried. Split out from [`sqry_binary`] so the
/// panic path itself is testable.
fn unwrap_or_panic(resolved: Result<PathBuf, SqryBinaryNotFound>) -> PathBuf {
    match resolved {
        Ok(path) => path,
        Err(err) => panic!("{err}"),
    }
}

/// The `sqry` binary this test run should drive.
///
/// # Panics
///
/// Panics when no candidate is a file, naming every environment variable
/// read and every candidate tried. A test harness that quietly skipped here
/// would report success over an absent subject.
#[must_use]
pub fn sqry_binary() -> PathBuf {
    unwrap_or_panic(try_sqry_binary())
}

#[cfg(test)]
mod tests {
    use super::{
        SQRY_BINARY_ENV_VARS, SQRY_BINARY_PROFILES, SqryBinaryNotFound, binary_file_name,
        resolve_sqry_binary, sqry_binary, unwrap_or_panic,
    };
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// The three variables are process-global, so the one test that reads
    /// them through the process environment holds this for its whole body.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn plant(root: &Path, profile: &str) -> PathBuf {
        let dir = root.join(profile);
        std::fs::create_dir_all(&dir).expect("profile dir");
        let file = dir.join(binary_file_name());
        std::fs::write(&file, b"#!/bin/sh\nexit 0\n").expect("planted binary");
        file
    }

    /// U5 unit leg: `CARGO_TARGET_DIR` is searched, and `debug` wins over
    /// `release` when both are present.
    #[test]
    fn cargo_target_dir_is_searched_debug_before_release() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("out");
        let debug = plant(&target, "debug");
        let release = plant(&target, "release");
        let empty_workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&empty_workspace).expect("workspace dir");

        let resolved = resolve_sqry_binary(None, None, Some(&target), &empty_workspace)
            .expect("the planted debug binary resolves");
        println!("resolved: {}", resolved.display());
        assert_eq!(resolved, debug);
        assert_ne!(resolved, release, "debug is searched before release");
    }

    /// U5 unit leg, the kill for a resolver that returns its first
    /// candidate without checking it is a file: with only `release`
    /// planted, `release` is what comes back.
    #[test]
    fn a_candidate_that_is_not_a_file_is_not_returned() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("out");
        let release = plant(&target, "release");
        let empty_workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&empty_workspace).expect("workspace dir");
        let absent_debug = target.join("debug").join(binary_file_name());
        assert!(
            !absent_debug.is_file(),
            "instrument: the debug candidate must be absent for this leg"
        );

        let resolved = resolve_sqry_binary(None, None, Some(&target), &empty_workspace)
            .expect("the planted release binary resolves");
        println!("resolved: {}", resolved.display());
        assert_eq!(resolved, release);
    }

    /// U5 unit leg: the workspace root's own `target` is the last place
    /// searched, and it is searched when `CARGO_TARGET_DIR` is unset.
    #[test]
    fn the_workspace_target_is_the_last_root_searched() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let planted = plant(&workspace.join("target"), "debug");

        let resolved = resolve_sqry_binary(None, None, None, &workspace)
            .expect("the workspace target resolves");
        println!("resolved: {}", resolved.display());
        assert_eq!(resolved, planted);
    }

    /// U5 unit leg: an override that names an existing file wins; one that
    /// names nothing falls through to the next branch and is still reported
    /// as a candidate that was tried.
    #[test]
    fn the_override_wins_only_when_it_names_a_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let installed = tmp.path().join("installed-sqry");
        std::fs::write(&installed, b"#!/bin/sh\nexit 0\n").expect("installed binary");
        let target = tmp.path().join("out");
        let debug = plant(&target, "debug");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");

        let resolved = resolve_sqry_binary(Some(&installed), None, Some(&target), &workspace)
            .expect("the override resolves");
        assert_eq!(resolved, installed);

        let absent = tmp.path().join("no-such-binary");
        let resolved = resolve_sqry_binary(Some(&absent), None, Some(&target), &workspace)
            .expect("an absent override falls through");
        assert_eq!(resolved, debug);
    }

    /// U5 unit leg: nothing anywhere is a failure that names every variable
    /// read and every candidate tried, and the panic path carries that same
    /// text.
    #[test]
    fn nothing_anywhere_names_every_variable_and_every_candidate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("out");
        std::fs::create_dir_all(&target).expect("target dir");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");

        let err = resolve_sqry_binary(None, None, Some(&target), &workspace)
            .expect_err("no candidate is a file");
        println!("variables read: {}", err.variables_read.len());
        println!("candidates tried: {}", err.candidates.len());
        assert_eq!(err.variables_read.len(), SQRY_BINARY_ENV_VARS.len());
        assert_eq!(
            err.candidates.len(),
            2 * SQRY_BINARY_PROFILES.len(),
            "two roots, two profiles each"
        );

        let rendered = err.to_string();
        for name in SQRY_BINARY_ENV_VARS {
            assert!(
                rendered.contains(name),
                "the failure names {name}: {rendered}"
            );
        }
        for candidate in &err.candidates {
            assert!(
                rendered.contains(&candidate.display().to_string()),
                "the failure names {}: {rendered}",
                candidate.display()
            );
        }

        let payload = std::panic::catch_unwind(move || unwrap_or_panic(Err(err)))
            .expect_err("the panic path panics");
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        println!("panic message length: {}", message.len());
        for name in SQRY_BINARY_ENV_VARS {
            assert!(message.contains(name), "the panic names {name}: {message}");
        }
    }

    /// U5 unit leg, the kill for a resolver that drops the
    /// `CARGO_TARGET_DIR` branch: driven through the process environment
    /// with the two overrides unset, the planted binary under
    /// `CARGO_TARGET_DIR` is what `sqry_binary` returns.
    #[test]
    fn sqry_binary_reads_cargo_target_dir_from_the_environment() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("out");
        let planted = plant(&target, "debug");

        let prior: Vec<(&str, Option<std::ffi::OsString>)> = SQRY_BINARY_ENV_VARS
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();
        // SAFETY: ENV_LOCK serialises this module's only environment
        // access, and every variable is restored below before the
        // assertion runs.
        unsafe {
            std::env::remove_var(SQRY_BINARY_ENV_VARS[0]);
            std::env::remove_var(SQRY_BINARY_ENV_VARS[1]);
            std::env::set_var(SQRY_BINARY_ENV_VARS[2], &target);
        }
        let resolved = sqry_binary();
        // SAFETY: same lock, restoring what was read above.
        unsafe {
            for (name, value) in prior {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }

        println!("resolved from the environment: {}", resolved.display());
        assert_eq!(resolved, planted);
    }

    /// A relative `CARGO_TARGET_DIR` is searched under the workspace root,
    /// not under the test process's working directory.
    #[test]
    fn a_relative_cargo_target_dir_is_taken_from_the_workspace_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let planted = plant(&workspace.join("private-target"), "debug");

        let resolved =
            resolve_sqry_binary(None, None, Some(Path::new("private-target")), &workspace)
                .expect("the relative target dir resolves under the workspace root");
        println!("resolved: {}", resolved.display());
        assert_eq!(resolved, planted);

        let err = resolve_sqry_binary(None, None, Some(Path::new("elsewhere")), &workspace)
            .expect_err("nothing under the relative directory");
        println!("candidates tried: {}", err.candidates.len());
        assert_eq!(err.candidates.len(), 2 * SQRY_BINARY_PROFILES.len());
        assert_eq!(
            err.candidates[0],
            workspace
                .join("elsewhere")
                .join("debug")
                .join(binary_file_name())
        );
    }

    /// A failure value renders its own fields; an empty candidate list is
    /// still a readable message rather than a bare line.
    #[test]
    fn a_failure_with_no_candidates_still_names_the_variables() {
        let err = SqryBinaryNotFound {
            variables_read: SQRY_BINARY_ENV_VARS
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
            candidates: Vec::new(),
        };
        let rendered = err.to_string();
        println!("{rendered}");
        assert!(rendered.contains("candidates tried"));
        for name in SQRY_BINARY_ENV_VARS {
            assert!(rendered.contains(name));
        }
    }
}
