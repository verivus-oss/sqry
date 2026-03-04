//! Progress bar implementation for CLI operations

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use sqry_core::progress::{IndexProgress, NodeIngestCounts, ProgressReporter};
use std::fmt::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SLOW_INGEST_WARNING_SECS: u64 = 3;

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
        self.stage_bar.set_style(self.stage_bar_style.clone());
        self.stage_bar.set_length(total_items as u64);
        self.stage_bar.set_position(0);
        self.stage_bar
            .set_message(format!("Phase {phase_number}: {phase_name}"));
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
            "Phase {phase_number}: {phase_name} completed in {phase_duration:.2?}"
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
                println!("Graph phase {phase_number}: {phase_name} ({total_items} items)...");
            }
            IndexProgress::GraphPhaseCompleted {
                phase_number,
                phase_name,
                phase_duration,
            } => {
                println!(
                    "Graph phase {phase_number}: {phase_name} completed in {phase_duration:.2?}"
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

#[cfg(test)]
mod tests {
    use super::{format_duration_clock, format_rate};
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
}
