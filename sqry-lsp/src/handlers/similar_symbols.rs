//! Similar symbols handler for LSP.
//!
//! Finds symbols similar to a reference symbol using fuzzy matching.

use anyhow::Result;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::{FileScope, ResolutionMode, SymbolQuery, SymbolResolutionOutcome};
use tower_lsp::lsp_types::{Location, Position, Range, Url};

use crate::protocol::{
    SqrySearchItem, SqrySimilarSymbol, SqrySimilarSymbolsParams, SqrySimilarSymbolsResult,
};
use crate::session::SessionManager;

/// Default maximum results
const DEFAULT_MAX_RESULTS: usize = 20;

/// Default similarity threshold
const DEFAULT_SIMILARITY_THRESHOLD: f64 = 0.7;

/// Execute similar symbols search.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved, inputs are invalid,
/// or the graph query fails.
#[allow(clippy::too_many_lines)]
pub fn execute(
    session: &SessionManager,
    params: &SqrySimilarSymbolsParams,
) -> Result<SqrySimilarSymbolsResult> {
    let root = session.resolve_path(params.path.as_deref())?;
    let file_path = params.file_path.trim();
    let symbol_name = params.symbol_name.trim();

    if file_path.is_empty() {
        anyhow::bail!("file_path cannot be empty");
    }
    if symbol_name.is_empty() {
        anyhow::bail!("symbol_name cannot be empty");
    }

    let max_results = params.max_results.unwrap_or(DEFAULT_MAX_RESULTS);
    let threshold = params
        .similarity_threshold
        .unwrap_or(DEFAULT_SIMILARITY_THRESHOLD);

    log::debug!(
        "Finding symbols similar to '{symbol_name}' in '{file_path}', threshold={threshold}"
    );

    let graph = session
        .graph()?
        .ok_or_else(|| anyhow::anyhow!("No graph available. Run `sqry index` first."))?;

    let snapshot = graph.snapshot();
    let strings = snapshot.strings();
    let files = snapshot.files();

    // Find the reference symbol
    let target_file = root.join(file_path);
    let target_relative = target_file.strip_prefix(&root).unwrap_or(&target_file);

    let ref_node_id =
        find_symbol_in_file(&snapshot, target_relative, symbol_name).ok_or_else(|| {
            anyhow::anyhow!("Reference symbol '{symbol_name}' not found in '{file_path}'")
        })?;

    let ref_entry = snapshot
        .get_node(ref_node_id)
        .ok_or_else(|| anyhow::anyhow!("Reference symbol node not found"))?;

    // Build reference symbol info
    let reference_name = strings
        .resolve(ref_entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    let reference_qualified_name = ref_entry
        .qualified_name
        .and_then(|id| strings.resolve(id))
        .map_or_else(|| reference_name.clone(), |s| s.to_string());

    let ref_kind = format!("{:?}", ref_entry.kind).to_lowercase();

    let ref_language = files
        .language_for_file(ref_entry.file)
        .map_or("unknown".to_string(), |l| {
            l.to_string().to_ascii_lowercase()
        });

    let ref_file_path = files
        .resolve(ref_entry.file)
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let ref_location = build_location(
        &root,
        &ref_file_path,
        ref_entry.start_line,
        ref_entry.start_column,
        ref_entry.end_line,
        ref_entry.end_column,
    );

    let reference = SqrySearchItem {
        name: reference_name.clone(),
        kind: ref_kind.clone(),
        qualified_name: reference_qualified_name.clone(),
        language: ref_language,
        location: ref_location,
        score: None,
    };

    // Find similar symbols by name similarity
    let mut similar: Vec<(NodeId, f64)> = Vec::new();

    for (node_id, entry) in snapshot.iter_nodes() {
        if node_id == ref_node_id {
            continue;
        }

        // Same kind filter
        if entry.kind != ref_entry.kind {
            continue;
        }

        let name = match strings.resolve(entry.name) {
            Some(n) => n.to_string(),
            None => continue,
        };

        let similarity = calculate_similarity(&reference_name, &name);
        if similarity >= threshold {
            similar.push((node_id, similarity));
        }
    }

    // Sort by similarity descending
    similar.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    similar.truncate(max_results);

    // Build result
    let similar_symbols: Vec<SqrySimilarSymbol> = similar
        .iter()
        .filter_map(|&(node_id, similarity)| {
            let entry = snapshot.get_node(node_id)?;
            let name = strings.resolve(entry.name)?.to_string();
            let qualified_name =
                crate::conversion::display_entry_qualified_name(entry, strings, files, &name);
            let kind = format!("{:?}", entry.kind).to_lowercase();
            let language = files
                .language_for_file(entry.file)
                .map_or("unknown".to_string(), |l| {
                    l.to_string().to_ascii_lowercase()
                });
            let file_path = files.resolve(entry.file)?.display().to_string();
            let location = build_location(
                &root,
                &file_path,
                entry.start_line,
                entry.start_column,
                entry.end_line,
                entry.end_column,
            );

            Some(SqrySimilarSymbol {
                symbol: SqrySearchItem {
                    name,
                    kind,
                    qualified_name,
                    language,
                    location,
                    score: Some(similarity_to_f32(similarity)),
                },
                similarity,
            })
        })
        .collect();

    let total = similar_symbols.len();

    Ok(SqrySimilarSymbolsResult {
        reference,
        similar: similar_symbols,
        total,
    })
}

/// Find symbol in file by name.
fn find_symbol_in_file(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    target_file: &std::path::Path,
    symbol_name: &str,
) -> Option<NodeId> {
    match snapshot.resolve_symbol(&SymbolQuery {
        symbol: symbol_name,
        file_scope: FileScope::Path(target_file),
        mode: ResolutionMode::Strict,
    }) {
        SymbolResolutionOutcome::Resolved(node_id) => Some(node_id),
        SymbolResolutionOutcome::NotFound
        | SymbolResolutionOutcome::FileNotIndexed
        | SymbolResolutionOutcome::Ambiguous(_) => None,
    }
}

/// Build LSP location.
fn build_location(
    workspace_root: &std::path::Path,
    file_path: &str,
    start_line: u32,
    start_column: u32,
    end_line: u32,
    end_column: u32,
) -> Location {
    let full_path = workspace_root.join(file_path);
    let uri = Url::from_file_path(&full_path)
        .unwrap_or_else(|()| Url::parse(&format!("file://{}", full_path.display())).unwrap());

    Location {
        uri,
        range: Range {
            start: Position::new(start_line.saturating_sub(1), start_column.saturating_sub(1)),
            end: Position::new(end_line.saturating_sub(1), end_column.saturating_sub(1)),
        },
    }
}

/// Calculate similarity between two strings using Levenshtein-based metric.
fn calculate_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }

    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();

    if a_lower == b_lower {
        return 0.95;
    }

    let max_len = a.len().max(b.len());
    if max_len == 0 {
        return 1.0;
    }

    let distance =
        f64::from(u32::try_from(levenshtein_distance(&a_lower, &b_lower)).unwrap_or(u32::MAX));
    let max_len = f64::from(u32::try_from(max_len).unwrap_or(u32::MAX));
    1.0 - (distance / max_len)
}

/// Levenshtein distance calculation.
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = usize::from(a_chars[i - 1] != b_chars[j - 1]);
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

#[allow(clippy::cast_possible_truncation)]
fn similarity_to_f32(similarity: f64) -> f32 {
    similarity as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // ── levenshtein_distance ─────────────────────────────────────────────────

    #[test]
    fn levenshtein_identical_strings() {
        assert_eq!(levenshtein_distance("hello", "hello"), 0);
    }

    #[test]
    fn levenshtein_empty_strings() {
        assert_eq!(levenshtein_distance("", ""), 0);
    }

    #[test]
    fn levenshtein_one_empty() {
        assert_eq!(levenshtein_distance("hello", ""), 5);
        assert_eq!(levenshtein_distance("", "hello"), 5);
    }

    #[test]
    fn levenshtein_single_substitution() {
        assert_eq!(levenshtein_distance("cat", "bat"), 1);
    }

    #[test]
    fn levenshtein_single_insertion() {
        assert_eq!(levenshtein_distance("cat", "cats"), 1);
    }

    #[test]
    fn levenshtein_single_deletion() {
        assert_eq!(levenshtein_distance("cats", "cat"), 1);
    }

    #[test]
    fn levenshtein_completely_different() {
        assert_eq!(levenshtein_distance("abc", "xyz"), 3);
    }

    // ── calculate_similarity ─────────────────────────────────────────────────

    #[test]
    fn similarity_identical_returns_one() {
        let s = calculate_similarity("process_data", "process_data");
        assert!((s - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn similarity_case_difference_returns_point_95() {
        let s = calculate_similarity("process_data", "PROCESS_DATA");
        assert!((s - 0.95).abs() < f64::EPSILON, "expected 0.95, got {s}");
    }

    #[test]
    fn similarity_both_empty_returns_one() {
        let s = calculate_similarity("", "");
        assert!((s - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn similarity_partially_similar_between_zero_and_one() {
        let s = calculate_similarity("process", "process_data");
        assert!(s > 0.0 && s < 1.0, "expected (0, 1), got {s}");
    }

    #[test]
    fn similarity_completely_different_less_than_one() {
        let s = calculate_similarity("aaa", "bbb");
        assert!(s < 1.0, "expected < 1.0 for different strings, got {s}");
    }

    // ── similarity_to_f32 ───────────────────────────────────────────────────

    #[test]
    fn similarity_to_f32_converts() {
        let f = similarity_to_f32(0.75);
        assert!((f - 0.75_f32).abs() < 1e-6_f32);
    }

    // ── build_location ───────────────────────────────────────────────────────

    #[test]
    fn build_location_converts_1based_to_0based() {
        let root = if cfg!(windows) {
            Path::new(r"C:\workspace")
        } else {
            Path::new("/workspace")
        };
        let loc = build_location(root, "src/lib.rs", 5, 3, 10, 1);
        // start_line 5 -> 4, start_column 3 -> 2
        assert_eq!(loc.range.start.line, 4);
        assert_eq!(loc.range.start.character, 2);
        // end_line 10 -> 9, end_column 1 -> 0
        assert_eq!(loc.range.end.line, 9);
        assert_eq!(loc.range.end.character, 0);
    }

    #[test]
    fn build_location_zero_lines_saturate() {
        let root = if cfg!(windows) {
            Path::new(r"C:\workspace")
        } else {
            Path::new("/workspace")
        };
        let loc = build_location(root, "src/lib.rs", 0, 0, 0, 0);
        assert_eq!(loc.range.start.line, 0);
        assert_eq!(loc.range.start.character, 0);
    }

    #[test]
    fn build_location_uri_contains_filename() {
        let root = if cfg!(windows) {
            Path::new(r"C:\workspace")
        } else {
            Path::new("/workspace")
        };
        let loc = build_location(root, "src/lib.rs", 1, 0, 1, 0);
        assert!(loc.uri.as_str().contains("lib.rs"));
    }
}
