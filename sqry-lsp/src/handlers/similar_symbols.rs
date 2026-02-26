//! Similar symbols handler for LSP.
//!
//! Finds symbols similar to a reference symbol using fuzzy matching.

use anyhow::Result;
use sqry_core::graph::unified::node::NodeId;
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
            let qualified_name = entry
                .qualified_name
                .and_then(|id| strings.resolve(id))
                .map_or_else(|| name.clone(), |s| s.to_string());
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
    let files = snapshot.files();
    let strings = snapshot.strings();

    for (node_id, entry) in snapshot.iter_nodes() {
        let name = strings.resolve(entry.name)?;
        if name.as_ref() != symbol_name {
            continue;
        }

        if let Some(file_path) = files.resolve(entry.file)
            && file_path.as_ref() == target_file
        {
            return Some(node_id);
        }
    }

    None
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
