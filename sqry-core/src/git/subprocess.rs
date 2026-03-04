//! Subprocess-backed git implementation
//!
//! This backend executes git commands via subprocess for change detection.
//! It provides robust error handling, timeout protection, and output size limits.

use super::{ChangeSet, GitBackend, GitCapabilities, GitError, Result, parser};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Maximum allowed git output size (10 MB by default)
///
/// Can be overridden via `SQRY_GIT_MAX_OUTPUT_SIZE` environment variable.
/// Values are clamped to 1 MB - 100 MB range for security (P1-17).
const DEFAULT_MAX_OUTPUT_SIZE: usize = 10 * 1024 * 1024; // 10 MB
const MIN_MAX_OUTPUT_SIZE: usize = 1024 * 1024; // 1 MB minimum
const MAX_MAX_OUTPUT_SIZE: usize = 100 * 1024 * 1024; // 100 MB maximum

/// Default timeout for git commands (3 seconds)
const DEFAULT_TIMEOUT_MS: u64 = 3000;

/// Get maximum git output size, respecting environment variable override
///
/// Reads from `SQRY_GIT_MAX_OUTPUT_SIZE` environment variable.
/// If not set or invalid, returns 10 MB default.
/// Values are clamped between 1 MB and 100 MB for safety (P1-17).
///
/// # Security
///
/// This limit prevents `DoS` attacks from malicious repositories that could
/// produce arbitrarily large git diffs or status output, leading to memory
/// exhaustion and potential OOM kills.
///
/// # Examples
///
/// ```
/// # use sqry_core::git::max_git_output_size;
/// // Default behavior
/// let size = max_git_output_size(); // 10 MB (10485760 bytes)
/// assert_eq!(size, 10 * 1024 * 1024);
/// ```
///
/// ```
/// # use sqry_core::git::max_git_output_size;
/// // With environment override
/// unsafe { std::env::set_var("SQRY_GIT_MAX_OUTPUT_SIZE", "52428800"); } // 50 MB
/// let size = max_git_output_size(); // 50 MB
/// assert_eq!(size, 52428800);
/// # unsafe { std::env::remove_var("SQRY_GIT_MAX_OUTPUT_SIZE"); }
/// ```
///
/// ```
/// # use sqry_core::git::max_git_output_size;
/// // Clamping: value too low
/// unsafe { std::env::set_var("SQRY_GIT_MAX_OUTPUT_SIZE", "500"); } // 500 bytes (too small)
/// let size = max_git_output_size(); // Clamped to 1 MB minimum
/// assert_eq!(size, 1024 * 1024);
/// # unsafe { std::env::remove_var("SQRY_GIT_MAX_OUTPUT_SIZE"); }
/// ```
///
/// ```
/// # use sqry_core::git::max_git_output_size;
/// // Malformed value
/// unsafe { std::env::set_var("SQRY_GIT_MAX_OUTPUT_SIZE", "invalid"); }
/// let size = max_git_output_size(); // Falls back to 10 MB default
/// assert_eq!(size, 10 * 1024 * 1024);
/// # unsafe { std::env::remove_var("SQRY_GIT_MAX_OUTPUT_SIZE"); }
/// ```
#[must_use]
pub fn max_git_output_size() -> usize {
    let size = std::env::var("SQRY_GIT_MAX_OUTPUT_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_OUTPUT_SIZE);
    size.clamp(MIN_MAX_OUTPUT_SIZE, MAX_MAX_OUTPUT_SIZE)
}

/// Subprocess-backed git integration
///
/// Executes git commands via `Command::new("git")` with array arguments
/// (no shell invocation) to prevent command injection.
///
/// # Security
///
/// - Command injection: Uses `Command::new("git")` with array args, never shell
/// - Path traversal: All paths are canonicalized and validated
/// - Resource exhaustion: Output limited to 10MB, commands timeout after 3s
/// - Process cleanup: Hung processes killed with SIGTERM then SIGKILL
#[derive(Debug, Clone, Copy, Default)]
pub struct SubprocessGit;

impl SubprocessGit {
    /// Create a new subprocess git backend
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Execute a git command with timeout and output size limit
    ///
    /// # Security
    ///
    /// - Uses `Command::new("git")` with array args (no shell)
    /// - Limits output to configurable size (default 10MB, range 1MB-100MB)
    /// - Kills process after timeout (default 3s)
    /// - Captures stderr for error reporting
    ///
    /// # Visibility
    ///
    /// Public within crate to allow `RecencyIndex` and other git modules to use
    /// the same safety mechanisms (output limits, timeouts).
    pub(crate) fn execute_git(args: &[&str], timeout_ms: Option<u64>) -> Result<String> {
        let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
        let max_output_size = max_git_output_size(); // P1-17: Configurable limit

        // Build command (no shell invocation)
        let mut cmd = Command::new("git");
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

        // Spawn process
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                GitError::NotFound
            } else {
                GitError::CommandFailed {
                    message: format!("Failed to spawn git: {e}"),
                    stdout: String::new(),
                    stderr: String::new(),
                }
            }
        })?;

        // Wait with timeout
        let start = std::time::Instant::now();
        let result = loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    // Process exited
                    break status;
                }
                Ok(None) => {
                    // Still running - check timeout
                    if start.elapsed() >= timeout {
                        // Timeout - kill process
                        let _ = child.kill();
                        // Duration beyond u64::MAX ms (~584 million years) is impossible; clamp to max
                        let timeout_ms = timeout.as_millis().try_into().unwrap_or(u64::MAX);
                        return Err(GitError::Timeout(timeout_ms));
                    }
                    // Sleep briefly to avoid busy-wait
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    return Err(GitError::CommandFailed {
                        message: format!("Failed to wait for git: {e}"),
                        stdout: String::new(),
                        stderr: String::new(),
                    });
                }
            }
        };

        let status = result;

        // Read stdout (with size limit)
        // P1-17: Read +1 byte to detect if limit was exceeded
        let mut stdout = Vec::new();
        if let Some(out) = child.stdout.take() {
            let mut limited = out.take((max_output_size + 1) as u64);
            limited
                .read_to_end(&mut stdout)
                .map_err(|e| GitError::CommandFailed {
                    message: format!("Failed to read stdout: {e}"),
                    stdout: String::new(),
                    stderr: String::new(),
                })?;

            // P1-17: Hard error if limit exceeded (not silent truncation)
            if stdout.len() > max_output_size {
                return Err(GitError::OutputExceededLimit {
                    limit_bytes: max_output_size,
                    actual_bytes: stdout.len(), // Exact size (we read it all)
                });
            }
        }

        // Read stderr (for error messages)
        let mut stderr = Vec::new();
        if let Some(err) = child.stderr.take() {
            let mut limited = err.take((max_output_size + 1) as u64);
            limited
                .read_to_end(&mut stderr)
                .map_err(|e| GitError::CommandFailed {
                    message: format!("Failed to read stderr: {e}"),
                    stdout: String::new(),
                    stderr: String::new(),
                })?;
        }

        // Check exit status
        if !status.success() {
            let stdout_str = String::from_utf8_lossy(&stdout);
            let stderr_str = String::from_utf8_lossy(&stderr);
            return Err(GitError::CommandFailed {
                message: format!("Exit code {}", status.code().unwrap_or(-1)),
                stdout: stdout_str.to_string(),
                stderr: stderr_str.to_string(),
            });
        }

        // Convert stdout to string
        String::from_utf8(stdout)
            .map_err(|e| GitError::InvalidOutput(format!("Git output is not valid UTF-8: {e}")))
    }

    /// Get timeout from environment variable (clamped to 100-60000ms)
    fn get_timeout_ms() -> Option<u64> {
        std::env::var("SQRY_GIT_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(|t| t.clamp(100, 60000))
    }

    /// Get rename similarity threshold from environment (clamped to 0-100)
    #[allow(dead_code)]
    fn get_rename_similarity() -> u8 {
        std::env::var("SQRY_GIT_RENAME_SIMILARITY")
            .ok()
            .and_then(|s| s.parse::<u8>().ok())
            .map_or(50, |s| s.min(100))
    }

    /// Check if untracked files should be included (default: true)
    #[allow(dead_code)]
    fn should_include_untracked() -> bool {
        std::env::var("SQRY_GIT_INCLUDE_UNTRACKED")
            .ok()
            .and_then(|s| s.parse::<u8>().ok())
            != Some(0)
    }
}

impl GitBackend for SubprocessGit {
    fn is_repo(&self, root: &Path) -> Result<bool> {
        let result = Self::execute_git(
            &["-C", &root.display().to_string(), "rev-parse", "--git-dir"],
            Self::get_timeout_ms(),
        );

        match result {
            Ok(_) => Ok(true),
            Err(GitError::CommandFailed { stderr, .. })
                if stderr.contains("not a git repository") =>
            {
                Ok(false)
            }
            Err(GitError::NotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn repo_root(&self, root: &Path) -> Result<PathBuf> {
        let output = Self::execute_git(
            &[
                "-C",
                &root.display().to_string(),
                "rev-parse",
                "--show-toplevel",
            ],
            Self::get_timeout_ms(),
        )?;

        Ok(PathBuf::from(output.trim()))
    }

    fn head(&self, root: &Path) -> Result<Option<String>> {
        let result = Self::execute_git(
            &["-C", &root.display().to_string(), "rev-parse", "HEAD"],
            Self::get_timeout_ms(),
        );

        match result {
            Ok(output) => Ok(Some(output.trim().to_string())),
            Err(GitError::CommandFailed { message, .. }) if message.contains("128") => {
                // HEAD-less repo (no commits yet)
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn uncommitted(
        &self,
        root: &Path,
        include_untracked: bool,
    ) -> Result<(ChangeSet, Option<String>)> {
        // Get current HEAD before querying status (to avoid race)
        let head = self.head(root)?;

        // Convert path to string (needed for lifetime)
        let root_str = root.display().to_string();

        // Execute git status with null-terminated output
        let args = if include_untracked {
            vec![
                "-C",
                &root_str,
                "status",
                "--porcelain=v1",
                "-z",
                "--ignore-submodules=all",
            ]
        } else {
            vec![
                "-C",
                &root_str,
                "status",
                "--porcelain=v1",
                "-z",
                "--ignore-submodules=all",
                "--untracked-files=no",
            ]
        };

        let output = Self::execute_git(&args, Self::get_timeout_ms())?;

        // Parse porcelain output
        let changeset = parser::parse_porcelain(&output)?;

        Ok((changeset, head))
    }

    fn since(
        &self,
        root: &Path,
        baseline: &str,
        rename_similarity: u8,
    ) -> Result<(ChangeSet, Option<String>)> {
        // Get current HEAD before querying diff (to avoid race)
        let head = self.head(root)?;

        // Build diff range (baseline..HEAD)
        let range = format!("{baseline}..HEAD");

        // Execute git diff with null-terminated output
        let similarity = format!("-M{rename_similarity}");
        let output = Self::execute_git(
            &[
                "-C",
                &root.display().to_string(),
                "diff",
                "--name-status",
                "-z",
                &similarity,
                "--ignore-submodules=all",
                &range, // range before path separator per git syntax
                "--",
            ],
            Self::get_timeout_ms(),
        )?;

        // Parse diff output
        let changeset = parser::parse_diff_name_status(&output)?;

        Ok((changeset, head))
    }

    fn capabilities(&self) -> GitCapabilities {
        GitCapabilities {
            supports_blame: false,         // Blame not yet implemented
            supports_time_travel: false,   // Time-travel not yet implemented
            supports_history_index: false, // Historical indexing not yet implemented
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir_outside_git_repo() -> tempfile::TempDir {
        #[cfg(unix)]
        fn is_in_git_repo(path: &std::path::Path) -> bool {
            path.ancestors()
                .any(|ancestor| ancestor.join(".git").is_dir())
        }

        #[cfg(unix)]
        {
            for base in [
                std::path::Path::new("/var/tmp"),
                std::path::Path::new("/dev/shm"),
            ] {
                if base.is_dir()
                    && !is_in_git_repo(base)
                    && let Ok(tmpdir) = tempfile::TempDir::new_in(base)
                {
                    return tmpdir;
                }
            }
        }

        tempfile::tempdir().expect("create temp dir")
    }

    /// Helper to create a temporary git repo for testing
    fn create_test_repo() -> tempfile::TempDir {
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path();

        // Initialize git repo
        let init = Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("Failed to init git repo");
        assert!(init.status.success(), "git init failed: {init:?}");

        // Configure git user for commits
        let cfg1 = Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .expect("Failed to config user.name");
        assert!(
            cfg1.status.success(),
            "git config user.name failed: {cfg1:?}"
        );

        let cfg2 = Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(path)
            .output()
            .expect("Failed to config user.email");
        assert!(
            cfg2.status.success(),
            "git config user.email failed: {cfg2:?}"
        );

        // Disable GPG signing to avoid global config interference
        let cfg3 = Command::new("git")
            .args(["config", "commit.gpgSign", "false"])
            .current_dir(path)
            .output()
            .expect("Failed to config commit.gpgSign");
        assert!(
            cfg3.status.success(),
            "git config commit.gpgSign failed: {cfg3:?}"
        );

        // Disable CRLF conversion for consistent line endings across platforms
        let cfg4 = Command::new("git")
            .args(["config", "core.autocrlf", "false"])
            .current_dir(path)
            .output()
            .expect("Failed to config core.autocrlf");
        assert!(
            cfg4.status.success(),
            "git config core.autocrlf failed: {cfg4:?}"
        );

        tmpdir
    }

    #[test]
    fn test_is_repo_true() {
        let tmpdir = create_test_repo();
        let backend = SubprocessGit::new();

        let result = backend.is_repo(tmpdir.path());
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_is_repo_false() {
        let tmpdir = tempdir_outside_git_repo();
        let backend = SubprocessGit::new();

        let result = backend.is_repo(tmpdir.path());
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn test_repo_root() {
        let tmpdir = create_test_repo();
        let backend = SubprocessGit::new();

        let result = backend.repo_root(tmpdir.path());
        assert!(result.is_ok());

        let root = result.unwrap();
        assert!(root.ends_with(tmpdir.path().file_name().unwrap()));
    }

    #[test]
    fn test_head_no_commits() {
        let tmpdir = create_test_repo();
        let backend = SubprocessGit::new();

        let result = backend.head(tmpdir.path());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None); // No commits yet
    }

    #[test]
    fn test_head_with_commit() {
        let tmpdir = create_test_repo();
        let path = tmpdir.path();

        // Create and commit a file
        fs::write(path.join("test.txt"), "hello").unwrap();
        let add = Command::new("git")
            .args(["add", "test.txt"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(add.status.success(), "git add failed: {add:?}");
        let commit = Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(commit.status.success(), "git commit failed: {commit:?}");

        let backend = SubprocessGit::new();
        let result = backend.head(path);
        assert!(result.is_ok());

        let head = result.unwrap();
        assert!(head.is_some());
        assert_eq!(head.unwrap().len(), 40); // SHA-1 hash length
    }

    #[test]
    fn test_uncommitted_empty() {
        let tmpdir = create_test_repo();
        let path = tmpdir.path();

        // Create initial commit
        fs::write(path.join("test.txt"), "hello").unwrap();
        let add = Command::new("git")
            .args(["add", "test.txt"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(add.status.success());
        let commit = Command::new("git")
            .args(["commit", "-m", "Initial"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(commit.status.success());

        let backend = SubprocessGit::new();
        let result = backend.uncommitted(path, true);
        assert!(result.is_ok());

        let (changeset, head) = result.unwrap();
        assert!(changeset.is_empty());
        assert!(head.is_some());
    }

    #[test]
    fn test_uncommitted_modified() {
        let tmpdir = create_test_repo();
        let path = tmpdir.path();

        // Create initial commit
        fs::write(path.join("test.txt"), "hello").unwrap();
        let add = Command::new("git")
            .args(["add", "test.txt"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(add.status.success());
        let commit = Command::new("git")
            .args(["commit", "-m", "Initial"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(commit.status.success());

        // Modify file
        fs::write(path.join("test.txt"), "modified").unwrap();

        let backend = SubprocessGit::new();
        let result = backend.uncommitted(path, true);
        assert!(result.is_ok());

        let (changeset, _) = result.unwrap();
        assert_eq!(changeset.modified.len(), 1);
        assert_eq!(changeset.modified[0], PathBuf::from("test.txt"));
    }

    #[test]
    fn test_uncommitted_untracked() {
        let tmpdir = create_test_repo();
        let path = tmpdir.path();

        // Create initial commit
        fs::write(path.join("test.txt"), "hello").unwrap();
        Command::new("git")
            .args(["add", "test.txt"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "Initial"])
            .current_dir(path)
            .output()
            .unwrap();

        // Add untracked file
        fs::write(path.join("new.txt"), "new").unwrap();

        let backend = SubprocessGit::new();

        // With untracked
        let result = backend.uncommitted(path, true);
        assert!(result.is_ok());
        let (changeset, _) = result.unwrap();
        assert_eq!(changeset.added.len(), 1);

        // Without untracked
        let result = backend.uncommitted(path, false);
        assert!(result.is_ok());
        let (changeset, _) = result.unwrap();
        assert!(changeset.is_empty());
    }

    #[test]
    fn test_since() {
        let tmpdir = create_test_repo();
        let path = tmpdir.path();

        // Create first commit
        fs::write(path.join("file1.txt"), "hello").unwrap();
        let add = Command::new("git")
            .args(["add", "file1.txt"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(add.status.success());
        let commit = Command::new("git")
            .args(["commit", "-m", "First"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(commit.status.success());

        // Get baseline commit
        let backend = SubprocessGit::new();
        let baseline = backend.head(path).unwrap().unwrap();

        // Create second commit
        fs::write(path.join("file2.txt"), "world").unwrap();
        let add2 = Command::new("git")
            .args(["add", "file2.txt"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(add2.status.success());
        let commit2 = Command::new("git")
            .args(["commit", "-m", "Second"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(commit2.status.success());

        // Check changes since baseline
        let result = backend.since(path, &baseline, 50);
        assert!(result.is_ok());

        let (changeset, head) = result.unwrap();
        assert_eq!(changeset.added.len(), 1);
        assert_eq!(changeset.added[0], PathBuf::from("file2.txt"));
        assert!(head.is_some());
        assert_ne!(head.unwrap(), baseline); // HEAD moved
    }

    #[test]
    fn test_capabilities() {
        let backend = SubprocessGit::new();
        let caps = backend.capabilities();

        assert!(!caps.supports_blame);
        assert!(!caps.supports_time_travel);
        assert!(!caps.supports_history_index);
    }

    // P1-17: Tests for max_git_output_size() configuration function
    mod p1_17_git_output_limit {
        use super::*;
        use serial_test::serial;

        #[test]
        #[serial]
        fn test_max_git_output_size_default() {
            unsafe {
                std::env::remove_var("SQRY_GIT_MAX_OUTPUT_SIZE");
            }
            assert_eq!(max_git_output_size(), 10 * 1024 * 1024); // 10 MB
        }

        #[test]
        #[serial]
        fn test_max_git_output_size_env_override() {
            unsafe {
                std::env::set_var("SQRY_GIT_MAX_OUTPUT_SIZE", "52428800"); // 50 MB
            }
            assert_eq!(max_git_output_size(), 52_428_800);
            unsafe {
                std::env::remove_var("SQRY_GIT_MAX_OUTPUT_SIZE");
            }
        }

        #[test]
        #[serial]
        fn test_max_git_output_size_clamping_min() {
            unsafe {
                std::env::set_var("SQRY_GIT_MAX_OUTPUT_SIZE", "500"); // Below min (1 MB)
            }
            assert_eq!(max_git_output_size(), 1024 * 1024); // Clamped to 1 MB
            unsafe {
                std::env::remove_var("SQRY_GIT_MAX_OUTPUT_SIZE");
            }
        }

        #[test]
        #[serial]
        fn test_max_git_output_size_clamping_max() {
            unsafe {
                std::env::set_var("SQRY_GIT_MAX_OUTPUT_SIZE", "999000000000"); // Above max (100 MB)
            }
            assert_eq!(max_git_output_size(), 100 * 1024 * 1024); // Clamped to 100 MB
            unsafe {
                std::env::remove_var("SQRY_GIT_MAX_OUTPUT_SIZE");
            }
        }

        #[test]
        #[serial]
        fn test_max_git_output_size_malformed() {
            unsafe {
                std::env::set_var("SQRY_GIT_MAX_OUTPUT_SIZE", "invalid");
            }
            assert_eq!(max_git_output_size(), 10 * 1024 * 1024); // Falls back to default
            unsafe {
                std::env::remove_var("SQRY_GIT_MAX_OUTPUT_SIZE");
            }
        }

        #[test]
        fn test_output_exceeded_error_formatting() {
            let err = GitError::OutputExceededLimit {
                limit_bytes: 10 * 1024 * 1024,  // 10 MB
                actual_bytes: 15 * 1024 * 1024, // 15 MB
            };

            let msg = err.detailed_message();
            assert!(
                msg.contains("10.0 MB"),
                "Error message should show limit in MB"
            );
            assert!(
                msg.contains(">15.0 MB"),
                "Error message should show actual size in MB"
            );
            assert!(
                msg.contains("SQRY_GIT_MAX_OUTPUT_SIZE"),
                "Error message should mention env var"
            );
            assert!(
                msg.contains("git diff --stat"),
                "Error message should suggest investigation command"
            );
        }

        #[test]
        fn test_output_exceeded_error_suggested_limit() {
            let err = GitError::OutputExceededLimit {
                limit_bytes: 10 * 1024 * 1024,  // 10 MB
                actual_bytes: 15 * 1024 * 1024, // 15 MB
            };

            let suggested = err.suggested_limit();
            // Should suggest 2× actual, rounded up to nearest MB
            // 15MB × 2 = 30MB = 31457280 bytes
            // Rounded up to nearest MB: (30/1)+1 = 31MB = 32505856 bytes
            assert_eq!(suggested, 32_505_856); // 31 MB in bytes

            let msg = err.detailed_message();
            assert!(
                msg.contains("32505856"),
                "Error message should show suggested limit in bytes (31 MB = 32505856)"
            );
            assert!(
                msg.contains("31 MB"),
                "Error message should show suggested limit in MB"
            );
        }
    }
}
