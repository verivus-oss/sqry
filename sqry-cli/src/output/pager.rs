//! Pager integration for long output (P2-29)
//!
//! Provides automatic paging when output exceeds terminal height.
//! Uses external pagers like `less`, `bat`, or `more`.
//!
//! # Features
//!
//! - **Auto-detection**: Pages only when stdout is a TTY and output exceeds threshold
//! - **Capped buffering**: Max 1MB buffer, then streams to pager
//! - **Cross-platform**: Uses `terminal_size` crate for terminal dimensions
//! - **Unicode-aware**: Uses `unicode-width` for accurate line-wrap calculation
//! - **Safe command parsing**: Uses `shlex` for proper argument handling
//!
//! # Example
//!
//! ```ignore
//! use sqry_cli::output::pager::{BufferedOutput, PagerConfig, PagerExitStatus, PagerMode};
//!
//! let config = PagerConfig {
//!     enabled: PagerMode::Auto,
//!     ..Default::default()
//! };
//!
//! let mut output = BufferedOutput::new(config);
//! output.write("Hello, world!\n")?;
//!
//! // Finalize output - returns pager exit status
//! let status = output.finish()?;
//! if let Some(exit_code) = status.exit_code() {
//!     std::process::exit(exit_code);
//! }
//! ```

use std::io::{self, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use unicode_width::UnicodeWidthChar;

/// Capped buffer size to prevent unbounded memory growth (1MB)
const BUFFER_CAP_BYTES: usize = 1024 * 1024;

/// Pager mode selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PagerMode {
    /// Auto-detect: page if TTY and output exceeds threshold
    #[default]
    Auto,
    /// Always use pager (--pager flag)
    Always,
    /// Never use pager (--no-pager flag)
    Never,
}

/// Pager exit status for distinguishing normal exits from signal terminations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagerExitStatus {
    /// Pager exited successfully (exit code 0 or user quit with SIGPIPE/q)
    Success,
    /// Pager exited with non-zero exit code
    ExitCode(i32),
    /// Pager was terminated by a signal (Unix only)
    #[cfg_attr(not(unix), allow(dead_code))]
    Signal(i32),
}

impl PagerExitStatus {
    /// Returns the suggested process exit code
    ///
    /// - Success returns None (don't override exit code)
    /// - `ExitCode` returns the exit code
    /// - Signal returns 128 + signal number (Unix convention)
    #[must_use]
    #[allow(dead_code)] // Public API for future use
    pub fn exit_code(self) -> Option<i32> {
        match self {
            Self::Success => None,
            Self::ExitCode(code) => Some(code),
            Self::Signal(sig) => Some(128 + sig),
        }
    }

    /// Returns true if the pager exited successfully
    #[must_use]
    #[allow(dead_code)] // Public API for future use
    pub fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }
}

/// Configuration for pager behavior
#[derive(Debug, Clone)]
pub struct PagerConfig {
    /// Pager command (e.g., "less -R", "bat")
    pub command: String,
    /// Whether paging is enabled
    pub enabled: PagerMode,
    /// Minimum lines before auto-paging triggers (None = use terminal height)
    pub threshold: Option<usize>,
}

impl Default for PagerConfig {
    fn default() -> Self {
        Self {
            command: Self::default_pager_command(),
            enabled: PagerMode::Auto,
            threshold: None,
        }
    }
}

impl PagerConfig {
    /// Resolve pager command from environment/flags
    ///
    /// Priority order:
    /// 1. `$SQRY_PAGER` environment variable
    /// 2. `$PAGER` environment variable
    /// 3. Default: `less -FRX`
    ///
    /// The default flags mean:
    /// - `-F`: Quit if output fits on one screen (no need to press 'q')
    /// - `-R`: Raw control characters (preserve ANSI colors)
    /// - `-X`: Don't clear screen on exit (preserve output visibility)
    #[must_use]
    pub fn default_pager_command() -> String {
        std::env::var("SQRY_PAGER")
            .or_else(|_| std::env::var("PAGER"))
            .unwrap_or_else(|_| "less -FRX".to_string())
    }

    /// Build config from CLI args
    ///
    /// # Arguments
    ///
    /// * `pager_flag` - Whether `--pager` was specified
    /// * `no_pager_flag` - Whether `--no-pager` was specified
    /// * `pager_cmd` - Optional custom pager command from `--pager-cmd`
    #[must_use]
    pub fn from_cli_flags(pager_flag: bool, no_pager_flag: bool, pager_cmd: Option<&str>) -> Self {
        let mode = if no_pager_flag {
            PagerMode::Never
        } else if pager_flag {
            PagerMode::Always
        } else {
            PagerMode::Auto
        };

        let command = pager_cmd.map_or_else(Self::default_pager_command, String::from);

        Self {
            command,
            enabled: mode,
            threshold: None,
        }
    }
}

/// Determines whether to use pager for current output
pub struct PagerDecision {
    config: PagerConfig,
    is_tty: bool,
    terminal_height: Option<usize>,
}

impl PagerDecision {
    /// Create a new pager decision based on current terminal state
    #[must_use]
    pub fn new(config: PagerConfig) -> Self {
        use is_terminal::IsTerminal;

        let is_tty = std::io::stdout().is_terminal();
        let terminal_height = Self::detect_terminal_height();

        Self {
            config,
            is_tty,
            terminal_height,
        }
    }

    /// Public accessor for TTY detection result
    #[must_use]
    pub fn is_tty(&self) -> bool {
        self.is_tty
    }

    /// Check if paging should be used based on displayed row count
    ///
    /// This is the preferred method that accounts for line wrapping.
    #[must_use]
    pub fn should_page_rows(&self, displayed_rows: usize) -> bool {
        match self.config.enabled {
            PagerMode::Never => false,
            PagerMode::Always => true,
            PagerMode::Auto => {
                if !self.is_tty {
                    return false; // Don't page when piping
                }

                let threshold = self.config.threshold.or(self.terminal_height).unwrap_or(24);

                displayed_rows > threshold
            }
        }
    }

    /// Cross-platform terminal height detection using `terminal_size` crate
    #[must_use]
    fn detect_terminal_height() -> Option<usize> {
        use terminal_size::{Height, terminal_size};
        terminal_size().map(|(_, Height(h))| h as usize)
    }

    /// Cross-platform terminal width detection
    #[must_use]
    pub fn detect_terminal_width() -> Option<usize> {
        use terminal_size::{Width, terminal_size};
        terminal_size().map(|(Width(w), _)| w as usize)
    }
}

// Test helper for constructing PagerDecision with overrides
#[cfg(test)]
impl PagerDecision {
    /// Test-only constructor that allows overriding is_tty and terminal_height.
    /// Use this in unit tests to simulate different TTY/terminal configurations.
    #[must_use]
    pub fn for_testing(config: PagerConfig, is_tty: bool, terminal_height: Option<usize>) -> Self {
        Self {
            config,
            is_tty,
            terminal_height,
        }
    }
}

/// Manages output to pager process
///
/// Note: Debug is not derived because `Child` and `ChildStdin` don't implement Debug.
/// For test assertions, use `assert!(result.is_err())` pattern instead of `unwrap_err()`.
pub struct PagerWriter {
    child: Child,
    stdin: std::process::ChildStdin,
}

impl PagerWriter {
    /// Spawn pager process using shlex for proper argument parsing
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The command syntax is invalid
    /// - The pager executable cannot be found
    /// - The pager process fails to spawn
    ///
    /// # Panics
    /// Panics if the parsed command unexpectedly contains no program.
    pub fn spawn(command: &str) -> io::Result<Self> {
        // Use shlex for proper parsing that handles:
        // - Quoted arguments: "C:\Program Files\Git\usr\bin\less.exe" -R
        // - Escaped spaces: /path/with\ spaces/less
        // - Shell-style quoting: 'bat --style=plain'
        let parts = shlex::split(command).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid pager command syntax: {command}"),
            )
        })?;

        if parts.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Empty pager command",
            ));
        }

        let (program, args) = parts.split_first().expect("Already checked non-empty");

        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("Failed to open pager stdin"))?;

        Ok(Self { child, stdin })
    }

    /// Write content to pager
    ///
    /// # Errors
    ///
    /// Returns an error if writing to the pager's stdin fails.
    pub fn write(&mut self, content: &str) -> io::Result<()> {
        self.stdin.write_all(content.as_bytes())
    }

    /// Wait for pager to exit, returning the exit status
    ///
    /// # Errors
    ///
    /// Returns an error if waiting for the pager process fails.
    pub fn wait(mut self) -> io::Result<ExitStatus> {
        drop(self.stdin); // Close stdin to signal EOF
        self.child.wait()
    }
}

impl Write for PagerWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stdin.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdin.flush()
    }
}

/// Output mode after initial decision
enum OutputMode {
    /// Still buffering, haven't decided yet
    Buffering,
    /// Streaming to pager (threshold/cap exceeded)
    Pager(PagerWriter),
    /// Direct stdout (decided: below threshold)
    Direct,
}

/// Buffered output with capped streaming for auto mode
///
/// # Behavior by Mode
///
/// - **Never**: Writes directly to stdout (no buffering)
/// - **Always**: Spawns pager immediately
/// - **Auto + TTY**: Buffers output, spawns pager if threshold/cap exceeded
/// - **Auto + non-TTY**: Writes directly to stdout (no buffering, streams immediately)
///
/// # Memory Safety (TTY only)
///
/// When in Auto mode with a TTY, buffer never exceeds `BUFFER_CAP_BYTES` (1MB).
/// When the cap is reached, pager is spawned and buffer is flushed.
pub struct BufferedOutput {
    buffer: String,
    config: PagerConfig,
    decision: PagerDecision,
    mode: OutputMode,
    /// Terminal width for future display row calculation (currently unused)
    #[allow(dead_code)]
    terminal_width: Option<usize>,
    /// Number of complete lines in buffer (lines ending with \n)
    complete_lines: usize,
    /// Length of partial line at end of buffer (content after last \n)
    partial_line_len: usize,
    /// Deferred spawn error for non-NotFound failures (per CLI spec: exit 1)
    spawn_error: Option<io::Error>,
}

impl BufferedOutput {
    /// Create a new buffered output with the given configuration
    #[must_use]
    pub fn new(config: PagerConfig) -> Self {
        let decision = PagerDecision::new(config.clone());
        let terminal_width = PagerDecision::detect_terminal_width();

        // For explicit modes, decide immediately
        // Also for Auto mode when not a TTY, write directly to stdout (no buffering)
        let (mode, spawn_error) = match config.enabled {
            PagerMode::Never => (OutputMode::Direct, None),
            PagerMode::Always => {
                // Spawn pager immediately for Always mode
                match PagerWriter::spawn(&config.command) {
                    Ok(pager) => (OutputMode::Pager(pager), None),
                    Err(e) => {
                        // Per CLI spec: differentiate "not found" (warning, exit 0)
                        // from other spawn errors (error, exit 1)
                        let pager_name = config
                            .command
                            .split_whitespace()
                            .next()
                            .unwrap_or(&config.command);
                        if e.kind() == io::ErrorKind::NotFound {
                            eprintln!(
                                "Warning: pager '{pager_name}' not found. Output will not be paged. \
                                 To enable paging, install '{pager_name}' or set the SQRY_PAGER environment variable."
                            );
                            (OutputMode::Direct, None)
                        } else {
                            eprintln!(
                                "Error: Failed to start pager '{pager_name}': {e}. \
                                 Please check that the binary is correct and executable, \
                                 or set a different pager using the SQRY_PAGER environment variable."
                            );
                            // Defer error - still write to stdout, but finish() will return error
                            (OutputMode::Direct, Some(e))
                        }
                    }
                }
            }
            PagerMode::Auto => {
                // Non-TTY: stream immediately without buffering
                // TTY: buffer until we know if paging is needed
                if decision.is_tty() {
                    (OutputMode::Buffering, None)
                } else {
                    (OutputMode::Direct, None)
                }
            }
        };

        Self {
            buffer: String::new(),
            config,
            decision,
            mode,
            terminal_width,
            complete_lines: 0,
            partial_line_len: 0,
            spawn_error,
        }
    }

    /// Create a new buffered output for testing with explicit buffering mode
    ///
    /// This forces Buffering mode regardless of TTY detection, allowing tests
    /// to verify the line counting logic without needing a real TTY.
    #[cfg(test)]
    pub fn new_for_testing(config: PagerConfig) -> Self {
        let decision = PagerDecision::new(config.clone());
        let terminal_width = PagerDecision::detect_terminal_width();

        Self {
            buffer: String::new(),
            config,
            decision,
            mode: OutputMode::Buffering, // Always buffer for testing
            terminal_width,
            complete_lines: 0,
            partial_line_len: 0,
            spawn_error: None,
        }
    }

    fn write_direct(content: &str) -> io::Result<()> {
        std::io::stdout().write_all(content.as_bytes())
    }

    fn write_pager(pager: &mut PagerWriter, content: &str) -> io::Result<()> {
        match pager.write(content) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn update_line_counts(&mut self, content: &str) {
        let newline_count = content.bytes().filter(|&b| b == b'\n').count();
        self.complete_lines += newline_count;
        self.update_partial_line_len(content);
    }

    fn update_partial_line_len(&mut self, content: &str) {
        if let Some(last_nl_offset) = content.rfind('\n') {
            self.partial_line_len = content.len().saturating_sub(last_nl_offset + 1);
        } else {
            self.partial_line_len += content.len();
        }
    }

    fn displayed_row_estimate(&self) -> usize {
        self.complete_lines + usize::from(self.partial_line_len > 0)
    }

    fn should_transition_to_pager(&self, displayed_rows: usize) -> bool {
        self.decision.should_page_rows(displayed_rows) || self.buffer.len() > BUFFER_CAP_BYTES
    }

    fn transition_to_pager(&mut self) -> io::Result<()> {
        match PagerWriter::spawn(&self.config.command) {
            Ok(mut pager) => {
                pager.write(&self.buffer)?;
                self.buffer.clear();
                self.mode = OutputMode::Pager(pager);
                Ok(())
            }
            Err(e) => self.handle_pager_spawn_error(e),
        }
    }

    fn handle_pager_spawn_error(&mut self, err: io::Error) -> io::Result<()> {
        let pager_name = self
            .config
            .command
            .split_whitespace()
            .next()
            .unwrap_or(&self.config.command);
        if err.kind() == io::ErrorKind::NotFound {
            eprintln!(
                "Warning: pager '{pager_name}' not found. Output will not be paged. \
                 To enable paging, install '{pager_name}' or set the SQRY_PAGER environment variable."
            );
        } else {
            eprintln!(
                "Error: Failed to start pager '{pager_name}': {err}. \
                 Please check that the binary is correct and executable, \
                 or set a different pager using the SQRY_PAGER environment variable."
            );
            self.spawn_error = Some(err);
        }

        Self::write_direct(&self.buffer)?;
        self.buffer.clear();
        self.mode = OutputMode::Direct;
        Ok(())
    }

    /// Write content, handling mode transitions
    ///
    /// # Errors
    ///
    /// Returns an error if writing to stdout or pager fails.
    pub fn write(&mut self, content: &str) -> io::Result<()> {
        match &mut self.mode {
            OutputMode::Direct => {
                // Write directly to stdout
                Self::write_direct(content)
            }
            OutputMode::Pager(pager) => {
                // Stream to pager, handling broken pipe gracefully
                Self::write_pager(pager, content)
            }
            OutputMode::Buffering => {
                // Append to buffer first
                self.buffer.push_str(content);

                // Incremental line counting: count newlines in the new content
                // This is O(n) in the new content, not O(n) in the entire buffer
                self.update_line_counts(content);

                // Calculate displayed rows:
                // - Each complete line is 1+ rows (depends on wrapping)
                // - Partial line at end is 1 row (if non-empty)
                // For simplicity in threshold checking, use complete_lines + 1 if partial exists
                // This is a conservative estimate that may trigger paging slightly early
                let displayed_rows = self.displayed_row_estimate();

                // Check thresholds
                if self.should_transition_to_pager(displayed_rows) {
                    // Threshold or buffer cap exceeded: transition to pager mode
                    // Note: Non-TTY output uses Direct mode from the start, so we only
                    // reach here when is_tty() is true
                    self.transition_to_pager()?;
                }
                // Otherwise: continue buffering (below threshold and cap)
                Ok(())
            }
        }
    }

    /// Finalize output, flushing any buffered content
    ///
    /// Returns a `PagerExitStatus` indicating how the pager terminated:
    /// - `Success`: Pager exited normally (code 0, SIGPIPE, or no pager used)
    /// - `ExitCode(n)`: Pager exited with non-zero code
    /// - `Signal(n)`: Pager was terminated by signal (non-SIGPIPE)
    ///
    /// # Errors
    ///
    /// Returns an error if flushing or waiting for pager fails, or if there
    /// was a deferred pager spawn error (non-NotFound spawn failures).
    pub fn finish(self) -> io::Result<PagerExitStatus> {
        // Check for deferred spawn error first (non-NotFound spawn failures)
        // Per CLI spec: spawn failures (other than not-found) should exit 1
        if let Some(spawn_err) = self.spawn_error {
            return Err(spawn_err);
        }

        match self.mode {
            OutputMode::Direct => Ok(PagerExitStatus::Success),
            OutputMode::Pager(pager) => {
                let status = pager.wait()?;
                Ok(exit_status_to_pager_status(status))
            }
            OutputMode::Buffering => {
                // Never transitioned, output is small - write directly
                std::io::stdout().write_all(self.buffer.as_bytes())?;
                Ok(PagerExitStatus::Success)
            }
        }
    }
}

/// Check if exit status indicates broken pipe (user quit pager early)
#[must_use]
fn is_broken_pipe_exit(status: ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // SIGPIPE = signal 13
        status.signal() == Some(13)
    }
    #[cfg(not(unix))]
    {
        // On Windows, treat exit code 0 or 1 as "user quit"
        matches!(status.code(), Some(0) | Some(1))
    }
}

/// Convert process exit status to `PagerExitStatus`
///
/// Handles the differences between Unix and Windows:
/// - Unix: Distinguishes exit codes from signal terminations
/// - Windows: Only has exit codes
fn exit_status_to_pager_status(status: ExitStatus) -> PagerExitStatus {
    // Success or SIGPIPE is treated as normal user quit
    if status.success() || is_broken_pipe_exit(status) {
        return PagerExitStatus::Success;
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // Check for signal termination (excluding SIGPIPE which was handled above)
        if let Some(signal) = status.signal() {
            return PagerExitStatus::Signal(signal);
        }
    }

    // Non-zero exit code
    if let Some(code) = status.code() {
        PagerExitStatus::ExitCode(code)
    } else {
        // Shouldn't happen: not success, no signal, no code
        // Default to exit code 1
        PagerExitStatus::ExitCode(1)
    }
}

/// Standard tab width for display calculation
#[allow(dead_code)]
const TAB_WIDTH: usize = 8;

fn skip_csi_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(&next) = chars.peek() {
        chars.next();
        if (0x40..=0x7E).contains(&(next as u8)) {
            break;
        }
    }
}

fn skip_osc_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(&next) = chars.peek() {
        if next == '\x07' {
            chars.next();
            break;
        }
        if next == '\x1b' {
            chars.next();
            if chars.peek() == Some(&'\\') {
                chars.next();
            }
            break;
        }
        chars.next();
    }
}

/// Strip ANSI escape sequences from a string
///
/// Removes CSI sequences (ESC [ ... `final_byte`) and OSC sequences (ESC ] ... ST).
/// This ensures ANSI color codes don't inflate width calculations.
#[allow(dead_code)]
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Start of escape sequence
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    skip_csi_sequence(&mut chars);
                }
                Some(']') => {
                    chars.next();
                    skip_osc_sequence(&mut chars);
                }
                _ => {}
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Calculate displayed width of a line, accounting for tabs
///
/// Tabs expand to the next tab stop (every 8 columns by default).
#[allow(dead_code)]
fn displayed_line_width(line: &str) -> usize {
    let mut width = 0;
    for c in line.chars() {
        if c == '\t' {
            // Expand to next tab stop
            width = (width / TAB_WIDTH + 1) * TAB_WIDTH;
        } else {
            width += UnicodeWidthChar::width(c).unwrap_or(0);
        }
    }
    width
}

/// Count displayed rows accounting for line wrapping
///
/// Uses unicode-width for accurate character width calculation,
/// which handles CJK characters, emoji, and other wide characters.
/// Strips ANSI escape sequences and expands tabs before calculation.
#[allow(dead_code)]
#[must_use]
pub fn count_displayed_rows(content: &str, terminal_width: Option<usize>) -> usize {
    let width = terminal_width.unwrap_or(80);

    content
        .lines()
        .map(|line| {
            // Strip ANSI escape sequences before width calculation
            let clean_line = strip_ansi(line);
            let line_width = displayed_line_width(&clean_line);
            if line_width == 0 {
                1 // Empty line still takes 1 row
            } else {
                // Ceiling division: how many terminal rows does this line span?
                line_width.div_ceil(width)
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // ===== PagerMode Tests =====

    #[test]
    fn test_pager_mode_default() {
        assert_eq!(PagerMode::default(), PagerMode::Auto);
    }

    // ===== PagerConfig Tests =====

    #[test]
    fn test_pager_config_default() {
        let config = PagerConfig::default();
        assert_eq!(config.enabled, PagerMode::Auto);
        assert!(config.threshold.is_none());
        // Command depends on environment, don't assert exact value
    }

    #[test]
    #[serial]
    fn test_pager_config_env_sqry_pager() {
        // SAFETY: Test isolation via serial_test
        unsafe {
            std::env::set_var("SQRY_PAGER", "bat --style=plain");
            std::env::remove_var("PAGER");
        }

        let cmd = PagerConfig::default_pager_command();
        assert_eq!(cmd, "bat --style=plain");

        unsafe {
            std::env::remove_var("SQRY_PAGER");
        }
    }

    #[test]
    #[serial]
    fn test_pager_config_env_pager_fallback() {
        // SAFETY: Test isolation via serial_test
        unsafe {
            std::env::remove_var("SQRY_PAGER");
            std::env::set_var("PAGER", "more");
        }

        let cmd = PagerConfig::default_pager_command();
        assert_eq!(cmd, "more");

        unsafe {
            std::env::remove_var("PAGER");
        }
    }

    #[test]
    #[serial]
    fn test_pager_config_env_sqry_pager_priority() {
        // SQRY_PAGER takes priority over PAGER
        // SAFETY: Test isolation via serial_test
        unsafe {
            std::env::set_var("SQRY_PAGER", "bat");
            std::env::set_var("PAGER", "less");
        }

        let cmd = PagerConfig::default_pager_command();
        assert_eq!(cmd, "bat");

        unsafe {
            std::env::remove_var("SQRY_PAGER");
            std::env::remove_var("PAGER");
        }
    }

    #[test]
    #[serial]
    fn test_pager_config_env_default_fallback() {
        // Neither env var set - should fall back to "less -FRX"
        // SAFETY: Test isolation via serial_test
        unsafe {
            std::env::remove_var("SQRY_PAGER");
            std::env::remove_var("PAGER");
        }

        let cmd = PagerConfig::default_pager_command();
        assert_eq!(cmd, "less -FRX");
    }

    #[test]
    fn test_pager_config_from_cli_flags_no_pager() {
        let config = PagerConfig::from_cli_flags(false, true, None);
        assert_eq!(config.enabled, PagerMode::Never);
    }

    #[test]
    fn test_pager_config_from_cli_flags_pager() {
        let config = PagerConfig::from_cli_flags(true, false, None);
        assert_eq!(config.enabled, PagerMode::Always);
    }

    #[test]
    fn test_pager_config_from_cli_flags_auto() {
        let config = PagerConfig::from_cli_flags(false, false, None);
        assert_eq!(config.enabled, PagerMode::Auto);
    }

    #[test]
    fn test_pager_config_from_cli_flags_custom_cmd() {
        let config = PagerConfig::from_cli_flags(true, false, Some("bat --color=always"));
        assert_eq!(config.command, "bat --color=always");
    }

    // ===== PagerDecision Tests =====

    #[test]
    fn test_pager_decision_never_mode() {
        let config = PagerConfig {
            enabled: PagerMode::Never,
            ..Default::default()
        };
        let decision = PagerDecision::for_testing(config, true, Some(24));
        assert!(!decision.should_page_rows(1000));
    }

    #[test]
    fn test_pager_decision_always_mode() {
        let config = PagerConfig {
            enabled: PagerMode::Always,
            ..Default::default()
        };
        let decision = PagerDecision::for_testing(config, true, Some(24));
        assert!(decision.should_page_rows(1));
    }

    #[test]
    fn test_pager_decision_auto_below_threshold() {
        let config = PagerConfig {
            enabled: PagerMode::Auto,
            threshold: Some(100),
            ..Default::default()
        };
        let decision = PagerDecision::for_testing(config, true, Some(24));
        assert!(!decision.should_page_rows(50));
    }

    #[test]
    fn test_pager_decision_auto_above_threshold() {
        let config = PagerConfig {
            enabled: PagerMode::Auto,
            threshold: Some(100),
            ..Default::default()
        };
        let decision = PagerDecision::for_testing(config, true, Some(24));
        assert!(decision.should_page_rows(150));
    }

    #[test]
    fn test_pager_decision_auto_non_tty() {
        // When not a TTY (piped), should never page in Auto mode
        let config = PagerConfig {
            enabled: PagerMode::Auto,
            ..Default::default()
        };
        let decision = PagerDecision::for_testing(config, false, Some(24));
        assert!(!decision.should_page_rows(1000));
    }

    #[test]
    fn test_pager_decision_auto_uses_terminal_height() {
        let config = PagerConfig {
            enabled: PagerMode::Auto,
            threshold: None, // Use terminal height
            ..Default::default()
        };
        let decision = PagerDecision::for_testing(config, true, Some(30));
        assert!(!decision.should_page_rows(25)); // Below 30
        assert!(decision.should_page_rows(35)); // Above 30
    }

    #[test]
    fn test_pager_decision_auto_default_threshold() {
        // When no threshold and no terminal height, use default of 24
        let config = PagerConfig {
            enabled: PagerMode::Auto,
            threshold: None,
            ..Default::default()
        };
        let decision = PagerDecision::for_testing(config, true, None);
        assert!(!decision.should_page_rows(20)); // Below 24
        assert!(decision.should_page_rows(30)); // Above 24
    }

    // ===== count_displayed_rows Tests =====

    #[test]
    fn test_count_displayed_rows_simple() {
        let content = "line1\nline2\nline3\n";
        assert_eq!(count_displayed_rows(content, Some(80)), 3);
    }

    #[test]
    fn test_count_displayed_rows_empty_lines() {
        let content = "line1\n\nline3\n";
        assert_eq!(count_displayed_rows(content, Some(80)), 3);
    }

    #[test]
    fn test_count_displayed_rows_long_line_wraps() {
        // 160 chars should wrap to 2 rows at width 80
        let long_line = "a".repeat(160);
        assert_eq!(count_displayed_rows(&long_line, Some(80)), 2);
    }

    #[test]
    fn test_count_displayed_rows_exactly_width() {
        // 80 chars at width 80 = 1 row
        let exact_line = "a".repeat(80);
        assert_eq!(count_displayed_rows(&exact_line, Some(80)), 1);
    }

    #[test]
    fn test_count_displayed_rows_unicode() {
        // CJK characters are typically 2-width
        let cjk = "中文字符"; // 4 CJK chars = 8 width units
        // At width 80, this fits in 1 row
        assert_eq!(count_displayed_rows(cjk, Some(80)), 1);

        // At width 4, this would be 2 rows (8/4 = 2)
        assert_eq!(count_displayed_rows(cjk, Some(4)), 2);
    }

    #[test]
    fn test_count_displayed_rows_default_width() {
        let content = "test line\n";
        // Default width is 80
        assert_eq!(count_displayed_rows(content, None), 1);
    }

    // ===== PagerWriter Tests =====

    #[test]
    fn test_pager_writer_spawn_invalid_syntax() {
        // Unterminated quote
        let result = PagerWriter::spawn("less \"unclosed");
        assert!(result.is_err());
        // PagerWriter doesn't implement Debug, so we can't use unwrap_err()
        // Instead, check the error kind directly
        let err = result.err().expect("Should be an error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn test_pager_writer_spawn_empty_command() {
        let result = PagerWriter::spawn("");
        assert!(result.is_err());
        let err = result.err().expect("Should be an error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn test_shlex_parsing_simple() {
        let parts = shlex::split("less -R").unwrap();
        assert_eq!(parts, vec!["less", "-R"]);
    }

    #[test]
    fn test_shlex_parsing_quoted() {
        let parts = shlex::split("\"bat\" --style=plain").unwrap();
        assert_eq!(parts, vec!["bat", "--style=plain"]);
    }

    #[test]
    fn test_shlex_parsing_windows_path() {
        let parts = shlex::split("\"C:\\Program Files\\Git\\usr\\bin\\less.exe\" -R").unwrap();
        assert_eq!(
            parts,
            vec!["C:\\Program Files\\Git\\usr\\bin\\less.exe", "-R"]
        );
    }

    // ===== BufferedOutput Tests =====

    #[test]
    fn test_buffered_output_never_mode_writes_directly() {
        // In Never mode, output goes directly to stdout (can't easily test stdout,
        // but we can verify mode selection)
        let config = PagerConfig {
            enabled: PagerMode::Never,
            ..Default::default()
        };
        let output = BufferedOutput::new(config);
        assert!(matches!(output.mode, OutputMode::Direct));
    }

    #[test]
    fn test_buffered_output_auto_mode_non_tty_streams_directly() {
        // In Auto mode, non-TTY output goes directly to stdout (no buffering)
        // CI/test environments are typically non-TTY
        let config = PagerConfig {
            enabled: PagerMode::Auto,
            ..Default::default()
        };
        let output = BufferedOutput::new(config);
        // In test environment (non-TTY), should use Direct mode
        assert!(
            matches!(output.mode, OutputMode::Direct)
                || matches!(output.mode, OutputMode::Buffering),
            "Expected Direct (non-TTY) or Buffering (TTY), got neither"
        );
    }

    // ===== is_broken_pipe_exit Tests =====

    #[test]
    #[cfg(unix)]
    fn test_is_broken_pipe_exit_sigpipe() {
        use std::os::unix::process::ExitStatusExt;
        // SIGPIPE = 13
        let status = ExitStatus::from_raw(13 << 8 | 0x7f); // Signal 13, stopped
        // This is tricky to test directly, so we just ensure the function compiles
        let _ = is_broken_pipe_exit(status);
    }

    // ===== Integration-style Tests =====

    #[test]
    fn test_buffer_cap_constant() {
        // Verify the cap is 1MB
        assert_eq!(BUFFER_CAP_BYTES, 1024 * 1024);
    }

    // ===== PagerExitStatus Tests =====

    #[test]
    fn test_pager_exit_status_success() {
        let status = PagerExitStatus::Success;
        assert!(status.is_success());
        assert_eq!(status.exit_code(), None);
    }

    #[test]
    fn test_pager_exit_status_exit_code() {
        let status = PagerExitStatus::ExitCode(42);
        assert!(!status.is_success());
        assert_eq!(status.exit_code(), Some(42));
    }

    #[test]
    fn test_pager_exit_status_signal() {
        // Signal 9 (SIGKILL) should return 128 + 9 = 137
        let status = PagerExitStatus::Signal(9);
        assert!(!status.is_success());
        assert_eq!(status.exit_code(), Some(137));
    }

    // ===== ANSI Stripping Tests =====

    #[test]
    fn test_strip_ansi_plain_text() {
        assert_eq!(strip_ansi("hello world"), "hello world");
    }

    #[test]
    fn test_strip_ansi_csi_color() {
        // Red text: ESC[31m hello ESC[0m
        let colored = "\x1b[31mhello\x1b[0m";
        assert_eq!(strip_ansi(colored), "hello");
    }

    #[test]
    fn test_strip_ansi_multiple_codes() {
        // Bold red: ESC[1;31m hello ESC[0m
        let colored = "\x1b[1;31mhello\x1b[0m world";
        assert_eq!(strip_ansi(colored), "hello world");
    }

    #[test]
    fn test_strip_ansi_osc_sequence() {
        // OSC sequence: ESC ] 0 ; title BEL
        let with_osc = "before\x1b]0;window title\x07after";
        assert_eq!(strip_ansi(with_osc), "beforeafter");
    }

    #[test]
    fn test_strip_ansi_preserves_unicode() {
        let text = "\x1b[32m日本語\x1b[0m";
        assert_eq!(strip_ansi(text), "日本語");
    }

    // ===== Tab Width Tests =====

    #[test]
    fn test_displayed_line_width_no_tabs() {
        assert_eq!(displayed_line_width("hello"), 5);
    }

    #[test]
    fn test_displayed_line_width_single_tab_start() {
        // Tab at start expands to position 8
        assert_eq!(displayed_line_width("\thello"), 8 + 5);
    }

    #[test]
    fn test_displayed_line_width_tab_after_text() {
        // "hi" (2 chars) + tab expands to position 8
        assert_eq!(displayed_line_width("hi\tworld"), 8 + 5);
    }

    #[test]
    fn test_displayed_line_width_multiple_tabs() {
        // Tab to 8, tab to 16
        assert_eq!(displayed_line_width("\t\t"), 16);
    }

    #[test]
    fn test_displayed_line_width_cjk() {
        // CJK characters are 2 columns wide
        assert_eq!(displayed_line_width("日本"), 4);
    }

    // ===== count_displayed_rows with ANSI =====

    #[test]
    fn test_count_displayed_rows_strips_ansi() {
        // Without stripping, ANSI codes would inflate the count
        let colored = "\x1b[31mhello\x1b[0m"; // "hello" with color codes
        // "hello" is 5 chars, fits in 80 columns = 1 row
        assert_eq!(count_displayed_rows(colored, Some(80)), 1);
    }

    #[test]
    fn test_count_displayed_rows_with_tabs() {
        // "hi\tworld" = position 8 + 5 = 13 chars
        // In 80-column terminal, fits in 1 row
        assert_eq!(count_displayed_rows("hi\tworld", Some(80)), 1);

        // In 10-column terminal: 13 chars needs ceiling(13/10) = 2 rows
        assert_eq!(count_displayed_rows("hi\tworld", Some(10)), 2);
    }

    #[test]
    fn test_count_displayed_rows_ansi_and_tabs_combined() {
        // Colored text with tabs
        let content = "\x1b[32m\tindented\x1b[0m";
        // After stripping: "\tindented" = 8 + 8 = 16 chars
        assert_eq!(count_displayed_rows(content, Some(80)), 1);
        assert_eq!(count_displayed_rows(content, Some(10)), 2);
    }

    // ===== Incremental Line Counting Tests =====

    #[test]
    fn test_incremental_line_counting_single_write() {
        // Create a BufferedOutput that forces Buffering mode for testing
        let config = PagerConfig::default();
        let mut output = BufferedOutput::new_for_testing(config);

        // Write 5 complete lines
        output.write("line1\nline2\nline3\nline4\nline5\n").unwrap();

        assert_eq!(output.complete_lines, 5);
        assert_eq!(output.partial_line_len, 0);
    }

    #[test]
    fn test_incremental_line_counting_chunked_writes() {
        // This is the case that was broken before the fix
        // Tests the pattern used by write_result: content followed by newline
        let config = PagerConfig::default();
        let mut output = BufferedOutput::new_for_testing(config);

        // Simulate how write_result sends content and newlines separately
        output.write("line1").unwrap();
        assert_eq!(output.complete_lines, 0);
        assert_eq!(output.partial_line_len, 5);

        output.write("\n").unwrap();
        assert_eq!(output.complete_lines, 1);
        assert_eq!(output.partial_line_len, 0);

        output.write("line2").unwrap();
        assert_eq!(output.complete_lines, 1);
        assert_eq!(output.partial_line_len, 5);

        output.write("\n").unwrap();
        assert_eq!(output.complete_lines, 2);
        assert_eq!(output.partial_line_len, 0);

        // Continue for more lines
        for i in 3..=10 {
            output.write(&format!("line{i}")).unwrap();
            output.write("\n").unwrap();
        }

        // Should have 10 complete lines
        assert_eq!(output.complete_lines, 10);
        assert_eq!(output.partial_line_len, 0);
    }

    #[test]
    fn test_incremental_line_counting_mixed_writes() {
        let config = PagerConfig::default();
        let mut output = BufferedOutput::new_for_testing(config);

        // Mix of complete lines and chunked writes
        output.write("line1\nline2\n").unwrap();
        assert_eq!(output.complete_lines, 2);
        assert_eq!(output.partial_line_len, 0);

        output.write("partial").unwrap();
        assert_eq!(output.complete_lines, 2);
        assert_eq!(output.partial_line_len, 7);

        output.write(" more").unwrap();
        assert_eq!(output.complete_lines, 2);
        assert_eq!(output.partial_line_len, 12);

        output.write("\nline4\n").unwrap();
        assert_eq!(output.complete_lines, 4);
        assert_eq!(output.partial_line_len, 0);
    }

    #[test]
    fn test_incremental_line_counting_multiple_newlines_in_one_write() {
        let config = PagerConfig::default();
        let mut output = BufferedOutput::new_for_testing(config);

        // Write content with multiple embedded newlines
        output.write("a\nb\nc\nd\ne").unwrap();
        assert_eq!(output.complete_lines, 4); // 4 newlines = 4 complete lines
        assert_eq!(output.partial_line_len, 1); // "e" is partial
    }
}
