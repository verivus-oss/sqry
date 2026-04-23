//! `sqry graph resolve <symbol> [--explain] [--json]`
//!
//! End-to-end proof point for the Phase 2 binding plane: loads a snapshot
//! from disk, constructs a `BindingPlane` facade, runs a resolution query,
//! and prints the outcome. With `--explain` the ordered step trace is rendered
//! via [`WitnessRendering`].
//!
//! # Output contracts
//!
//! **Text mode** (default):
//! ```text
//! symbol: <name>
//! outcome: <Debug repr of SymbolResolutionOutcome>
//!   - node_id=<NodeId> classification=<Classification> kind=<NodeKind>
//! [blank line + "witness trace:" section when --explain is given]
//! ```
//!
//! **JSON mode** (`--json`):
//! ```json
//! {
//!   "symbol":   "<name>",
//!   "outcome":  "<serde outcome>",
//!   "bindings": [
//!     { "node_id": "...", "classification": "...", "kind": "..." }
//!   ],
//!   "explain":  null | { "steps": [...], "candidate_count": N, ... }
//! }
//! ```
//! The JSON shape is the stable external contract for scripting consumers.
//! Changes to key names or value types are a breaking public-API change.
//!
//! Spec: `01_SPEC.md#fr9-end-to-end-proof`
//! Plan: `03_IMPLEMENTATION_PLAN.md` P2U10

use anyhow::{Context, Result};
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::resolution::{FileScope, ResolutionMode, SymbolQuery};

/// Runs the `sqry graph resolve` subcommand.
///
/// # Arguments
///
/// - `snapshot` — immutable MVCC snapshot, already loaded by the caller.
/// - `symbol`   — raw symbol text from the CLI argument.
/// - `explain`  — when `true`, the ordered witness step trace is appended
///   to the text output (or included as the `explain` field in JSON output).
/// - `json`     — when `true`, all output is emitted as a JSON document whose
///   shape is the stable external contract documented in the module doc.
///
/// # Errors
///
/// Returns an error only when JSON serialization fails (an internal invariant
/// violation — `serde_json` should never fail on our well-typed structs).
pub fn run(snapshot: &GraphSnapshot, symbol: &str, explain: bool, json: bool) -> Result<()> {
    let query = SymbolQuery {
        symbol,
        file_scope: FileScope::Any,
        mode: ResolutionMode::AllowSuffixCandidates,
    };

    let plane = snapshot.binding_plane();
    let resolution = plane.resolve(&query);

    if json {
        print_json(&plane, symbol, &resolution, explain)?;
    } else {
        print_text(&plane, symbol, &resolution, explain);
    }

    Ok(())
}

/// Renders the resolution result as human-readable text.
fn print_text(
    plane: &sqry_core::graph::unified::bind::BindingPlane<'_>,
    symbol: &str,
    resolution: &sqry_core::graph::unified::bind::BindingResolution,
    explain: bool,
) {
    println!("symbol: {symbol}");
    println!("outcome: {:?}", resolution.result.outcome);
    for binding in &resolution.result.bindings {
        println!(
            "  - node_id={:?} classification={:?} kind={:?}",
            binding.node_id, binding.classification, binding.kind
        );
    }
    if explain {
        let rendering = plane.explain(resolution);
        println!("\nwitness trace:");
        print!("{}", rendering.text);
    }
}

/// Renders the resolution result as a stable JSON document.
///
/// # Errors
///
/// Returns an error if `serde_json::to_string_pretty` fails (should not occur
/// for this well-typed document).
fn print_json(
    plane: &sqry_core::graph::unified::bind::BindingPlane<'_>,
    symbol: &str,
    resolution: &sqry_core::graph::unified::bind::BindingResolution,
    explain: bool,
) -> Result<()> {
    let bindings: Vec<serde_json::Value> = resolution
        .result
        .bindings
        .iter()
        .map(|b| {
            serde_json::json!({
                "node_id":        format!("{:?}", b.node_id),
                "classification": format!("{:?}", b.classification),
                "kind":           format!("{:?}", b.kind),
            })
        })
        .collect();

    let explain_value: serde_json::Value = if explain {
        let rendering = plane.explain(resolution);
        rendering.json.clone()
    } else {
        serde_json::Value::Null
    };

    let outcome_value = serde_json::to_value(&resolution.result.outcome)
        .unwrap_or_else(|_| serde_json::Value::String(format!("{:?}", resolution.result.outcome)));

    let doc = serde_json::json!({
        "symbol":   symbol,
        "outcome":  outcome_value,
        "bindings": bindings,
        "explain":  explain_value,
    });

    println!(
        "{}",
        serde_json::to_string_pretty(&doc).context("JSON serialization failed")?
    );
    Ok(())
}
