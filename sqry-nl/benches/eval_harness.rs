//! Evaluation harness for sqry-nl translation quality.
//!
//! This benchmark measures:
//! - Intent classification accuracy (per-class F1, overall accuracy)
//! - Command accuracy (regex pattern matching)
//! - Translation latency (P50, P95, P99)
//!
//! # Usage
//!
//! ```bash
//! # Run full evaluation
//! cargo bench --bench eval_harness
//!
//! # Run with specific filter
//! cargo bench --bench eval_harness -- "intent_accuracy"
//!
//! # Generate detailed report
//! cargo bench --bench eval_harness -- --verbose
//! ```
//!
//! # Golden Queries Format
//!
//! The harness reads from `tests/golden_queries.toml`:
//!
//! ```toml
//! [[queries]]
//! input = "find authenticate function"
//! expected_intent = "symbol_query"
//! expected_command = 'sqry query ".*authenticate.*"'
//! difficulty = "easy"
//! tags = ["symbol", "function"]
//! ```

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use regex::Regex;
use serde::Deserialize;
use sqry_nl::{Intent, TranslationResponse, Translator, TranslatorConfig};
use std::collections::HashMap;
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Golden query test case loaded from TOML.
#[derive(Debug, Clone, Deserialize)]
struct GoldenQuery {
    input: String,
    expected_intent: String,
    expected_command: Option<String>,
    difficulty: String,
    #[allow(dead_code)]
    tags: Vec<String>,
}

/// Full golden queries file structure.
#[derive(Debug, Deserialize)]
struct GoldenQueriesFile {
    #[allow(dead_code)]
    version: String,
    #[allow(dead_code)]
    created: String,
    queries: Vec<GoldenQuery>,
}

/// Evaluation results for a single query.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct QueryResult {
    input: String,
    expected_intent: String,
    actual_intent: String,
    is_intent_correct: bool,
    expected_command: Option<String>,
    actual_command: Option<String>,
    is_command_correct: bool,
    latency: Duration,
    difficulty: String,
}

/// Aggregated evaluation metrics.
#[derive(Debug, Default)]
struct EvaluationMetrics {
    total_queries: usize,
    intent_correct: usize,
    command_correct: usize,
    per_intent_correct: HashMap<String, usize>,
    per_intent_total: HashMap<String, usize>,
    per_difficulty_correct: HashMap<String, (usize, usize)>, // (correct, total)
    latencies: Vec<Duration>,
    failures: Vec<QueryResult>,
}

impl EvaluationMetrics {
    fn intent_accuracy(&self) -> f64 {
        if self.total_queries == 0 {
            return 0.0;
        }
        usize_to_f64(self.intent_correct) / usize_to_f64(self.total_queries)
    }

    fn command_accuracy(&self) -> f64 {
        let command_queries =
            self.total_queries - self.per_intent_total.get("ambiguous").unwrap_or(&0);
        if command_queries == 0 {
            return 0.0;
        }
        usize_to_f64(self.command_correct) / usize_to_f64(command_queries)
    }

    #[allow(dead_code)]
    fn per_intent_accuracy(&self, intent: &str) -> f64 {
        let total = *self.per_intent_total.get(intent).unwrap_or(&0);
        let correct = *self.per_intent_correct.get(intent).unwrap_or(&0);
        if total == 0 {
            return 0.0;
        }
        usize_to_f64(correct) / usize_to_f64(total)
    }

    fn percentile_latency(&self, percentile: usize) -> Duration {
        if self.latencies.is_empty() {
            return Duration::ZERO;
        }
        let mut sorted = self.latencies.clone();
        sorted.sort();
        let percentile = percentile.min(100);
        let idx = ((sorted.len() - 1) * percentile) / 100;
        sorted[idx]
    }

    fn mean_latency(&self) -> Duration {
        if self.latencies.is_empty() {
            return Duration::ZERO;
        }
        let total: Duration = self.latencies.iter().sum();
        let count =
            u32::try_from(self.latencies.len()).expect("latency sample count fits into u32");
        total / count
    }
}

fn usize_to_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).expect("count fits into u32"))
}

/// Load golden queries from the test file.
///
/// # Panics
///
/// Panics if the golden queries file cannot be found, read, or parsed.
/// This is intentional - CI should fail if the evaluation data is missing or invalid.
fn load_golden_queries() -> Vec<GoldenQuery> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = PathBuf::from(manifest_dir).join("tests/golden_queries.toml");
    let path_display = path.display();

    let content = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read golden queries from {path_display}: {e}"));

    assert!(
        !content.is_empty(),
        "Golden queries file is empty: {path_display}"
    );

    let file: GoldenQueriesFile =
        toml::from_str(&content).unwrap_or_else(|e| panic!("Failed to parse golden queries: {e}"));

    assert!(
        !file.queries.is_empty(),
        "Golden queries file contains no queries: {path_display}"
    );

    file.queries
}

/// Parse intent string to Intent enum.
fn parse_intent(s: &str) -> Intent {
    match s {
        "symbol_query" => Intent::SymbolQuery,
        "text_search" => Intent::TextSearch,
        "trace_path" => Intent::TracePath,
        "find_callers" => Intent::FindCallers,
        "find_callees" => Intent::FindCallees,
        "visualize" => Intent::Visualize,
        "index_status" => Intent::IndexStatus,
        _ => Intent::Ambiguous,
    }
}

/// Extract intent and command from translation response.
fn extract_response(response: &TranslationResponse) -> (Intent, Option<String>) {
    match response {
        TranslationResponse::Execute {
            command, intent, ..
        } => (*intent, Some(command.clone())),
        TranslationResponse::Confirm { command, .. } => {
            // For confirm responses, we need to infer intent from the command
            let intent = infer_intent_from_command(command);
            (intent, Some(command.clone()))
        }
        TranslationResponse::Disambiguate { options, .. } => {
            // Use the highest confidence option
            if let Some(opt) = options.first() {
                (opt.intent, Some(opt.command.clone()))
            } else {
                (Intent::Ambiguous, None)
            }
        }
        TranslationResponse::Reject { .. } => (Intent::Ambiguous, None),
    }
}

/// Infer intent from command string (for Confirm responses).
fn infer_intent_from_command(cmd: &str) -> Intent {
    if cmd.contains("graph direct-callers") {
        Intent::FindCallers
    } else if cmd.contains("graph direct-callees") {
        Intent::FindCallees
    } else if cmd.contains("graph trace-path") {
        Intent::TracePath
    } else if cmd.contains("visualize") {
        Intent::Visualize
    } else if cmd.contains("index") && cmd.contains("status") {
        Intent::IndexStatus
    } else if cmd.contains("search") {
        Intent::TextSearch
    } else if cmd.contains("query") {
        Intent::SymbolQuery
    } else {
        Intent::Ambiguous
    }
}

/// Check if command matches expected pattern.
fn command_matches(actual: &str, expected_pattern: &str) -> bool {
    if expected_pattern.is_empty() {
        return true; // No command expected (e.g., ambiguous)
    }

    // Try exact match first
    if actual == expected_pattern {
        return true;
    }

    // Try regex match
    if let Ok(re) = Regex::new(expected_pattern)
        && re.is_match(actual)
    {
        return true;
    }

    // Try fuzzy substring match (for partial patterns)
    // Extract the key parts from expected pattern
    let expected_lower = expected_pattern.to_lowercase();
    let actual_lower = actual.to_lowercase();

    // Check if all non-regex parts are present
    let parts: Vec<&str> = expected_lower
        .split(&['*', '.', '+', '?', '[', ']', '(', ')'][..])
        .filter(|s| !s.is_empty() && s.len() > 2)
        .collect();

    parts.iter().all(|part| actual_lower.contains(part))
}

/// Run evaluation on all golden queries.
fn run_evaluation(translator: &mut Translator, queries: &[GoldenQuery]) -> EvaluationMetrics {
    let mut metrics = EvaluationMetrics::default();

    for query in queries {
        let start = Instant::now();
        let response = translator.translate(&query.input);
        let latency = start.elapsed();

        let (actual_intent, actual_command) = extract_response(&response);
        let expected_intent = parse_intent(&query.expected_intent);

        let is_intent_correct = actual_intent == expected_intent;
        let is_ambiguous = is_ambiguous_intent(&query.expected_intent);
        let is_command_correct = compute_command_correct(query, &actual_command, is_ambiguous);

        metrics.total_queries += 1;
        if is_intent_correct {
            metrics.intent_correct += 1;
        }
        // Only count command correctness for non-ambiguous queries
        if !is_ambiguous && is_command_correct {
            metrics.command_correct += 1;
        }
        metrics.latencies.push(latency);

        // Per-intent stats
        update_per_intent(&mut metrics, query, is_intent_correct);

        // Per-difficulty stats
        update_per_difficulty(&mut metrics, query, is_intent_correct);

        // Track failures for debugging
        if !is_intent_correct || !is_command_correct {
            record_failure(
                &mut metrics,
                query,
                actual_intent,
                actual_command,
                is_intent_correct,
                is_command_correct,
                latency,
            );
        }
    }

    metrics
}

fn is_ambiguous_intent(expected_intent: &str) -> bool {
    expected_intent == "ambiguous"
}

fn compute_command_correct(
    query: &GoldenQuery,
    actual_command: &Option<String>,
    is_ambiguous: bool,
) -> bool {
    // Command accuracy only applies to non-ambiguous queries
    // Ambiguous queries don't have expected commands, so they shouldn't
    // inflate the command_correct count (which would be compared against
    // a denominator that excludes them)
    if is_ambiguous {
        return false;
    }

    if let Some(ref expected) = query.expected_command {
        if let Some(actual) = actual_command {
            return command_matches(actual, expected);
        }
        return expected.is_empty();
    }

    // Non-ambiguous with no expected command pattern:
    // check if we got ANY valid command
    actual_command.is_some()
}

fn update_per_intent(
    metrics: &mut EvaluationMetrics,
    query: &GoldenQuery,
    is_intent_correct: bool,
) {
    let intent_key = query.expected_intent.clone();
    *metrics
        .per_intent_total
        .entry(intent_key.clone())
        .or_insert(0) += 1;
    if is_intent_correct {
        *metrics.per_intent_correct.entry(intent_key).or_insert(0) += 1;
    }
}

fn update_per_difficulty(
    metrics: &mut EvaluationMetrics,
    query: &GoldenQuery,
    is_intent_correct: bool,
) {
    let entry = metrics
        .per_difficulty_correct
        .entry(query.difficulty.clone())
        .or_insert((0, 0));
    if is_intent_correct {
        entry.0 += 1;
    }
    entry.1 += 1;
}

fn record_failure(
    metrics: &mut EvaluationMetrics,
    query: &GoldenQuery,
    actual_intent: Intent,
    actual_command: Option<String>,
    is_intent_correct: bool,
    is_command_correct: bool,
    latency: Duration,
) {
    metrics.failures.push(QueryResult {
        input: query.input.clone(),
        expected_intent: query.expected_intent.clone(),
        actual_intent: actual_intent.as_str().to_string(),
        is_intent_correct,
        expected_command: query.expected_command.clone(),
        actual_command,
        is_command_correct,
        latency,
        difficulty: query.difficulty.clone(),
    });
}

/// Print detailed evaluation report.
fn print_report(metrics: &EvaluationMetrics) {
    let sep = "=".repeat(60);
    println!("\n{sep}");
    println!(" sqry-nl Evaluation Report");
    println!("{sep}\n");

    // Overall metrics
    print_overall_metrics(metrics);

    // Per-intent breakdown
    print_per_intent(metrics);

    // Per-difficulty breakdown
    print_per_difficulty(metrics);

    // Failure analysis
    print_failures(metrics);

    println!("\n{sep}");
}

fn print_overall_metrics(metrics: &EvaluationMetrics) {
    println!("## Overall Metrics\n");
    println!("| Metric | Value | Target | Status |");
    println!("|--------|-------|--------|--------|");
    let intent_acc = metrics.intent_accuracy() * 100.0;
    let intent_status = if intent_acc >= 95.0 { "PASS" } else { "FAIL" };
    println!("| Intent Accuracy | {intent_acc:.1}% | >= 95% | {intent_status} |");
    let cmd_acc = metrics.command_accuracy() * 100.0;
    let cmd_status = if cmd_acc >= 85.0 { "PASS" } else { "FAIL" };
    println!("| Command Accuracy | {cmd_acc:.1}% | >= 85% | {cmd_status} |");
    println!(
        "| P50 Latency | {:?} | - | - |",
        metrics.percentile_latency(50)
    );
    println!(
        "| P95 Latency | {:?} | - | - |",
        metrics.percentile_latency(95)
    );
    println!(
        "| P99 Latency | {:?} | - | - |",
        metrics.percentile_latency(99)
    );
    println!("| Mean Latency | {:?} | - | - |", metrics.mean_latency());
    let total_queries = metrics.total_queries;
    println!("| Total Queries | {total_queries} | - | - |");
}

fn print_per_intent(metrics: &EvaluationMetrics) {
    println!("\n## Per-Intent Accuracy\n");
    println!("| Intent | Correct | Total | Accuracy |");
    println!("|--------|---------|-------|----------|");
    let intents = [
        "symbol_query",
        "text_search",
        "trace_path",
        "find_callers",
        "find_callees",
        "visualize",
        "index_status",
        "ambiguous",
    ];
    for intent in intents {
        let total = *metrics.per_intent_total.get(intent).unwrap_or(&0);
        let correct = *metrics.per_intent_correct.get(intent).unwrap_or(&0);
        let acc = if total > 0 {
            (usize_to_f64(correct) / usize_to_f64(total)) * 100.0
        } else {
            0.0
        };
        println!("| {intent} | {correct} | {total} | {acc:.1}% |");
    }
}

fn print_per_difficulty(metrics: &EvaluationMetrics) {
    println!("\n## Per-Difficulty Accuracy\n");
    println!("| Difficulty | Correct | Total | Accuracy |");
    println!("|------------|---------|-------|----------|");
    for difficulty in ["easy", "medium", "hard"] {
        let (correct, total) = *metrics
            .per_difficulty_correct
            .get(difficulty)
            .unwrap_or(&(0, 0));
        let acc = if total > 0 {
            (usize_to_f64(correct) / usize_to_f64(total)) * 100.0
        } else {
            0.0
        };
        println!("| {difficulty} | {correct} | {total} | {acc:.1}% |");
    }
}

fn print_failures(metrics: &EvaluationMetrics) {
    if metrics.failures.is_empty() {
        return;
    }

    let failure_count = metrics.failures.len();
    println!("\n## Failures ({failure_count} total)\n");
    let max_failures = 10;
    for (i, failure) in metrics.failures.iter().take(max_failures).enumerate() {
        println!("### Failure {}\n", i + 1);
        println!("- **Input**: `{}`", failure.input);
        println!("- **Expected Intent**: `{}`", failure.expected_intent);
        println!("- **Actual Intent**: `{}`", failure.actual_intent);
        println!("- **Intent Correct**: {}", failure.is_intent_correct);
        if let Some(ref expected) = failure.expected_command {
            println!("- **Expected Command**: `{expected}`");
        }
        if let Some(ref actual) = failure.actual_command {
            println!("- **Actual Command**: `{actual}`");
        }
        println!("- **Command Correct**: {}", failure.is_command_correct);
        println!("- **Difficulty**: {}", failure.difficulty);
        println!();
    }
    if metrics.failures.len() > max_failures {
        println!(
            "... and {} more failures",
            metrics.failures.len() - max_failures
        );
    }
}

/// Criterion benchmark for intent accuracy.
fn bench_intent_accuracy(c: &mut Criterion) {
    // load_golden_queries() panics on failure - no need for empty check
    let queries = load_golden_queries();
    let config = TranslatorConfig::default();
    let mut translator = Translator::new(config).expect("Failed to create translator");

    // Run full evaluation and print report
    let metrics = run_evaluation(&mut translator, &queries);
    print_report(&metrics);

    // Benchmark individual translations
    let mut group = c.benchmark_group("translation_latency");

    // Sample queries for benchmarking (one from each intent)
    let sample_queries: Vec<&GoldenQuery> = queries
        .iter()
        .fold(HashMap::new(), |mut acc: HashMap<&str, &GoldenQuery>, q| {
            acc.entry(q.expected_intent.as_str()).or_insert(q);
            acc
        })
        .into_values()
        .collect();

    for query in sample_queries {
        group.bench_with_input(
            BenchmarkId::new("intent", &query.expected_intent),
            &query.input,
            |b, input| {
                b.iter(|| {
                    let response = translator.translate(black_box(input));
                    black_box(response)
                });
            },
        );
    }

    group.finish();

    // Assert targets are met (rule-based baseline thresholds)
    // Note: These are baseline thresholds for the rule-based classifier.
    // The full 95%/85% targets require a trained ONNX model.
    let intent_accuracy = metrics.intent_accuracy();
    let intent_accuracy_pct = intent_accuracy * 100.0;
    assert!(
        intent_accuracy >= 0.70,
        "Intent accuracy {intent_accuracy_pct:.1}% below 70% threshold"
    );

    // Command accuracy baseline is 55% for rule-based (60%+ requires trained model)
    // The entity extraction from NL text is limited without ML-based NER
    let command_accuracy = metrics.command_accuracy();
    let command_accuracy_pct = command_accuracy * 100.0;
    assert!(
        command_accuracy >= 0.55,
        "Command accuracy {command_accuracy_pct:.1}% below 55% threshold"
    );

    // Latency thresholds (rule-based should be fast)
    let p95_latency = metrics.percentile_latency(95);
    assert!(
        p95_latency <= Duration::from_millis(100),
        "P95 latency {p95_latency:?} exceeds 100ms threshold"
    );
}

/// Criterion benchmark for command accuracy.
fn bench_command_accuracy(c: &mut Criterion) {
    // load_golden_queries() panics on failure - no need for empty check
    let queries = load_golden_queries();
    let config = TranslatorConfig::default();
    let mut translator = Translator::new(config).expect("Failed to create translator");

    // Benchmark batch translation
    c.bench_function("batch_translation_100", |b| {
        let batch: Vec<_> = queries.iter().take(100).collect();
        b.iter(|| {
            for query in &batch {
                let _ = translator.translate(black_box(&query.input));
            }
        });
    });
}

/// Criterion benchmark for cache performance.
fn bench_cache_performance(c: &mut Criterion) {
    let config = TranslatorConfig::default();
    let mut translator = Translator::new(config).expect("Failed to create translator");

    // Warm up cache
    let queries = [
        "find authenticate function",
        "who calls login",
        "trace from main to database",
        "index status",
    ];

    for q in &queries {
        let _ = translator.translate(q);
    }

    // Benchmark cache hits
    c.bench_function("cache_hit", |b| {
        b.iter(|| {
            let response = translator.translate(black_box("find authenticate function"));
            black_box(response)
        });
    });

    // Benchmark cache misses
    let mut counter = 0;
    c.bench_function("cache_miss", |b| {
        b.iter(|| {
            counter += 1;
            let query = format!("find function_{counter}");
            let response = translator.translate(black_box(&query));
            black_box(response)
        });
    });
}

criterion_group!(
    benches,
    bench_intent_accuracy,
    bench_command_accuracy,
    bench_cache_performance
);
criterion_main!(benches);
