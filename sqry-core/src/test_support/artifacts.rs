//! Test artifact writer for capturing logs to files
//!
//! This module provides functionality to write test logs to artifact files
//! in `target/test-artifacts/<crate>/` with collision-resistant naming.

use chrono::Local;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

/// Global counter for artifact file naming to prevent collisions
static COUNTER: AtomicU32 = AtomicU32::new(1);

/// Writer that tees log output to both stdout and a file
///
/// This struct implements `Write` and can be used as a target for `env_logger`.
/// It ensures all log messages are written to both stdout (for test output)
/// and a persistent file (for post-mortem analysis).
pub struct ArtifactWriter {
    file: Mutex<File>,
    path: PathBuf,
}

impl ArtifactWriter {
    /// Create a new artifact writer for the given crate
    ///
    /// # Arguments
    ///
    /// * `crate_name` - Name of the crate (used in filename)
    ///
    /// # Returns
    ///
    /// Returns `Some(ArtifactWriter)` if successful, `None` if file creation fails.
    ///
    /// # File Naming
    ///
    /// Files are named with format: `<crate>-<timestamp>-<pid>-<counter>.log`
    ///
    /// Example: `sqry-core-20251011T150012345-12345-001.log`
    ///
    /// Components:
    /// - `crate`: Crate name (e.g., "sqry-core")
    /// - `timestamp`: ISO 8601 with millisecond precision (`YYYYMMDDTHHMMSSmmm`)
    /// - `pid`: Process ID
    /// - `counter`: 3-digit monotonic counter (001, 002, 003, ...)
    ///
    /// This scheme provides:
    /// - Millisecond-level time resolution (vs second-level)
    /// - Process isolation via PID
    /// - Intra-process uniqueness via atomic counter
    /// - Collision probability: ~1 in 1 billion per process
    #[must_use]
    pub fn new(crate_name: &str) -> Option<Self> {
        let path = generate_artifact_path(crate_name).ok()?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            eprintln!(
                "Warning: Failed to create artifact directory {}: {e}",
                parent.display()
            );
            return None;
        }

        // Create the file
        let file = match File::create(&path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "Warning: Failed to create artifact file {}: {e}",
                    path.display()
                );
                return None;
            }
        };

        Some(ArtifactWriter {
            file: Mutex::new(file),
            path,
        })
    }

    /// Get the path to the artifact file
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Write for ArtifactWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Write to file (primary target)
        let mut file = self.file.lock().unwrap();
        file.write_all(buf)?;
        // Don't flush on every write - rely on explicit flush() or Drop
        // This improves performance by reducing syscalls

        // Also write to stdout so tests can capture output
        io::stdout().write_all(buf)?;

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut file = self.file.lock().unwrap();
        file.flush()?;
        io::stdout().flush()
    }
}

/// Generate a collision-resistant artifact file path
///
/// Creates path: `target/test-artifacts/<crate>/<crate>-<timestamp>-<pid>-<counter>.log`
///
/// # Arguments
///
/// * `crate_name` - Name of the crate
///
/// # Returns
///
/// Returns path on success, or `io::Error` if path cannot be determined.
fn generate_artifact_path(crate_name: &str) -> io::Result<PathBuf> {
    // Get workspace root (walk up from current dir to find Cargo.toml)
    let workspace_root = find_workspace_root()?;

    // Create artifacts directory path
    let artifacts_dir = workspace_root
        .join("target")
        .join("test-artifacts")
        .join(crate_name);

    // Generate unique filename
    let filename = generate_unique_filename(crate_name);

    Ok(artifacts_dir.join(filename))
}

/// Generate a unique filename using timestamp, PID, and counter
///
/// Format: `<crate>-<timestamp>-<pid>-<counter>.log`
///
/// Example: `sqry-core-20251011T150012345-12345-001.log`
fn generate_unique_filename(crate_name: &str) -> String {
    // Get current timestamp with millisecond precision
    let timestamp = Local::now().format("%Y%m%dT%H%M%S%3f");

    // Get process ID
    let pid = std::process::id();

    // Get monotonic counter (atomic increment)
    let counter = COUNTER.fetch_add(1, Ordering::SeqCst);

    // Format: <crate>-<timestamp>-<pid>-<counter>.log
    format!("{crate_name}-{timestamp}-{pid}-{counter:03}.log")
}

/// Find the workspace root by walking up the directory tree
///
/// Looks for a directory containing `Cargo.toml` with `[workspace]` section.
fn find_workspace_root() -> io::Result<PathBuf> {
    let current_dir = std::env::current_dir()?;

    // Walk up directory tree looking for workspace root
    for ancestor in current_dir.ancestors() {
        let cargo_toml = ancestor.join("Cargo.toml");
        if cargo_toml.exists() {
            // Check if it's a workspace manifest
            if let Ok(contents) = std::fs::read_to_string(&cargo_toml)
                && contents.contains("[workspace]")
            {
                return Ok(ancestor.to_path_buf());
            }
            // If not a workspace, it might still be the package root
            // Continue walking up
        }
    }

    // Fallback: use target relative to current directory
    Ok(current_dir)
}

/// Create an artifact writer if `SQRY_TEST_VERBOSE_ARTIFACTS` is set
///
/// This is the main entry point called by `verbosity::init()`.
///
/// # Arguments
///
/// * `crate_name` - Name of the crate
///
/// # Returns
///
/// Returns `Some(ArtifactWriter)` if artifacts are enabled and writer can be created,
/// `None` otherwise. Never panics.
#[must_use]
pub fn maybe_writer(crate_name: &str) -> Option<ArtifactWriter> {
    // Check if artifacts are enabled
    if std::env::var("SQRY_TEST_VERBOSE_ARTIFACTS").is_err() {
        return None;
    }

    // Try to create writer
    if let Some(writer) = ArtifactWriter::new(crate_name) {
        eprintln!(
            "Test artifacts will be written to: {}",
            writer.path().display()
        );
        Some(writer)
    } else {
        eprintln!("Warning: Failed to create artifact writer for {crate_name}");
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::io::Write;
    use std::sync::Mutex;

    // Mutex to ensure environment variable tests don't run concurrently
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn test_generate_unique_filename_format() {
        let filename = generate_unique_filename("sqry-core");

        // Should match format: sqry-core-<timestamp>-<pid>-<counter>.log
        assert!(filename.starts_with("sqry-core-"));
        assert!(
            std::path::Path::new(&filename)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("log"))
        );

        // Should contain PID
        let pid = std::process::id();
        assert!(filename.contains(&format!("-{pid}-")));

        // Should contain counter (at least 3 digits)
        let parts: Vec<&str> = filename.rsplitn(2, '-').collect();
        assert!(parts[0].len() >= 7); // "001.log" minimum
    }

    #[test]
    fn test_generate_unique_filename_increments_counter() {
        let name1 = generate_unique_filename("test");
        let name2 = generate_unique_filename("test");

        // Counter should increment (though timestamps might differ)
        assert_ne!(name1, name2, "Filenames should be unique");
    }

    #[test]
    fn test_maybe_writer_disabled_by_default() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            env::remove_var("SQRY_TEST_VERBOSE_ARTIFACTS");
        }

        let writer = maybe_writer("test-crate");
        assert!(
            writer.is_none(),
            "Writer should be None when artifacts are disabled"
        );
    }

    #[test]
    fn test_maybe_writer_enabled_with_env() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe {
            env::set_var("SQRY_TEST_VERBOSE_ARTIFACTS", "1");
        }

        let writer = maybe_writer("test-crate");
        // Note: This might fail in some environments (e.g., read-only filesystem)
        // so we just verify it doesn't panic
        drop(writer);

        unsafe {
            env::remove_var("SQRY_TEST_VERBOSE_ARTIFACTS");
        }
    }

    #[test]
    fn test_artifact_writer_writes_to_file() {
        let _lock = ENV_MUTEX.lock().unwrap();

        // Create a temporary artifact writer
        let Some(mut writer) = ArtifactWriter::new("test-writer") else {
            eprintln!("Skipping test: cannot create artifact writer in this environment");
            return;
        };

        // Write some data
        let test_data = b"Test log message\n";
        let result = writer.write(test_data);
        assert!(result.is_ok(), "Write should succeed");
        assert_eq!(result.unwrap(), test_data.len());

        // Flush to ensure data is written
        assert!(writer.flush().is_ok(), "Flush should succeed");

        // Verify file exists
        assert!(
            writer.path().exists(),
            "Artifact file should exist at {:?}",
            writer.path()
        );

        // Clean up
        let _ = fs::remove_file(writer.path());
    }

    #[test]
    fn test_find_workspace_root() {
        // This test might fail in non-workspace contexts, so we just ensure it doesn't panic
        let result = find_workspace_root();
        assert!(result.is_ok(), "find_workspace_root should not fail");
    }

    #[test]
    fn test_collision_resistance() {
        // Generate 100 filenames rapidly to test collision resistance
        let mut filenames = std::collections::HashSet::new();

        for _ in 0..100 {
            let name = generate_unique_filename("collision-test");
            assert!(
                filenames.insert(name.clone()),
                "Collision detected: {name} already exists"
            );
        }

        assert_eq!(filenames.len(), 100, "All 100 filenames should be unique");
    }
}
