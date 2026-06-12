//! Progress bar implementation for CLI operations

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use sqry_core::progress::{
    IndexProgress, NodeIngestCounts, ProgressReporter, SharedReporter, no_op_reporter,
};
use std::fmt::Write;
use std::io::{self, Write as IoWrite};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SLOW_INGEST_WARNING_SECS: u64 = 3;
// Total number of distinct tracker phases the unified graph build emits
// via `BuildProgressTracker::start_phase`. As of Phase A + Go T1 these are:
//   1-3 : Chunked structural indexing (parse / range-plan / semantic commit)
//   4   : Finalizing graph
//   5   : Binding plane derivation (Phase 4e)
//   6   : C indirect-call resolution (Phase A pass 5b)
//   7   : Go method-set satisfaction (T1.2 / T1.1 / T1.3)
//   8   : Cross-language linking (Pass 5)
// The denominator is purely cosmetic — it shapes the "Phase N/{total}"
// label in the CLI progress reporter. Bump it whenever a new tracker
// phase is added or removed in `sqry-core/src/graph/unified/build/entrypoint.rs`.
const TOTAL_GRAPH_PHASES: u8 = 8;

/// CLI progress reporter using indicatif
pub struct CliProgressReporter {
    multi: MultiProgress,
    file_bar: ProgressBar,
    stage_bar: ProgressBar,
    file_style: ProgressStyle,
    stage_bar_style: ProgressStyle,
    stage_spinner_style: ProgressStyle,
    state: Mutex<CliProgressState>,
}

#[derive(Default)]
struct CliProgressState {
    total_files: Option<usize>,
    file_bar_finished: bool,
    last_ingest_file: Option<String>,
}

impl CliProgressReporter {
    /// Create a new CLI progress reporter
    ///
    /// # Panics
    /// Panics if the progress bar template string is invalid.
    #[must_use]
    pub fn new() -> Self {
        let multi = MultiProgress::new();
        let file_bar = multi.add(ProgressBar::new(0));
        let stage_bar = multi.add(ProgressBar::new_spinner());

        let file_style = ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} files | {msg}")
            .unwrap()
            .progress_chars("=>-");
        let stage_bar_style = ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} | {msg}")
            .unwrap()
            .progress_chars("=>-");
        let stage_spinner_style = ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap();

        file_bar.set_style(file_style.clone());
        stage_bar.set_style(stage_spinner_style.clone());
        stage_bar.enable_steady_tick(std::time::Duration::from_millis(120));

        Self {
            multi,
            file_bar,
            stage_bar,
            file_style,
            stage_bar_style,
            stage_spinner_style,
            state: Mutex::new(CliProgressState::default()),
        }
    }

    /// Finish and clear the progress bar
    pub fn finish(&self) {
        self.file_bar.finish_and_clear();
        self.stage_bar.finish_and_clear();
        let _ = self.multi.clear();
    }

    fn handle_started(&self, total_files: usize) {
        let mut state = self.state.lock().unwrap();
        state.total_files = Some(total_files);
        self.file_bar.set_style(self.file_style.clone());
        self.file_bar.set_length(total_files as u64);
        self.file_bar.set_position(0);
        self.file_bar.set_message("Indexing files");
        self.stage_bar.set_style(self.stage_spinner_style.clone());
        self.stage_bar.set_message("Waiting for ingestion...");
    }

    fn handle_file_processing(&self, path: &Path, current: usize) {
        self.file_bar.set_style(self.file_style.clone());
        self.file_bar.set_position(current as u64);
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        self.file_bar.set_message(file_name.to_string());
        let mut state = self.state.lock().unwrap();
        if let Some(total_files) = state.total_files
            && current >= total_files
            && !state.file_bar_finished
        {
            self.file_bar
                .finish_with_message(format!("Files indexed: {total_files}"));
            state.file_bar_finished = true;
        }
    }

    fn handle_file_completed(&self, symbols: usize) {
        self.file_bar.set_message(format!("{symbols} symbols"));
    }

    fn handle_ingest_progress(
        &self,
        files_processed: usize,
        total_files: usize,
        total_symbols: usize,
        counts: &NodeIngestCounts,
        elapsed: std::time::Duration,
        eta: Option<std::time::Duration>,
    ) {
        self.stage_bar.set_style(self.stage_bar_style.clone());
        self.stage_bar.set_length(total_files as u64);
        self.stage_bar.set_position(files_processed as u64);
        let rate = format_rate(files_processed, elapsed);
        let eta_display = eta.map_or_else(|| "--:--".to_string(), format_duration_clock);
        let elapsed_display = format_duration_clock(elapsed);
        let file_hint = self.current_ingest_file();
        let file_suffix = file_hint
            .as_deref()
            .map(|name| format!(" | file: {name}"))
            .unwrap_or_default();
        let mut message = format!(
            "Ingesting symbols: {total_symbols} symbols | elapsed {elapsed_display} | eta {eta_display} | {rate}{file_suffix}"
        );
        let _ = write!(message, "\n({})", format_ingest_counts(counts));
        self.stage_bar.set_message(message);
    }

    fn handle_ingest_file_started(&self, path: &Path) {
        let file_label = ingest_file_label(path);
        {
            let mut state = self.state.lock().unwrap();
            state.last_ingest_file = Some(file_label.clone());
        }
        self.stage_bar.set_style(self.stage_bar_style.clone());
        self.stage_bar
            .set_message(format!("Ingesting {file_label}..."));
    }

    fn handle_ingest_file_completed(&self, path: &Path, symbols: usize, duration: Duration) {
        if is_slow_ingest(duration) {
            let warning = format!(
                "Warning: slow ingest ({duration:.2?}, {symbols} symbols): {}",
                path.display()
            );
            self.stage_bar.println(warning);
        }
    }

    fn current_ingest_file(&self) -> Option<String> {
        let state = self.state.lock().unwrap();
        state.last_ingest_file.clone()
    }

    fn handle_stage_started(&self, stage_name: &str) {
        self.stage_bar.set_style(self.stage_spinner_style.clone());
        self.stage_bar.set_message(format!("{stage_name}..."));
    }

    fn handle_stage_completed(&self, stage_name: &str, stage_duration: std::time::Duration) {
        self.stage_bar.set_style(self.stage_spinner_style.clone());
        self.stage_bar
            .set_message(format!("{stage_name} completed in {stage_duration:.2?}"));
    }

    fn handle_graph_phase_started(&self, phase_number: u8, phase_name: &str, total_items: usize) {
        if total_items == 0 {
            // Use spinner style when total is unknown/zero to avoid stuck "0/0" display
            self.stage_bar.set_style(self.stage_spinner_style.clone());
        } else {
            self.stage_bar.set_style(self.stage_bar_style.clone());
            self.stage_bar.set_length(total_items as u64);
        }
        self.stage_bar.set_position(0);
        self.stage_bar
            .set_message(format_graph_phase_message(phase_number, phase_name));
    }

    fn handle_graph_phase_progress(&self, items_processed: usize, total_items: usize) {
        self.stage_bar.set_position(items_processed as u64);
        if self.stage_bar.length() != Some(total_items as u64) {
            self.stage_bar.set_length(total_items as u64);
        }
    }

    fn handle_graph_phase_completed(
        &self,
        phase_number: u8,
        phase_name: &str,
        phase_duration: std::time::Duration,
    ) {
        self.stage_bar.set_message(format!(
            "{} completed in {phase_duration:.2?}",
            format_graph_phase_message(phase_number, phase_name)
        ));
    }

    fn handle_saving_started(&self, component_name: &str) {
        self.stage_bar.set_style(self.stage_spinner_style.clone());
        self.stage_bar
            .set_message(format!("Saving {component_name}..."));
    }

    fn handle_saving_completed(&self, component_name: &str, save_duration: std::time::Duration) {
        self.stage_bar
            .set_message(format!("Saved {component_name} in {save_duration:.2?}"));
    }

    fn handle_completed(&self, total_symbols: usize, duration: std::time::Duration) {
        self.stage_bar
            .set_message(format!("Indexed {total_symbols} symbols in {duration:.2?}"));
    }
}

impl ProgressReporter for CliProgressReporter {
    fn report(&self, event: IndexProgress) {
        match event {
            IndexProgress::Started { total_files } => {
                self.handle_started(total_files);
            }
            IndexProgress::FileProcessing {
                path,
                current,
                total: _,
            } => {
                self.handle_file_processing(&path, current);
            }
            IndexProgress::FileCompleted { symbols, .. } => {
                self.handle_file_completed(symbols);
            }
            IndexProgress::IngestProgress {
                files_processed,
                total_files,
                total_symbols,
                counts,
                elapsed,
                eta,
            } => {
                self.handle_ingest_progress(
                    files_processed,
                    total_files,
                    total_symbols,
                    &counts,
                    elapsed,
                    eta,
                );
            }
            IndexProgress::IngestFileStarted { path, .. } => {
                self.handle_ingest_file_started(&path);
            }
            IndexProgress::IngestFileCompleted {
                path,
                symbols,
                duration,
            } => {
                self.handle_ingest_file_completed(&path, symbols, duration);
            }
            IndexProgress::StageStarted { stage_name } => {
                self.handle_stage_started(stage_name);
            }
            IndexProgress::StageCompleted {
                stage_name,
                stage_duration,
            } => {
                self.handle_stage_completed(stage_name, stage_duration);
            }
            // Graph build phase events
            IndexProgress::GraphPhaseStarted {
                phase_number,
                phase_name,
                total_items,
            } => {
                self.handle_graph_phase_started(phase_number, phase_name, total_items);
            }
            IndexProgress::GraphPhaseProgress {
                items_processed,
                total_items,
                ..
            } => {
                self.handle_graph_phase_progress(items_processed, total_items);
            }
            IndexProgress::GraphPhaseCompleted {
                phase_number,
                phase_name,
                phase_duration,
            } => {
                self.handle_graph_phase_completed(phase_number, phase_name, phase_duration);
            }
            // Saving events
            IndexProgress::SavingStarted { component_name } => {
                self.handle_saving_started(component_name);
            }
            IndexProgress::SavingCompleted {
                component_name,
                save_duration,
            } => {
                self.handle_saving_completed(component_name, save_duration);
            }
            // Final completion - update message but don't finish the bar
            // The bar is finished explicitly via finish() method after all phases complete
            IndexProgress::Completed {
                total_symbols,
                duration,
            } => {
                self.handle_completed(total_symbols, duration);
            }
            // Handle any future variants gracefully
            _ => {}
        }
    }
}

fn format_ingest_counts(counts: &NodeIngestCounts) -> String {
    let mut parts = Vec::new();
    parts.push(format!("fn {}", format_count(counts.functions)));
    parts.push(format!("mth {}", format_count(counts.methods)));
    parts.push(format!("cls {}", format_count(counts.classes)));
    if counts.structs > 0 {
        parts.push(format!("struct {}", format_count(counts.structs)));
    }
    if counts.enums > 0 {
        parts.push(format!("enum {}", format_count(counts.enums)));
    }
    if counts.interfaces > 0 {
        parts.push(format!("iface {}", format_count(counts.interfaces)));
    }
    if counts.other > 0 {
        parts.push(format!("other {}", format_count(counts.other)));
    }
    parts.join(", ")
}

fn format_graph_phase_message(phase_number: u8, phase_name: &str) -> String {
    if phase_number == 1
        && phase_name == "Chunked structural indexing (parse -> range-plan -> semantic commit)"
    {
        return format!("Phase 1-3/{TOTAL_GRAPH_PHASES}: {phase_name}");
    }
    format!("Phase {phase_number}/{TOTAL_GRAPH_PHASES}: {phase_name}")
}

fn ingest_file_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.display().to_string(), ToString::to_string)
}

fn is_slow_ingest(duration: Duration) -> bool {
    duration >= Duration::from_secs(SLOW_INGEST_WARNING_SECS)
}

fn format_count(value: usize) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    let thousands = value / 1_000;
    let remainder = value % 1_000;
    if thousands < 10 {
        let tenths = remainder / 100;
        if tenths == 0 {
            format!("{thousands}k")
        } else {
            format!("{thousands}.{tenths}k")
        }
    } else {
        format!("{thousands}k")
    }
}

fn format_rate(files_processed: usize, elapsed: std::time::Duration) -> String {
    let elapsed_ms = elapsed.as_millis();
    if elapsed_ms == 0 {
        return "0 files/sec".to_string();
    }
    let files_processed = u128::from(files_processed as u64);
    let rate = (files_processed * 1_000) / elapsed_ms;
    format!("{rate} files/sec")
}

fn format_duration_clock(duration: std::time::Duration) -> String {
    let secs = duration.as_secs();
    let minutes = secs / 60;
    let seconds = secs % 60;
    if minutes < 60 {
        return format!("{minutes:02}:{seconds:02}");
    }
    let hours = minutes / 60;
    let rem_minutes = minutes % 60;
    format!("{hours}h{rem_minutes:02}m")
}

/// Step-level progress reporter for non-TTY output.
///
/// Emits coarse-grained progress messages without spamming.
pub struct CliStepProgressReporter {
    state: Mutex<StepState>,
}

#[derive(Default)]
struct StepState {
    total_files: Option<usize>,
}

impl CliStepProgressReporter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(StepState::default()),
        }
    }
}

impl Default for CliStepProgressReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressReporter for CliStepProgressReporter {
    fn report(&self, event: IndexProgress) {
        match event {
            IndexProgress::Started { total_files } => {
                let mut state = self.state.lock().unwrap();
                state.total_files = Some(total_files);
                println!("Indexing {total_files} files...");
            }
            IndexProgress::GraphPhaseStarted {
                phase_number,
                phase_name,
                total_items,
            } => {
                println!(
                    "{} ({total_items} items)...",
                    format_graph_phase_message(phase_number, phase_name)
                );
            }
            IndexProgress::GraphPhaseCompleted {
                phase_number,
                phase_name,
                phase_duration,
            } => {
                println!(
                    "{} completed in {phase_duration:.2?}",
                    format_graph_phase_message(phase_number, phase_name)
                );
            }
            IndexProgress::IngestProgress {
                files_processed,
                total_files: _,
                total_symbols,
                counts,
                elapsed,
                eta,
            } => {
                let rate = format_rate(files_processed, elapsed);
                let eta_display = eta.map_or_else(|| "--:--".to_string(), format_duration_clock);
                let elapsed_display = format_duration_clock(elapsed);
                println!(
                    "Ingesting symbols: {total_symbols} symbols | elapsed {elapsed_display} | eta {eta_display} | {rate}"
                );
                println!("({})", format_ingest_counts(&counts));
            }
            IndexProgress::IngestFileCompleted {
                path,
                symbols,
                duration,
            } => {
                if is_slow_ingest(duration) {
                    println!(
                        "Warning: slow ingest ({duration:.2?}, {symbols} symbols): {}",
                        path.display()
                    );
                }
            }
            IndexProgress::StageStarted { stage_name } => {
                println!("Stage: {stage_name}...");
            }
            IndexProgress::StageCompleted {
                stage_name,
                stage_duration,
            } => {
                println!("Stage: {stage_name} completed in {stage_duration:.2?}");
            }
            IndexProgress::SavingStarted { component_name } => {
                println!("Saving {component_name}...");
            }
            IndexProgress::SavingCompleted {
                component_name,
                save_duration,
            } => {
                println!("Saved {component_name} in {save_duration:.2?}");
            }
            IndexProgress::Completed {
                total_symbols,
                duration,
            } => {
                let total_files = self
                    .state
                    .lock()
                    .unwrap()
                    .total_files
                    .map_or_else(String::new, |count| format!(" across {count} files"));
                println!("Indexed {total_symbols} symbols{total_files} in {duration:.2?}");
            }
            _ => {}
        }
    }
}

/// Step runner for coarse-grained progress reporting.
pub struct StepRunner {
    enabled: bool,
    step_index: usize,
}

impl StepRunner {
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            step_index: 0,
        }
    }

    /// Run a named step and emit start/finish lines when enabled.
    ///
    /// # Errors
    ///
    /// Returns any error produced by the step action.
    pub fn step<T, E, F>(&mut self, name: &str, action: F) -> Result<T, E>
    where
        E: std::fmt::Display,
        F: FnOnce() -> Result<T, E>,
    {
        self.step_index += 1;
        let step_number = self.step_index;
        if self.enabled {
            println!("Step {step_number}: {name}...");
        }
        let start = Instant::now();
        let result = action();
        if self.enabled {
            match &result {
                Ok(_) => println!(
                    "Step {step_number}: {name} completed in {:.2?}",
                    start.elapsed()
                ),
                Err(err) => println!(
                    "Step {step_number}: {name} failed after {:.2?}: {err}",
                    start.elapsed()
                ),
            }
        }
        result
    }
}

impl Default for CliProgressReporter {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// PlainProgressReporter — `sqry search --verbose` sibling
// ============================================================================
//
// Coexists with `CliProgressReporter` (line 14, indicatif-backed, used by
// `sqry index`) and `CliStepProgressReporter` (line 403, step-style, also used
// by indexing). The search path needs a third reporter because indicatif's
// `MultiProgress` writes ANSI escape sequences and redrawing widgets — fine
// for an interactive TTY, wrong for `sqry search ... 2>&1 | grep` or scripted
// invocations.
//
// Output goes to stderr exclusively (so stdout pipelines stay clean). Two
// modes, selected by the `SQRY_OUTPUT_FORMAT` env var at construction:
//   - default ("plain" or unset): `[sqry] <stage> ...` / `[sqry] <stage>
//     complete in <X.YZms>` lines.
//   - "json": one JSON object per line, e.g.
//     `{"event":"stage_started","stage":"load snapshot","ts":1715472000123}`.
//
// `FileProcessing` events are rate-limited to one line per 250ms minimum so a
// 12M-symbol indexing run can't flood stderr.
//
// Contract notes:
//   - No ANSI escape sequences ever (CLI_PROGRESS_REPORTER acceptance #11).
//   - Stable stage-name strings — JSON consumers will key off them; renames
//     are a breaking change.
//   - `sqry index` golden output is byte-identical because nothing in this
//     impl block touches `CliProgressReporter` or `CliStepProgressReporter`.

/// Output format for [`PlainProgressReporter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlainOutputMode {
    /// `[sqry] <stage> ...` human-readable lines (default).
    Plain,
    /// One JSON object per line (`SQRY_OUTPUT_FORMAT=json`).
    Json,
}

impl PlainOutputMode {
    /// Read `SQRY_OUTPUT_FORMAT` once at construction. Anything other than
    /// `json` (case-insensitive) — including unset, empty, or "plain" —
    /// resolves to `Plain`.
    fn from_env() -> Self {
        std::env::var("SQRY_OUTPUT_FORMAT")
            .ok()
            .filter(|v| v.eq_ignore_ascii_case("json"))
            .map_or(Self::Plain, |_| Self::Json)
    }
}

/// Plain-text / JSON-line progress reporter for the `sqry search` path.
///
/// See module-level comment block above the type for the surface-ownership
/// rationale and output-format contract.
pub struct PlainProgressReporter {
    mode: PlainOutputMode,
    state: Mutex<PlainProgressState>,
}

#[derive(Default)]
struct PlainProgressState {
    /// Last time a `FileProcessing` line was emitted; used to enforce the
    /// 250ms minimum interval. `None` means none emitted yet.
    last_files_emit: Option<Instant>,
}

/// Minimum interval between successive `FileProcessing` emissions.
const PLAIN_FILES_RATE_LIMIT: Duration = Duration::from_millis(250);

impl PlainProgressReporter {
    /// Construct a reporter wired to the current `SQRY_OUTPUT_FORMAT` env.
    #[must_use]
    pub fn new() -> Self {
        Self {
            mode: PlainOutputMode::from_env(),
            state: Mutex::new(PlainProgressState::default()),
        }
    }

    /// Returns an `Arc<PlainProgressReporter>` when `verbose` is true,
    /// otherwise the canonical `no_op_reporter()` (so callers can use the
    /// same `SharedReporter` type regardless of opt-in state).
    ///
    /// Named `for_search` (not `for_cli`) to disambiguate against the
    /// existing `CliProgressReporter` (which is also a "CLI" reporter) and
    /// against `commands::graph::loader::load_unified_graph_for_cli`.
    #[must_use]
    pub fn for_search(verbose: bool) -> SharedReporter {
        if verbose {
            Arc::new(Self::new())
        } else {
            no_op_reporter()
        }
    }

    fn emit_stage_started(&self, stage_name: &'static str) {
        write_stage_started(&mut io::stderr().lock(), self.mode, stage_name);
    }

    fn emit_stage_completed(&self, stage_name: &'static str, duration: Duration) {
        write_stage_completed(&mut io::stderr().lock(), self.mode, stage_name, duration);
    }

    fn emit_summary(&self, total_symbols: usize, duration: Duration) {
        write_summary(&mut io::stderr().lock(), self.mode, total_symbols, duration);
    }

    fn emit_files(&self, current: usize, total: usize) {
        write_files(&mut io::stderr().lock(), self.mode, current, total);
    }
}

// Free-standing writers parameterised over `W: Write` so unit tests can
// drive them against an in-memory buffer. The `impl ProgressReporter` path
// above locks stderr and delegates here; tests use a `Vec<u8>`.

fn write_stage_started<W: IoWrite>(w: &mut W, mode: PlainOutputMode, stage_name: &'static str) {
    match mode {
        PlainOutputMode::Plain => {
            let _ = writeln!(w, "[sqry] {stage_name} ...");
        }
        PlainOutputMode::Json => {
            write_json(
                w,
                &[
                    ("event", JsonValue::Str("stage_started")),
                    ("stage", JsonValue::Str(stage_name)),
                    ("ts", JsonValue::Num(unix_millis())),
                ],
            );
        }
    }
}

fn write_stage_completed<W: IoWrite>(
    w: &mut W,
    mode: PlainOutputMode,
    stage_name: &'static str,
    duration: Duration,
) {
    match mode {
        PlainOutputMode::Plain => {
            let _ = writeln!(
                w,
                "[sqry] {stage_name} complete in {}",
                format_brief_duration(duration)
            );
        }
        PlainOutputMode::Json => {
            let ms = u128_to_u64_saturating(duration.as_millis());
            write_json(
                w,
                &[
                    ("event", JsonValue::Str("stage_completed")),
                    ("stage", JsonValue::Str(stage_name)),
                    ("duration_ms", JsonValue::Num(ms)),
                    ("ts", JsonValue::Num(unix_millis())),
                ],
            );
        }
    }
}

fn write_summary<W: IoWrite>(
    w: &mut W,
    mode: PlainOutputMode,
    total_symbols: usize,
    duration: Duration,
) {
    match mode {
        PlainOutputMode::Plain => {
            let _ = writeln!(
                w,
                "[sqry] indexing complete: {total_symbols} symbols in {}",
                format_brief_duration(duration)
            );
        }
        PlainOutputMode::Json => {
            let ms = u128_to_u64_saturating(duration.as_millis());
            write_json(
                w,
                &[
                    ("event", JsonValue::Str("completed")),
                    ("total_symbols", JsonValue::Num(total_symbols as u64)),
                    ("duration_ms", JsonValue::Num(ms)),
                    ("ts", JsonValue::Num(unix_millis())),
                ],
            );
        }
    }
}

fn write_files<W: IoWrite>(w: &mut W, mode: PlainOutputMode, current: usize, total: usize) {
    match mode {
        PlainOutputMode::Plain => {
            let _ = writeln!(w, "[sqry] files processed {current}/{total}");
        }
        PlainOutputMode::Json => {
            write_json(
                w,
                &[
                    ("event", JsonValue::Str("files_progress")),
                    ("current", JsonValue::Num(current as u64)),
                    ("total", JsonValue::Num(total as u64)),
                    ("ts", JsonValue::Num(unix_millis())),
                ],
            );
        }
    }
}

/// Emit a JSON object with the given key/value pairs, terminated by `\n`.
/// Hand-rolled (no `serde_json::Value` allocation) so the hot path stays
/// allocation-light. All values are owned by the caller as `&'static str`
/// or `u64`; debug-asserts guard against meta characters that would require
/// escaping.
fn write_json<W: IoWrite>(w: &mut W, fields: &[(&str, JsonValue)]) {
    let _ = w.write_all(b"{");
    for (i, (key, value)) in fields.iter().enumerate() {
        if i > 0 {
            let _ = w.write_all(b",");
        }
        debug_assert!(
            !key.contains('"') && !key.contains('\\'),
            "json key must not need escaping: {key}"
        );
        let _ = write!(w, "\"{key}\":");
        match value {
            JsonValue::Str(s) => {
                debug_assert!(
                    !s.contains('"') && !s.contains('\\'),
                    "json string value must not need escaping: {s}"
                );
                let _ = write!(w, "\"{s}\"");
            }
            JsonValue::Num(n) => {
                let _ = write!(w, "{n}");
            }
        }
    }
    let _ = w.write_all(b"}\n");
}

impl Default for PlainProgressReporter {
    fn default() -> Self {
        Self::new()
    }
}

/// Minimal JSON value type for the hand-rolled writer in `emit_json`.
enum JsonValue {
    Str(&'static str),
    Num(u64),
}

impl ProgressReporter for PlainProgressReporter {
    fn report(&self, event: IndexProgress) {
        match event {
            IndexProgress::StageStarted { stage_name } => {
                self.emit_stage_started(stage_name);
            }
            IndexProgress::StageCompleted {
                stage_name,
                stage_duration,
            } => {
                self.emit_stage_completed(stage_name, stage_duration);
            }
            IndexProgress::FileProcessing { current, total, .. } => {
                // Rate-limit to one line per 250ms. Hold the mutex only long
                // enough to read/update the timestamp — emission happens
                // after release so writers don't serialise on this lock.
                let should_emit = {
                    let mut state = match self.state.lock() {
                        Ok(g) => g,
                        // Mutex is poisoned: a previous emitter panicked.
                        // Recover the inner state (the only invariant is the
                        // timestamp, which is harmless to reset) and continue.
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    let now = Instant::now();
                    let allow = state
                        .last_files_emit
                        .is_none_or(|t| now.duration_since(t) >= PLAIN_FILES_RATE_LIMIT);
                    if allow {
                        state.last_files_emit = Some(now);
                    }
                    allow
                };
                if should_emit {
                    self.emit_files(current, total);
                }
            }
            IndexProgress::Completed {
                total_symbols,
                duration,
            } => {
                self.emit_summary(total_symbols, duration);
            }
            // Other events (FileCompleted, IngestProgress, IngestFile*,
            // GraphPhase*, Saving*) are intentionally not emitted — they
            // are indexing-internal and the search path doesn't trigger
            // them with enough density to matter. `non_exhaustive` on the
            // enum requires a wildcard.
            _ => {}
        }
    }
}

/// Format a `Duration` compactly: `<1ms` shows microseconds; `<1s` shows
/// milliseconds; otherwise shows seconds with two decimals.
fn format_brief_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs >= 1.0 {
        format!("{secs:.2}s")
    } else if d.as_millis() >= 1 {
        format!("{}ms", d.as_millis())
    } else {
        format!("{}us", d.as_micros())
    }
}

/// Best-effort millisecond unix timestamp for JSON-line events. Returns 0 if
/// the system clock is before the epoch (cannot happen in practice but
/// avoids a panic on a malformed clock).
fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u128_to_u64_saturating(d.as_millis()))
        .unwrap_or(0)
}

/// Convert `u128` to `u64` saturating at `u64::MAX`. `Duration::as_millis()`
/// returns u128 but we serialize as u64 — a u64 millis count is good for
/// ~584 million years, so this is safety more than necessity.
fn u128_to_u64_saturating(v: u128) -> u64 {
    if v > u128::from(u64::MAX) {
        u64::MAX
    } else {
        u64::try_from(v).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::{format_duration_clock, format_graph_phase_message, format_rate};
    use std::time::Duration;

    #[test]
    fn test_format_rate_zero_elapsed() {
        assert_eq!(format_rate(0, Duration::from_secs(0)), "0 files/sec");
    }

    #[test]
    fn test_format_rate_per_second() {
        assert_eq!(format_rate(1000, Duration::from_secs(1)), "1000 files/sec");
    }

    #[test]
    fn test_format_rate_fractional_seconds() {
        assert_eq!(format_rate(1500, Duration::from_secs(2)), "750 files/sec");
    }

    #[test]
    fn test_format_duration_clock_under_hour() {
        assert_eq!(format_duration_clock(Duration::from_secs(65)), "01:05");
    }

    #[test]
    fn test_format_duration_clock_hour_boundary() {
        assert_eq!(format_duration_clock(Duration::from_secs(3600)), "1h00m");
    }

    #[test]
    fn test_format_duration_clock_hours_minutes() {
        assert_eq!(format_duration_clock(Duration::from_secs(3720)), "1h02m");
    }

    #[test]
    fn test_format_graph_phase_message() {
        assert_eq!(
            format_graph_phase_message(
                1,
                "Chunked structural indexing (parse -> range-plan -> semantic commit)"
            ),
            "Phase 1-3/8: Chunked structural indexing (parse -> range-plan -> semantic commit)"
        );
    }
}

#[cfg(test)]
mod plain_reporter_tests {
    use super::{
        PLAIN_FILES_RATE_LIMIT, PlainOutputMode, PlainProgressReporter, format_brief_duration,
        write_files, write_stage_completed, write_stage_started, write_summary,
    };
    use sqry_core::progress::{IndexProgress, ProgressReporter};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    fn captured<F: FnOnce(&mut Vec<u8>)>(f: F) -> String {
        let mut buf = Vec::new();
        f(&mut buf);
        String::from_utf8(buf).expect("plain-reporter output must be valid utf8")
    }

    // ── format_brief_duration ────────────────────────────────────────

    #[test]
    fn format_brief_duration_sub_millis_uses_us() {
        let s = format_brief_duration(Duration::from_micros(42));
        assert_eq!(s, "42us");
    }

    #[test]
    fn format_brief_duration_sub_second_uses_ms() {
        let s = format_brief_duration(Duration::from_millis(150));
        assert_eq!(s, "150ms");
    }

    #[test]
    fn format_brief_duration_super_second_uses_two_decimals() {
        let s = format_brief_duration(Duration::from_millis(1240));
        assert_eq!(s, "1.24s");
    }

    // ── PlainOutputMode::from_env ────────────────────────────────────
    //
    // These tests serialize on a process-global env var, so they share a
    // mutex to avoid race-induced flakes. We DO NOT use #[serial_test]
    // because the test crate doesn't depend on it; a local Mutex is
    // sufficient because the lock scope is small.

    fn with_env<F: FnOnce()>(key: &str, value: Option<&str>, f: F) {
        // SAFETY: tests within this module take the SAME guard before
        // touching env, so there is no cross-test racing inside the crate.
        // Outside the crate, set/remove_var is unsafe in Rust 2024; the
        // mutex confines the unsafe to within-crate test runs.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var(key).ok();
        // SAFETY: see above — tests serialize on LOCK.
        unsafe {
            if let Some(v) = value {
                std::env::set_var(key, v);
            } else {
                std::env::remove_var(key);
            }
        }
        f();
        // SAFETY: restoring previous state under the same lock.
        unsafe {
            match prev {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn output_mode_default_is_plain_when_env_unset() {
        with_env("SQRY_OUTPUT_FORMAT", None, || {
            assert_eq!(PlainOutputMode::from_env(), PlainOutputMode::Plain);
        });
    }

    #[test]
    fn output_mode_json_when_env_eq_json() {
        with_env("SQRY_OUTPUT_FORMAT", Some("json"), || {
            assert_eq!(PlainOutputMode::from_env(), PlainOutputMode::Json);
        });
    }

    #[test]
    fn output_mode_json_is_case_insensitive() {
        with_env("SQRY_OUTPUT_FORMAT", Some("JSON"), || {
            assert_eq!(PlainOutputMode::from_env(), PlainOutputMode::Json);
        });
        with_env("SQRY_OUTPUT_FORMAT", Some("Json"), || {
            assert_eq!(PlainOutputMode::from_env(), PlainOutputMode::Json);
        });
    }

    #[test]
    fn output_mode_plain_when_env_unrecognised() {
        with_env("SQRY_OUTPUT_FORMAT", Some("yaml"), || {
            assert_eq!(PlainOutputMode::from_env(), PlainOutputMode::Plain);
        });
        with_env("SQRY_OUTPUT_FORMAT", Some(""), || {
            assert_eq!(PlainOutputMode::from_env(), PlainOutputMode::Plain);
        });
    }

    // ── for_search constructor ───────────────────────────────────────

    #[test]
    fn for_search_false_returns_silent_reporter() {
        let reporter = PlainProgressReporter::for_search(false);
        // The no_op reporter must accept events without writing anything;
        // we exercise it through the trait and assume sqry_core's
        // no_op_reporter contract holds.
        reporter.report(IndexProgress::StageStarted {
            stage_name: "test stage",
        });
        // No assertion needed — the contract is "must not panic, must not
        // write." The latter is enforced by no_op_reporter itself.
    }

    #[test]
    fn for_search_true_returns_plain_reporter() {
        let reporter = PlainProgressReporter::for_search(true);
        // Cannot downcast through SharedReporter without RTTI infrastructure;
        // instead, verify the contract behavior (events do not panic) and
        // rely on integration tests for stderr capture.
        reporter.report(IndexProgress::StageStarted {
            stage_name: "test stage",
        });
        reporter.report(IndexProgress::StageCompleted {
            stage_name: "test stage",
            stage_duration: Duration::from_millis(5),
        });
    }

    // ── Plain-mode line format ───────────────────────────────────────

    #[test]
    fn plain_stage_started_format() {
        let out = captured(|w| write_stage_started(w, PlainOutputMode::Plain, "load snapshot"));
        assert_eq!(out, "[sqry] load snapshot ...\n");
    }

    #[test]
    fn plain_stage_completed_format() {
        let out = captured(|w| {
            write_stage_completed(
                w,
                PlainOutputMode::Plain,
                "load snapshot",
                Duration::from_millis(150),
            );
        });
        assert_eq!(out, "[sqry] load snapshot complete in 150ms\n");
    }

    #[test]
    fn plain_summary_format() {
        let out = captured(|w| {
            write_summary(w, PlainOutputMode::Plain, 12345, Duration::from_millis(890));
        });
        assert_eq!(out, "[sqry] indexing complete: 12345 symbols in 890ms\n");
    }

    #[test]
    fn plain_files_format() {
        let out = captured(|w| write_files(w, PlainOutputMode::Plain, 5, 100));
        assert_eq!(out, "[sqry] files processed 5/100\n");
    }

    #[test]
    fn plain_output_contains_no_ansi_escape_sequences() {
        let buf = captured(|w| {
            write_stage_started(w, PlainOutputMode::Plain, "load snapshot");
            write_stage_completed(
                w,
                PlainOutputMode::Plain,
                "load snapshot",
                Duration::from_millis(150),
            );
            write_files(w, PlainOutputMode::Plain, 1, 2);
            write_summary(w, PlainOutputMode::Plain, 10, Duration::from_secs(1));
        });
        // Acceptance #11: no ANSI escape sequences.
        assert!(
            !buf.contains('\x1b'),
            "plain mode emitted an ANSI escape sequence: {buf:?}"
        );
    }

    // ── JSON-mode line format ────────────────────────────────────────

    fn parse_jsonl_line(line: &str) -> serde_json::Value {
        serde_json::from_str(line).expect("each json-line must parse as a JSON object")
    }

    #[test]
    fn json_stage_started_has_required_fields() {
        let out = captured(|w| write_stage_started(w, PlainOutputMode::Json, "exact name lookup"));
        assert!(out.ends_with('\n'), "json line must be newline-terminated");
        let v = parse_jsonl_line(out.trim_end());
        assert_eq!(v["event"], "stage_started");
        assert_eq!(v["stage"], "exact name lookup");
        assert!(v["ts"].is_number(), "ts must be a number");
    }

    #[test]
    fn json_stage_completed_has_duration_ms() {
        let out = captured(|w| {
            write_stage_completed(
                w,
                PlainOutputMode::Json,
                "load snapshot",
                Duration::from_millis(42),
            );
        });
        let v = parse_jsonl_line(out.trim_end());
        assert_eq!(v["event"], "stage_completed");
        assert_eq!(v["stage"], "load snapshot");
        assert_eq!(v["duration_ms"], 42);
    }

    #[test]
    fn json_files_progress_has_current_and_total() {
        let out = captured(|w| write_files(w, PlainOutputMode::Json, 7, 99));
        let v = parse_jsonl_line(out.trim_end());
        assert_eq!(v["event"], "files_progress");
        assert_eq!(v["current"], 7);
        assert_eq!(v["total"], 99);
    }

    #[test]
    fn json_summary_has_total_symbols() {
        let out = captured(|w| {
            write_summary(w, PlainOutputMode::Json, 3, Duration::from_millis(8));
        });
        let v = parse_jsonl_line(out.trim_end());
        assert_eq!(v["event"], "completed");
        assert_eq!(v["total_symbols"], 3);
        assert_eq!(v["duration_ms"], 8);
    }

    #[test]
    fn json_mode_emits_one_object_per_line() {
        let buf = captured(|w| {
            write_stage_started(w, PlainOutputMode::Json, "load snapshot");
            write_stage_completed(
                w,
                PlainOutputMode::Json,
                "load snapshot",
                Duration::from_millis(5),
            );
            write_stage_started(w, PlainOutputMode::Json, "exact name lookup");
        });
        let lines: Vec<&str> = buf.lines().collect();
        assert_eq!(lines.len(), 3, "expected exactly three JSON lines");
        for line in lines {
            let _ = parse_jsonl_line(line);
        }
    }

    // ── Rate limiting + stress ───────────────────────────────────────

    #[test]
    fn file_processing_rate_limit_is_250ms() {
        // The rate limit is the constant — verify the constant itself so a
        // future refactor that loosens or tightens it is forced to update
        // this test alongside the contract docstring.
        assert_eq!(PLAIN_FILES_RATE_LIMIT, Duration::from_millis(250));
    }

    #[test]
    fn report_does_not_panic_on_event_flood() {
        // Acceptance #9: no panics on event flood (10k+ events/sec).
        // We approximate "10k events/sec" with 50k synchronous events; if
        // the reporter is non-allocating in the hot path this completes in
        // well under a second on any CI runner.
        let reporter = Arc::new(PlainProgressReporter::new());
        for i in 0..50_000 {
            reporter.report(IndexProgress::StageStarted {
                stage_name: "stress",
            });
            reporter.report(IndexProgress::FileProcessing {
                path: std::path::PathBuf::from("/tmp/stress"),
                current: i,
                total: 50_000,
            });
        }
    }

    #[test]
    fn report_is_thread_safe_under_concurrent_emitters() {
        // ProgressReporter requires Send + Sync; exercise it from multiple
        // threads at once. The reporter must not deadlock or panic.
        let reporter = Arc::new(PlainProgressReporter::new());
        let handles: Vec<_> = (0..8)
            .map(|t| {
                let r = Arc::clone(&reporter);
                thread::spawn(move || {
                    for i in 0..1_000 {
                        r.report(IndexProgress::StageStarted {
                            stage_name: "concurrent",
                        });
                        r.report(IndexProgress::FileProcessing {
                            path: std::path::PathBuf::from(format!("/tmp/{t}/{i}")),
                            current: i,
                            total: 1_000,
                        });
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("worker thread must not panic");
        }
    }
}
