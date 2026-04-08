//! Output stream abstraction for stdout/stderr separation

use super::pager::{BufferedOutput, PagerConfig, PagerExitStatus};
use std::io::{self, Write};

/// Internal stdout backend for `OutputStreams`
enum StdoutBackend {
    /// Direct stdout (no paging)
    Direct(Box<dyn Write + Send>),
    /// Pager-backed output (may auto-page)
    Pager(BufferedOutput),
}

/// Manages stdout and stderr streams
///
/// Supports optional pager integration for large output. When created with
/// `with_pager()`, stdout writes are buffered and may be piped through
/// a pager (like `less`) if output exceeds terminal height.
pub struct OutputStreams {
    stdout: StdoutBackend,
    stderr: Box<dyn Write + Send>,
}

impl OutputStreams {
    /// Create streams using actual stdout/stderr (no paging)
    #[must_use]
    pub fn new() -> Self {
        Self {
            stdout: StdoutBackend::Direct(Box::new(io::stdout())),
            stderr: Box::new(io::stderr()),
        }
    }

    /// Create streams with pager support
    ///
    /// When pager is enabled, stdout output is buffered and may be
    /// piped through a pager (like `less`) if output exceeds terminal height.
    ///
    /// Call `finish()` at the end to properly handle pager lifecycle.
    #[must_use]
    pub fn with_pager(config: PagerConfig) -> Self {
        Self {
            stdout: StdoutBackend::Pager(BufferedOutput::new(config)),
            stderr: Box::new(io::stderr()),
        }
    }

    /// Create streams with custom writers (for testing)
    #[cfg(test)]
    #[allow(dead_code)] // API for future tests
    pub fn with_writers<W1, W2>(stdout: W1, stderr: W2) -> Self
    where
        W1: Write + Send + 'static,
        W2: Write + Send + 'static,
    {
        Self {
            stdout: StdoutBackend::Direct(Box::new(stdout)),
            stderr: Box::new(stderr),
        }
    }

    /// Write results to stdout (data stream)
    ///
    /// # Errors
    /// Returns an error if writing to stdout fails.
    pub fn write_result(&mut self, content: &str) -> io::Result<()> {
        match &mut self.stdout {
            StdoutBackend::Direct(writer) => writeln!(writer, "{content}"),
            StdoutBackend::Pager(buffer) => {
                buffer.write(content)?;
                buffer.write("\n")
            }
        }
    }

    /// Write diagnostic to stderr (diagnostic stream)
    ///
    /// # Errors
    /// Returns an error if writing to stderr fails.
    pub fn write_diagnostic(&mut self, content: &str) -> io::Result<()> {
        writeln!(self.stderr, "{content}")
    }

    /// Flush stderr (for --explain to avoid interleaving)
    #[allow(dead_code)]
    ///
    /// # Errors
    /// Returns an error if flushing stderr fails.
    pub fn flush_stderr(&mut self) -> io::Result<()> {
        self.stderr.flush()
    }

    /// Finalize output, flushing buffer and waiting for pager if applicable
    ///
    /// Returns the pager exit status. For non-pager streams, returns `Success`.
    /// Call this at the end of command execution to properly handle pager lifecycle.
    ///
    /// # Errors
    ///
    /// Returns an error if flushing or waiting for pager fails.
    pub fn finish(self) -> io::Result<PagerExitStatus> {
        match self.stdout {
            StdoutBackend::Direct(_) => Ok(PagerExitStatus::Success),
            StdoutBackend::Pager(buffer) => buffer.finish(),
        }
    }

    /// Finalize output and check for pager exit status
    ///
    /// This is a convenience method that combines `finish()` with pager exit code
    /// checking. If the pager exited with a non-zero code, returns a `CliError::PagerExit`.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Flushing or waiting for pager fails (IO error)
    /// - Pager exited with non-zero code (`CliError::PagerExit`)
    pub fn finish_checked(self) -> anyhow::Result<()> {
        let status = self.finish()?;
        if let Some(code) = status.exit_code() {
            return Err(crate::error::CliError::pager_exit(code).into());
        }
        Ok(())
    }
}

impl Default for OutputStreams {
    fn default() -> Self {
        Self::new()
    }
}

/// Test-friendly streams that capture output to strings
#[cfg(test)]
pub struct TestOutputStreams {
    pub stdout: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    pub stderr: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

#[cfg(test)]
impl TestOutputStreams {
    #[must_use]
    pub fn new() -> (Self, OutputStreams) {
        let stdout = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stderr = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        let test = Self {
            stdout: std::sync::Arc::clone(&stdout),
            stderr: std::sync::Arc::clone(&stderr),
        };

        let streams = OutputStreams {
            stdout: StdoutBackend::Direct(Box::new(SharedWriter(stdout))),
            stderr: Box::new(SharedWriter(stderr)),
        };

        (test, streams)
    }

    /// Returns the captured stdout as a string.
    ///
    /// # Panics
    ///
    /// Panics if the internal stdout mutex has been poisoned.
    #[must_use]
    pub fn stdout_string(&self) -> String {
        let guard = self.stdout.lock().unwrap();
        String::from_utf8_lossy(&guard).to_string()
    }

    /// Returns the captured stderr as a string.
    ///
    /// # Panics
    ///
    /// Panics if the internal stderr mutex has been poisoned.
    #[must_use]
    pub fn stderr_string(&self) -> String {
        let guard = self.stderr.lock().unwrap();
        String::from_utf8_lossy(&guard).to_string()
    }
}

#[cfg(test)]
struct SharedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

#[cfg(test)]
impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut guard = self.0.lock().unwrap();
        guard.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_streams_creation() {
        let _streams = OutputStreams::new();
        // Just verify it can be created
    }

    #[test]
    fn test_default() {
        let _streams = OutputStreams::default();
    }

    #[test]
    fn test_output_streams_capture() {
        let (test, mut streams) = TestOutputStreams::new();

        streams.write_result("hello").unwrap();
        streams.write_diagnostic("world").unwrap();

        assert_eq!(test.stdout_string(), "hello\n");
        assert_eq!(test.stderr_string(), "world\n");
    }

    #[test]
    fn test_finish_non_pager_returns_success() {
        let streams = OutputStreams::new();
        let status = streams.finish().unwrap();
        assert!(status.is_success());
    }
}
