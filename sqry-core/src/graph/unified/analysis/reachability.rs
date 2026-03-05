//! 2-hop interval labeling and path reconstruction

use super::condensation::{CondensationDag, Interval};
use anyhow::Result;

/// Label data for 2-hop interval labeling
type LabelData = (Vec<u32>, Vec<Interval>, Vec<u32>, Vec<Interval>);

/// Compute 2-hop interval labels for a condensation DAG
///
/// Returns (`label_out_offsets`, `label_out_data`, `label_in_offsets`, `label_in_data`)
///
/// # Errors
///
/// Returns an error if the operation fails.
#[allow(clippy::cast_possible_truncation)] // Graph sizes realistically won't exceed u32::MAX
pub fn compute_2hop_labels(dag: &CondensationDag, budget: usize) -> Result<LabelData> {
    let scc_count = dag.scc_count as usize;

    // Step 1: Create position-based intervals for each SCC
    let base_intervals = compute_base_intervals(dag, scc_count);

    // Step 2: Build reverse adjacency for label_in computation
    let predecessors = build_predecessors(dag, scc_count);

    // Step 3: Compute label_out in reverse topological order
    let (label_out_data, total_out) = compute_label_out(dag, scc_count, &base_intervals, budget)?;

    // Step 4: Compute label_in in forward topological order
    let label_in_data = compute_label_in(
        dag,
        scc_count,
        &base_intervals,
        &predecessors,
        budget,
        total_out,
    )?;

    // Step 5: Flatten into offset-based arrays
    let (label_out_offsets, label_out_flat) = flatten_labels(&label_out_data, scc_count);
    let (label_in_offsets, label_in_flat) = flatten_labels(&label_in_data, scc_count);

    // Budget already checked incrementally during computation (prevents OOM)
    debug_assert!(label_out_flat.len() + label_in_flat.len() <= budget);

    Ok((
        label_out_offsets,
        label_out_flat,
        label_in_offsets,
        label_in_flat,
    ))
}

/// Create position-based intervals for each SCC from topological ordering.
#[allow(clippy::cast_possible_truncation)]
fn compute_base_intervals(dag: &CondensationDag, scc_count: usize) -> Vec<Interval> {
    let mut base_intervals = vec![Interval::new(0, 0); scc_count];
    for (topo_idx, &scc_id) in dag.topo_order.iter().enumerate() {
        base_intervals[scc_id as usize] = Interval::new(topo_idx as u32, (topo_idx + 1) as u32);
    }
    base_intervals
}

/// Build reverse adjacency (predecessor lists) for label_in computation.
#[allow(clippy::cast_possible_truncation)]
fn build_predecessors(dag: &CondensationDag, scc_count: usize) -> Vec<Vec<u32>> {
    let mut predecessors: Vec<Vec<u32>> = vec![Vec::new(); scc_count];
    for scc in 0..scc_count {
        for &successor in dag.successors(scc as u32) {
            predecessors[successor as usize].push(scc as u32);
        }
    }
    predecessors
}

/// Compute `label_out` in reverse topological order.
///
/// Returns the label data and the total interval count consumed.
///
/// # Errors
///
/// Returns an error if the budget is exceeded.
fn compute_label_out(
    dag: &CondensationDag,
    scc_count: usize,
    base_intervals: &[Interval],
    budget: usize,
) -> Result<(Vec<Vec<Interval>>, usize)> {
    let mut label_out_data: Vec<Vec<Interval>> = vec![Vec::new(); scc_count];
    let mut total_intervals = 0usize;

    for &scc_id in dag.topo_order.iter().rev() {
        let scc = scc_id as usize;
        let mut intervals = vec![base_intervals[scc]];

        for &successor in dag.successors(scc_id) {
            intervals.extend_from_slice(&label_out_data[successor as usize]);
        }

        label_out_data[scc] = merge_intervals(intervals);

        total_intervals += label_out_data[scc].len();
        if total_intervals > budget {
            anyhow::bail!(
                "2-hop label budget exceeded during label_out computation: {total_intervals} intervals > {budget} budget"
            );
        }
    }

    Ok((label_out_data, total_intervals))
}

/// Compute `label_in` in forward topological order.
///
/// # Errors
///
/// Returns an error if the budget is exceeded.
fn compute_label_in(
    dag: &CondensationDag,
    scc_count: usize,
    base_intervals: &[Interval],
    predecessors: &[Vec<u32>],
    budget: usize,
    initial_total: usize,
) -> Result<Vec<Vec<Interval>>> {
    let mut label_in_data: Vec<Vec<Interval>> = vec![Vec::new(); scc_count];
    let mut total_intervals = initial_total;

    for &scc_id in &dag.topo_order {
        let scc = scc_id as usize;
        let mut intervals = vec![base_intervals[scc]];

        for &predecessor in &predecessors[scc] {
            intervals.extend_from_slice(&label_in_data[predecessor as usize]);
        }

        label_in_data[scc] = merge_intervals(intervals);

        total_intervals += label_in_data[scc].len();
        if total_intervals > budget {
            anyhow::bail!(
                "2-hop label budget exceeded during label_in computation: {total_intervals} intervals > {budget} budget"
            );
        }
    }

    Ok(label_in_data)
}

/// Flatten per-SCC label data into CSR-style offset/data arrays.
#[allow(clippy::cast_possible_truncation)]
fn flatten_labels(label_data: &[Vec<Interval>], scc_count: usize) -> (Vec<u32>, Vec<Interval>) {
    let mut offsets = Vec::with_capacity(scc_count + 1);
    let mut flat = Vec::new();
    offsets.push(0);

    for labels in label_data.iter().take(scc_count) {
        flat.extend_from_slice(labels);
        offsets.push(flat.len() as u32);
    }

    (offsets, flat)
}

/// Merge overlapping intervals and sort
fn merge_intervals(mut intervals: Vec<Interval>) -> Vec<Interval> {
    if intervals.is_empty() {
        return intervals;
    }

    // Sort by start position
    intervals.sort_unstable_by_key(|i| i.start);

    let mut merged = Vec::new();
    let mut current = intervals[0];

    for &interval in &intervals[1..] {
        if interval.start <= current.end {
            // Overlapping or adjacent - merge
            current.end = current.end.max(interval.end);
        } else {
            // Non-overlapping - push current and start new
            merged.push(current);
            current = interval;
        }
    }

    merged.push(current);
    merged
}
