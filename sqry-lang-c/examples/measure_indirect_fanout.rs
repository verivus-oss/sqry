//! Per-callsite indirect-call fan-out measurement (DESIGN §5.1).
//!
//! This is the one-shot measurement binary referenced by Phase A
//! U16_FIXTURES. It builds a fresh sqry CodeGraph against the path
//! supplied on the command line (typically the
//! `test-fixtures/c-icall-precision/linux-driver-subset/` corpus),
//! walks every indirect callsite captured in
//! `CIndirectSideTables::pending_callsites`, and for each callsite
//! emits the **raw type-match candidate count** — i.e. step 3 of
//! pass5b's resolution algorithm in isolation, with NO binding-plane
//! refinement applied.
//!
//! Output shape (DESIGN §5.1 step 2):
//!
//! - Default: one integer per callsite, one per line — pipe-friendly for
//!   `scripts/measure/icall_fanout_histogram.py`.
//! - With `--verbose`: a `(file_path, line, expected_signature,
//!   candidate_count)` tuple per callsite on stderr alongside the
//!   integer-per-line stdout stream. The histogram script only needs
//!   stdout.
//!
//! Why this binary recomputes the candidate set instead of calling
//! `resolve_c_indirect_calls`: pass5b applies binding-plane refinement
//! and cardinality capping. The measurement under DESIGN §5.1 must
//! observe the **uncapped, unrefined** type-match fan-out so the cap
//! value itself can be calibrated against empirical p99. We replicate
//! pass5b's signature-recovery logic (struct-type-key for FieldExpr,
//! local-scope type token for PointerExpr) but stop short of consulting
//! `bindings_by_field` and short-circuit the `pass5b_c_indirect::CAP`
//! ceiling so the tail of the distribution is observable.

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::storage::c_indirect::{IndirectCallsite, IndirectShape};
use sqry_core::graph::unified::string::StringId;
use sqry_core::plugin::PluginManager;

/// Strip a leading `struct `/`union `/`enum ` keyword and trim trailing
/// pointer / qualifier characters from a `LocalScopeIndex` type token.
///
/// Mirrors `pass5b_c_indirect::strip_struct_keyword_and_pointer` (which
/// is module-private). Kept verbatim here so the measurement binary's
/// receiver-type normalisation matches pass5b's exactly.
fn strip_struct_keyword_and_pointer(type_token: &str) -> &str {
    let trimmed = type_token.trim();
    let after_keyword = trimmed
        .strip_prefix("struct ")
        .or_else(|| trimmed.strip_prefix("union "))
        .or_else(|| trimmed.strip_prefix("enum "))
        .unwrap_or(trimmed);
    after_keyword
        .trim()
        .trim_end_matches(|c: char| c == '*' || c.is_whitespace())
}

/// Build the reverse index `canonical_signature_StringId →
/// Vec<address-taken NodeId>`. Mirrors pass5b's private
/// `build_address_taken_signature_index` so callers do not depend on a
/// non-public symbol.
fn build_signature_index(graph: &CodeGraph) -> HashMap<StringId, Vec<NodeId>> {
    let mut out: HashMap<StringId, Vec<NodeId>> = HashMap::new();
    let Some(tables) = graph.c_indirect_tables() else {
        return out;
    };
    let metadata = graph.macro_metadata();
    for (&node_id, &sig) in &tables.fn_signature {
        if metadata.is_address_taken(node_id) {
            out.entry(sig).or_default().push(node_id);
        }
    }
    out
}

/// Compute the raw type-match candidate count for one callsite.
///
/// Returns `None` when the expected signature cannot be recovered
/// (no scope index for the file, no resolved receiver type, struct-
/// field key not interned, etc.). DESIGN §5.1's measurement protocol
/// treats unresolved callsites as not contributing to the
/// distribution — they would be `FallbackToStub` outcomes in pass5b
/// and are not part of the candidate-cardinality population.
fn raw_typematch_candidate_count(
    graph: &CodeGraph,
    callsite: &IndirectCallsite,
    signature_index: &HashMap<StringId, Vec<NodeId>>,
) -> Option<(usize, StringId)> {
    let tables = graph.c_indirect_tables()?;

    let expected_sig: StringId = match &callsite.shape {
        IndirectShape::FieldExpr {
            receiver_name,
            field_name,
        } => {
            let scope = tables.scope_index_for(callsite.file_id)?;
            let receiver_type = scope.resolve_type(receiver_name, callsite.use_span.0)?;
            let struct_tag = strip_struct_keyword_and_pointer(receiver_type);
            let strings = graph.strings();
            let struct_id = strings.get(struct_tag)?;
            let field_id = strings.get(field_name)?;
            *tables.struct_field_fnptr.get(&(struct_id, field_id))?
        }
        IndirectShape::PointerExpr { var_name } => {
            let scope = tables.scope_index_for(callsite.file_id)?;
            let type_token = scope.resolve_type(var_name, callsite.use_span.0)?;
            graph.strings().get(type_token)?
        }
    };

    let count = signature_index
        .get(&expected_sig)
        .map(|v| v.len())
        .unwrap_or(0);
    Some((count, expected_sig))
}

fn print_usage(program: &str) {
    eprintln!(
        "Usage: {program} [--verbose] <path-to-c-corpus>\n\n\
         Builds a sqry CodeGraph over <path>, walks every captured C\n\
         indirect callsite, and emits the raw type-match candidate count\n\
         per callsite (one integer per line on stdout). Use:\n\n\
         \tcargo run --example measure_indirect_fanout -- <path> | \\\n\
         \t    python3 scripts/measure/icall_fanout_histogram.py\n\n\
         to render the p50 / p75 / p90 / p95 / p99 / max histogram\n\
         used by Phase A U17_CAP_CALIBRATION."
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let program = args
        .first()
        .map(String::as_str)
        .unwrap_or("measure_indirect_fanout");

    // Tiny manual flag parser — no clap dependency, the example is one
    // command and one optional flag.
    let mut verbose = false;
    let mut positional: Option<&str> = None;
    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "--verbose" | "-v" => verbose = true,
            "--help" | "-h" => {
                print_usage(program);
                return ExitCode::SUCCESS;
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown flag `{other}`");
                print_usage(program);
                return ExitCode::from(2);
            }
            other => {
                if positional.is_some() {
                    eprintln!("error: multiple paths supplied");
                    print_usage(program);
                    return ExitCode::from(2);
                }
                positional = Some(other);
            }
        }
    }

    let Some(corpus_path) = positional else {
        print_usage(program);
        return ExitCode::from(2);
    };
    let corpus_path: PathBuf = Path::new(corpus_path).to_path_buf();
    if !corpus_path.exists() {
        eprintln!(
            "error: corpus path does not exist: {}",
            corpus_path.display()
        );
        return ExitCode::from(1);
    }

    // Build the workspace with the C plugin only. Non-C files are
    // ignored — the corpus is C-only by construction (DESIGN §13).
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(sqry_lang_c::CPlugin::new()));
    let config = BuildConfig::default();
    let graph = match build_unified_graph(&corpus_path, &plugins, &config) {
        Ok(g) => g,
        Err(e) => {
            eprintln!(
                "error: build_unified_graph failed for `{}`: {e}",
                corpus_path.display()
            );
            return ExitCode::from(1);
        }
    };

    let Some(tables) = graph.c_indirect_tables() else {
        eprintln!(
            "warning: graph has no c_indirect_tables — corpus contained no C \
             files or the C plugin staged nothing. Emitting empty output."
        );
        return ExitCode::SUCCESS;
    };

    let signature_index = build_signature_index(&graph);
    let total_callsites = tables.pending_callsites.len();
    let mut emitted = 0usize;
    let mut skipped = 0usize;

    if verbose {
        eprintln!(
            "measure_indirect_fanout: {} pending callsite(s) over {}",
            total_callsites,
            corpus_path.display()
        );
    }

    for callsite in &tables.pending_callsites {
        match raw_typematch_candidate_count(&graph, callsite, &signature_index) {
            Some((count, sig_id)) => {
                // One integer per line on stdout — the histogram
                // script's sole input contract.
                println!("{count}");
                emitted += 1;

                if verbose {
                    let path = graph
                        .files()
                        .resolve(callsite.file_id)
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "<unknown>".to_string());
                    let sig = graph
                        .strings()
                        .resolve(sig_id)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "<unresolved-sig>".to_string());
                    eprintln!(
                        "  callsite: file={} byte_offset={} shape={:?} expected_sig={:?} candidates={count}",
                        path, callsite.use_span.0, callsite.shape, sig
                    );
                }
            }
            None => {
                skipped += 1;
            }
        }
    }

    if verbose {
        eprintln!(
            "measure_indirect_fanout: emitted={emitted} skipped={skipped} \
             (skipped = callsites whose expected signature could not be \
             recovered — they would be FallbackToStub in pass5b and are \
             excluded from the cap-calibration distribution per DESIGN §5.1)"
        );
    }

    ExitCode::SUCCESS
}
