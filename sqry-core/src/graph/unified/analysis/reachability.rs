//! 2-hop interval labeling with wavefront parallelism
//!
//! Computes reachability labels for the condensation DAG using a level-based
//! wavefront strategy: SCCs at the same topological depth are independent and
//! processed in parallel via rayon. Sequential processing (with a reused bitset)
//! is used for small levels where parallelism overhead exceeds the benefit.

use super::condensation::{CondensationDag, Interval};
use anyhow::Result;
use rayon::prelude::*;

/// Label data for 2-hop interval labeling
type LabelData = (Vec<u32>, Vec<Interval>, Vec<u32>, Vec<Interval>);

/// Minimum SCCs per level to trigger parallel processing.
/// Below this threshold, sequential processing with a reused bitset is faster.
const PARALLEL_LEVEL_THRESHOLD: usize = 512;

/// Compute 2-hop interval labels for a condensation DAG
///
/// Returns (`label_out_offsets`, `label_out_data`, `label_in_offsets`, `label_in_data`)
///
/// Uses wavefront parallelism: SCCs at the same topological depth are processed
/// concurrently. For label_out, depth is measured from sinks (leaves); for
/// label_in, depth is measured from sources (roots).
///
/// # Errors
///
/// Returns an error if the label budget is exceeded.
#[allow(clippy::cast_possible_truncation)] // Graph sizes realistically won't exceed u32::MAX
pub fn compute_2hop_labels(dag: &CondensationDag, budget: usize) -> Result<LabelData> {
    let scc_count = dag.scc_count as usize;

    // Step 1: Create position-based intervals for each SCC
    let base_intervals = compute_base_intervals(dag, scc_count);

    // Step 2: Build reverse adjacency for label_in computation
    let predecessors = build_predecessors(dag, scc_count);

    // Step 3: Compute label_out using wavefront parallelism (reverse topo order)
    let (label_out_data, total_out) =
        compute_label_out_wavefront(dag, scc_count, &base_intervals, budget)?;

    // Step 4: Compute label_in using wavefront parallelism (forward topo order)
    let label_in_data = compute_label_in_wavefront(
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

/// Compute sink-rooted levels for wavefront label_out parallelism.
///
/// Level 0 = sinks (no successors). Level L = 1 + max(successor levels).
/// SCCs at the same level are independent for label_out computation.
fn compute_sink_levels(dag: &CondensationDag, scc_count: usize) -> (Vec<usize>, usize) {
    let mut levels = vec![0usize; scc_count];
    // Process in reverse topo order: sinks first, then backward
    for &scc_id in dag.topo_order.iter().rev() {
        let scc = scc_id as usize;
        let max_succ = dag
            .successors(scc_id)
            .iter()
            .map(|&s| levels[s as usize])
            .max();
        if let Some(max) = max_succ {
            levels[scc] = max + 1;
        }
    }
    let max_level = levels.iter().copied().max().unwrap_or(0);
    (levels, max_level)
}

/// Compute source-rooted levels for wavefront label_in parallelism.
///
/// Level 0 = sources (no predecessors). Level L = 1 + max(predecessor levels).
/// SCCs at the same level are independent for label_in computation.
fn compute_source_levels(
    scc_count: usize,
    predecessors: &[Vec<u32>],
    topo_order: &[u32],
) -> (Vec<usize>, usize) {
    let mut levels = vec![0usize; scc_count];
    for &scc_id in topo_order {
        let scc = scc_id as usize;
        let max_pred = predecessors[scc].iter().map(|&p| levels[p as usize]).max();
        if let Some(max) = max_pred {
            levels[scc] = max + 1;
        }
    }
    let max_level = levels.iter().copied().max().unwrap_or(0);
    (levels, max_level)
}

/// Group SCCs by level. Returns a vector where `groups[level]` contains all SCC IDs at that level.
///
/// Returns `None` if the DAG is too deep for wavefront to be beneficial (max_level > scc_count / 4).
/// Deep/narrow DAGs have few SCCs per level, making parallelism overhead exceed the benefit,
/// and allocating `max_level + 1` groups would waste memory.
fn group_by_level(scc_count: usize, levels: &[usize], max_level: usize) -> Option<Vec<Vec<u32>>> {
    // Guard: deep DAGs have ~1 SCC per level → no parallelism benefit, and O(max_level)
    // Vec<Vec<u32>> allocation would waste memory (hundreds of MB at 11M levels).
    if max_level > scc_count / 4 {
        return None;
    }
    let mut groups: Vec<Vec<u32>> = vec![Vec::new(); max_level + 1];
    for scc in 0..scc_count {
        groups[levels[scc]].push(scc as u32);
    }
    Some(groups)
}

/// Merge a collection of possibly-overlapping intervals into a minimal sorted set.
///
/// Intervals must be non-empty. Adjacent intervals (end == start) are merged.
fn merge_intervals(mut intervals: Vec<Interval>) -> Vec<Interval> {
    if intervals.len() <= 1 {
        return intervals;
    }
    intervals.sort_unstable_by_key(|i| i.start);
    let mut merged = Vec::with_capacity(intervals.len());
    let mut current = intervals[0];
    for &next in &intervals[1..] {
        if next.start <= current.end {
            current.end = current.end.max(next.end);
        } else {
            merged.push(current);
            current = next;
        }
    }
    merged.push(current);
    merged
}

/// Process a slice of SCCs sequentially using a reused bitset.
///
/// # Errors
///
/// Returns an error if the budget is exceeded.
fn process_sequential_out(
    sccs: &[u32],
    dag: &CondensationDag,
    base_intervals: &[Interval],
    label_out_data: &mut [Vec<Interval>],
    bitset: &mut FastBitSet,
    total_intervals: &mut usize,
    budget: usize,
) -> Result<()> {
    for &scc_id in sccs {
        let scc = scc_id as usize;

        bitset.clear();
        bitset.set_range(base_intervals[scc].start, base_intervals[scc].end);

        for &successor in dag.successors(scc_id) {
            for interval in &label_out_data[successor as usize] {
                bitset.set_range(interval.start, interval.end);
            }
        }

        label_out_data[scc] = bitset.extract_intervals();

        *total_intervals += label_out_data[scc].len();
        if *total_intervals > budget {
            anyhow::bail!(
                "2-hop label budget exceeded during label_out computation: \
                 {} intervals > {budget} budget",
                *total_intervals
            );
        }
    }
    Ok(())
}

/// Estimate the upper bound of intervals a parallel level will produce.
///
/// Each SCC contributes at most 1 (base) + sum(successor label sizes) intervals
/// before merging. After merging, the count can only decrease. This gives a safe
/// upper bound for pre-budget checking.
fn estimate_level_intervals_out(
    sccs: &[u32],
    dag: &CondensationDag,
    label_out_data: &[Vec<Interval>],
) -> usize {
    sccs.iter()
        .map(|&scc_id| {
            1 + dag
                .successors(scc_id)
                .iter()
                .map(|&s| label_out_data[s as usize].len())
                .sum::<usize>()
        })
        .sum()
}

/// Compute `label_out` using wavefront parallelism.
///
/// Processes levels from 0 (sinks) upward. Within each level, SCCs are
/// independent and processed in parallel when the level is large enough.
/// Falls back to fully sequential processing when the DAG is too deep
/// for wavefront to be beneficial, or when the budget would be exceeded.
///
/// # Errors
///
/// Returns an error if the budget is exceeded.
fn compute_label_out_wavefront(
    dag: &CondensationDag,
    scc_count: usize,
    base_intervals: &[Interval],
    budget: usize,
) -> Result<(Vec<Vec<Interval>>, usize)> {
    let mut label_out_data: Vec<Vec<Interval>> = vec![Vec::new(); scc_count];
    let mut total_intervals = 0usize;
    let mut bitset = FastBitSet::new(scc_count);

    let (levels, max_level) = compute_sink_levels(dag, scc_count);
    let level_groups = group_by_level(scc_count, &levels, max_level);

    let Some(level_groups) = level_groups else {
        // Deep/narrow DAG: wavefront has no benefit. Process sequentially in
        // reverse topological order (same as the original algorithm).
        for &scc_id in dag.topo_order.iter().rev() {
            process_sequential_out(
                &[scc_id],
                dag,
                base_intervals,
                &mut label_out_data,
                &mut bitset,
                &mut total_intervals,
                budget,
            )?;
        }
        return Ok((label_out_data, total_intervals));
    };

    for sccs in &level_groups {
        // Budget pre-check: estimate upper bound of intervals this level will produce.
        // If it would exceed the remaining budget, fall back to sequential processing
        // which checks incrementally per SCC and bails early.
        let remaining_budget = budget.saturating_sub(total_intervals);
        let use_parallel = sccs.len() >= PARALLEL_LEVEL_THRESHOLD
            && estimate_level_intervals_out(sccs, dag, &label_out_data) <= remaining_budget;

        if use_parallel {
            let results: Vec<(usize, Vec<Interval>)> = sccs
                .par_iter()
                .map(|&scc_id| {
                    let scc = scc_id as usize;
                    let successors = dag.successors(scc_id);

                    if successors.is_empty() {
                        return (scc, vec![base_intervals[scc]]);
                    }

                    let capacity = 1 + successors
                        .iter()
                        .map(|&s| label_out_data[s as usize].len())
                        .sum::<usize>();
                    let mut all = Vec::with_capacity(capacity);
                    all.push(base_intervals[scc]);
                    for &successor in successors {
                        all.extend_from_slice(&label_out_data[successor as usize]);
                    }
                    (scc, merge_intervals(all))
                })
                .collect();

            for (idx, intervals) in results {
                total_intervals += intervals.len();
                label_out_data[idx] = intervals;
            }
            // Post-check: merging reduces counts, so actual total may be under budget
            // even though the estimate was within bounds. But if the actual total exceeds,
            // bail now before processing the next level.
            if total_intervals > budget {
                anyhow::bail!(
                    "2-hop label budget exceeded during label_out computation: \
                     {total_intervals} intervals > {budget} budget"
                );
            }
        } else {
            process_sequential_out(
                sccs,
                dag,
                base_intervals,
                &mut label_out_data,
                &mut bitset,
                &mut total_intervals,
                budget,
            )?;
        }
    }

    Ok((label_out_data, total_intervals))
}

/// Process a slice of SCCs sequentially for label_in using a reused bitset.
///
/// # Errors
///
/// Returns an error if the budget is exceeded.
fn process_sequential_in(
    sccs: &[u32],
    base_intervals: &[Interval],
    predecessors: &[Vec<u32>],
    label_in_data: &mut [Vec<Interval>],
    bitset: &mut FastBitSet,
    total_intervals: &mut usize,
    budget: usize,
) -> Result<()> {
    for &scc_id in sccs {
        let scc = scc_id as usize;

        bitset.clear();
        bitset.set_range(base_intervals[scc].start, base_intervals[scc].end);

        for &predecessor in &predecessors[scc] {
            for interval in &label_in_data[predecessor as usize] {
                bitset.set_range(interval.start, interval.end);
            }
        }

        label_in_data[scc] = bitset.extract_intervals();

        *total_intervals += label_in_data[scc].len();
        if *total_intervals > budget {
            anyhow::bail!(
                "2-hop label budget exceeded during label_in computation: \
                 {} intervals > {budget} budget",
                *total_intervals
            );
        }
    }
    Ok(())
}

/// Estimate the upper bound of intervals a parallel label_in level will produce.
fn estimate_level_intervals_in(
    sccs: &[u32],
    predecessors: &[Vec<u32>],
    label_in_data: &[Vec<Interval>],
) -> usize {
    sccs.iter()
        .map(|&scc_id| {
            let scc = scc_id as usize;
            1 + predecessors[scc]
                .iter()
                .map(|&p| label_in_data[p as usize].len())
                .sum::<usize>()
        })
        .sum()
}

/// Compute `label_in` using wavefront parallelism.
///
/// Processes levels from 0 (sources) upward. Within each level, SCCs are
/// independent and processed in parallel when the level is large enough.
/// Falls back to fully sequential processing when the DAG is too deep
/// for wavefront to be beneficial, or when the budget would be exceeded.
///
/// # Errors
///
/// Returns an error if the budget is exceeded.
fn compute_label_in_wavefront(
    dag: &CondensationDag,
    scc_count: usize,
    base_intervals: &[Interval],
    predecessors: &[Vec<u32>],
    budget: usize,
    initial_total: usize,
) -> Result<Vec<Vec<Interval>>> {
    let mut label_in_data: Vec<Vec<Interval>> = vec![Vec::new(); scc_count];
    let mut total_intervals = initial_total;
    let mut bitset = FastBitSet::new(scc_count);

    let (levels, max_level) = compute_source_levels(scc_count, predecessors, &dag.topo_order);
    let level_groups = group_by_level(scc_count, &levels, max_level);

    let Some(level_groups) = level_groups else {
        // Deep/narrow DAG: process sequentially in forward topological order.
        for &scc_id in &dag.topo_order {
            process_sequential_in(
                &[scc_id],
                base_intervals,
                predecessors,
                &mut label_in_data,
                &mut bitset,
                &mut total_intervals,
                budget,
            )?;
        }
        return Ok(label_in_data);
    };

    for sccs in &level_groups {
        let remaining_budget = budget.saturating_sub(total_intervals);
        let use_parallel = sccs.len() >= PARALLEL_LEVEL_THRESHOLD
            && estimate_level_intervals_in(sccs, predecessors, &label_in_data) <= remaining_budget;

        if use_parallel {
            let results: Vec<(usize, Vec<Interval>)> = sccs
                .par_iter()
                .map(|&scc_id| {
                    let scc = scc_id as usize;
                    let preds = &predecessors[scc];

                    if preds.is_empty() {
                        return (scc, vec![base_intervals[scc]]);
                    }

                    let capacity = 1 + preds
                        .iter()
                        .map(|&p| label_in_data[p as usize].len())
                        .sum::<usize>();
                    let mut all = Vec::with_capacity(capacity);
                    all.push(base_intervals[scc]);
                    for &predecessor in preds {
                        all.extend_from_slice(&label_in_data[predecessor as usize]);
                    }
                    (scc, merge_intervals(all))
                })
                .collect();

            for (idx, intervals) in results {
                total_intervals += intervals.len();
                label_in_data[idx] = intervals;
            }
            if total_intervals > budget {
                anyhow::bail!(
                    "2-hop label budget exceeded during label_in computation: \
                     {total_intervals} intervals > {budget} budget"
                );
            }
        } else {
            process_sequential_in(
                sccs,
                base_intervals,
                predecessors,
                &mut label_in_data,
                &mut bitset,
                &mut total_intervals,
                budget,
            )?;
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

/// A specialized bitset for efficient interval merging.
///
/// Optimized for dense graphs and batch interval operations.
/// Tracks dirty word range for O(dirty) clear instead of O(N).
struct FastBitSet {
    words: Vec<u64>,
    min_word: usize,
    max_word: usize,
}

impl FastBitSet {
    fn new(size: usize) -> Self {
        let num_words = size.div_ceil(64);
        Self {
            words: vec![0; num_words],
            min_word: num_words,
            max_word: 0,
        }
    }

    fn clear(&mut self) {
        if self.min_word <= self.max_word {
            for i in self.min_word..=self.max_word {
                self.words[i] = 0;
            }
        }
        self.min_word = self.words.len();
        self.max_word = 0;
    }

    fn set_range(&mut self, start: u32, end: u32) {
        if start >= end {
            return;
        }
        let start = start as usize;
        let end = end as usize;

        let start_word = start / 64;
        let end_word = (end - 1) / 64;

        self.min_word = self.min_word.min(start_word);
        self.max_word = self.max_word.max(end_word);

        if start_word == end_word {
            let mask = ((!0u64) << (start % 64)) & ((!0u64) >> (63 - ((end - 1) % 64)));
            self.words[start_word] |= mask;
        } else {
            let start_mask = (!0u64) << (start % 64);
            self.words[start_word] |= start_mask;

            for i in (start_word + 1)..end_word {
                self.words[i] = !0u64;
            }

            let end_mask = (!0u64) >> (63 - ((end - 1) % 64));
            self.words[end_word] |= end_mask;
        }
    }

    fn extract_intervals(&self) -> Vec<Interval> {
        let mut intervals = Vec::new();
        if self.min_word > self.max_word {
            return intervals;
        }

        let mut in_interval = false;
        let mut current_start = 0u32;

        for i in self.min_word..=self.max_word {
            let word = self.words[i];
            let word_start = (i * 64) as u32;

            if word == 0 {
                if in_interval {
                    intervals.push(Interval::new(current_start, word_start));
                    in_interval = false;
                }
                continue;
            }

            if word == !0u64 {
                if !in_interval {
                    current_start = word_start;
                    in_interval = true;
                }
                continue;
            }

            // Mixed word - use trailing_zeros for efficiency
            let mut b = 0;
            while b < 64 {
                if !in_interval {
                    let zeros = (word >> b).trailing_zeros();
                    b += zeros;
                    if b < 64 {
                        current_start = word_start + b;
                        in_interval = true;
                    }
                } else {
                    let ones = (!(word >> b)).trailing_zeros();
                    b += ones;
                    if b < 64 {
                        intervals.push(Interval::new(current_start, word_start + b));
                        in_interval = false;
                    }
                }
            }
        }

        if in_interval {
            // Find the actual end bit in the last modified word
            let last_word = self.words[self.max_word];
            let leading_zeros = last_word.leading_zeros();
            let end = (self.max_word as u32 + 1) * 64 - leading_zeros;
            intervals.push(Interval::new(current_start, end));
        }

        intervals
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fast_bitset_basic() {
        let mut bitset = FastBitSet::new(128);
        bitset.set_range(1, 4);
        bitset.set_range(10, 12);
        bitset.set_range(63, 67);

        let intervals = bitset.extract_intervals();
        assert_eq!(intervals.len(), 3);
        assert_eq!(intervals[0], Interval::new(1, 4));
        assert_eq!(intervals[1], Interval::new(10, 12));
        assert_eq!(intervals[2], Interval::new(63, 67));
    }

    #[test]
    fn test_fast_bitset_overlap() {
        let mut bitset = FastBitSet::new(128);
        bitset.set_range(1, 10);
        bitset.set_range(5, 15);

        let intervals = bitset.extract_intervals();
        assert_eq!(intervals.len(), 1);
        assert_eq!(intervals[0], Interval::new(1, 15));
    }

    #[test]
    fn test_fast_bitset_full_word() {
        let mut bitset = FastBitSet::new(128);
        bitset.set_range(0, 64);
        bitset.set_range(64, 128);

        let intervals = bitset.extract_intervals();
        assert_eq!(intervals.len(), 1);
        assert_eq!(intervals[0], Interval::new(0, 128));
    }

    #[test]
    fn test_fast_bitset_mixed_word() {
        let mut bitset = FastBitSet::new(64);
        // Set alternate bits: 1, 3, 5...
        for i in (1..64).step_by(2) {
            bitset.set_range(i, i + 1);
        }

        let intervals = bitset.extract_intervals();
        assert_eq!(intervals.len(), 32);
        for (i, interval) in intervals.iter().enumerate().take(32) {
            assert_eq!(
                *interval,
                Interval::new((i as u32 * 2) + 1, (i as u32 * 2) + 2)
            );
        }
    }

    #[test]
    fn test_fast_bitset_edge_of_capacity() {
        // Set the very last bit in the bitset
        let mut bitset = FastBitSet::new(128);
        bitset.set_range(127, 128);
        let intervals = bitset.extract_intervals();
        assert_eq!(intervals.len(), 1);
        assert_eq!(intervals[0], Interval::new(127, 128));

        // Interval spanning from previous word into the last bit
        let mut bitset = FastBitSet::new(128);
        bitset.set_range(60, 128);
        let intervals = bitset.extract_intervals();
        assert_eq!(intervals.len(), 1);
        assert_eq!(intervals[0], Interval::new(60, 128));

        // Full capacity
        let mut bitset = FastBitSet::new(128);
        bitset.set_range(0, 128);
        let intervals = bitset.extract_intervals();
        assert_eq!(intervals.len(), 1);
        assert_eq!(intervals[0], Interval::new(0, 128));
    }

    #[test]
    fn test_fast_bitset_clear_reuse() {
        let mut bitset = FastBitSet::new(128);
        bitset.set_range(0, 64);
        let intervals = bitset.extract_intervals();
        assert_eq!(intervals[0], Interval::new(0, 64));

        bitset.clear();
        bitset.set_range(100, 110);
        let intervals = bitset.extract_intervals();
        assert_eq!(intervals.len(), 1);
        assert_eq!(intervals[0], Interval::new(100, 110));
    }

    #[test]
    fn test_merge_intervals_basic() {
        let intervals = vec![
            Interval::new(1, 3),
            Interval::new(5, 8),
            Interval::new(2, 6),
        ];
        let merged = merge_intervals(intervals);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0], Interval::new(1, 8));
    }

    #[test]
    fn test_merge_intervals_adjacent() {
        let intervals = vec![Interval::new(1, 3), Interval::new(3, 5)];
        let merged = merge_intervals(intervals);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0], Interval::new(1, 5));
    }

    #[test]
    fn test_merge_intervals_disjoint() {
        let intervals = vec![Interval::new(1, 3), Interval::new(5, 7)];
        let merged = merge_intervals(intervals);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0], Interval::new(1, 3));
        assert_eq!(merged[1], Interval::new(5, 7));
    }

    #[test]
    fn test_merge_intervals_single() {
        let intervals = vec![Interval::new(1, 5)];
        let merged = merge_intervals(intervals);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0], Interval::new(1, 5));
    }

    #[test]
    fn test_merge_intervals_empty() {
        let intervals: Vec<Interval> = Vec::new();
        let merged = merge_intervals(intervals);
        assert!(merged.is_empty());
    }

    // --- Wavefront parallelism tests ---

    use super::super::condensation::CondensationDag;
    use super::super::csr::CsrAdjacency;
    use super::super::scc::SccData;
    use crate::graph::unified::compaction::{CompactionSnapshot, MergedEdge};
    use crate::graph::unified::edge::EdgeKind;
    use crate::graph::unified::file::FileId;
    use crate::graph::unified::node::NodeId;

    /// Build a wide fan-out DAG: node 0 → nodes 1..=n (n sinks at level 0).
    /// This creates a single level with n SCCs, forcing the parallel path when n >= 512.
    fn wide_dag_snapshot(n: usize) -> CompactionSnapshot {
        let file = FileId::new(0);
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let edges: Vec<MergedEdge> = (1..=n)
            .enumerate()
            .map(|(i, target)| {
                MergedEdge::new(
                    NodeId::new(0, 0),
                    NodeId::new(target as u32, 0),
                    kind.clone(),
                    (i + 1) as u64,
                    file,
                )
            })
            .collect();
        CompactionSnapshot {
            csr_edges: edges,
            delta_edges: Vec::new(),
            node_count: n + 1,
            csr_version: 0,
        }
    }

    #[test]
    fn test_wavefront_parallel_path_wide_dag() {
        // 600 sinks → level 0 has 600 SCCs (> PARALLEL_LEVEL_THRESHOLD = 512)
        let snapshot = wide_dag_snapshot(600);
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let dag = CondensationDag::build(&scc, &csr).unwrap();

        assert_eq!(
            dag.strategy,
            super::super::condensation::ReachabilityStrategy::IntervalLabels
        );
        assert!(!dag.label_out_data.is_empty());

        // Verify reachability: node 0 can reach all sinks
        let scc_0 = scc.scc_of(NodeId::new(0, 0)).unwrap();
        for i in 1..=600u32 {
            let scc_i = scc.scc_of(NodeId::new(i, 0)).unwrap();
            assert!(dag.can_reach(scc_0, scc_i), "0 should reach {i}");
            assert!(!dag.can_reach(scc_i, scc_0), "{i} should not reach 0");
        }
    }

    #[test]
    fn test_wavefront_equivalence_with_sequential() {
        // Build the same DAG with labels via default (wavefront) and compare
        // reachability answers against BFS-only (budget=1, degrade policy).
        let snapshot = wide_dag_snapshot(600);
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();

        let dag_labels = CondensationDag::build(&scc, &csr).unwrap();
        assert_eq!(
            dag_labels.strategy,
            super::super::condensation::ReachabilityStrategy::IntervalLabels
        );

        let config_bfs = super::super::condensation::LabelBudgetConfig {
            budget_per_kind: 1,
            on_exceeded: super::super::condensation::BudgetExceededPolicy::Degrade,
            ..Default::default()
        };
        let dag_bfs = CondensationDag::build_with_budget(&scc, &csr, &config_bfs).unwrap();
        assert_eq!(
            dag_bfs.strategy,
            super::super::condensation::ReachabilityStrategy::DagBfs
        );

        // All pairs must agree
        for from in 0..dag_labels.scc_count {
            for to in 0..dag_labels.scc_count {
                assert_eq!(
                    dag_labels.can_reach(from, to),
                    dag_bfs.can_reach(from, to),
                    "Mismatch for can_reach({from}, {to})"
                );
            }
        }
    }

    #[test]
    fn test_wavefront_budget_exceeded_parallel_path() {
        // Budget of 100 with 600-wide fan-out: budget will be exceeded.
        // With Fail policy, should return error. With Degrade, should fall back.
        let snapshot = wide_dag_snapshot(600);
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();

        let config_fail = super::super::condensation::LabelBudgetConfig {
            budget_per_kind: 100,
            on_exceeded: super::super::condensation::BudgetExceededPolicy::Fail,
            density_gate_threshold: 0,
            skip_labels: false,
        };
        let result = CondensationDag::build_with_budget(&scc, &csr, &config_fail);
        assert!(result.is_err(), "Should fail with tight budget");

        let config_degrade = super::super::condensation::LabelBudgetConfig {
            budget_per_kind: 100,
            on_exceeded: super::super::condensation::BudgetExceededPolicy::Degrade,
            density_gate_threshold: 0,
            skip_labels: false,
        };
        let dag = CondensationDag::build_with_budget(&scc, &csr, &config_degrade).unwrap();
        assert_eq!(
            dag.strategy,
            super::super::condensation::ReachabilityStrategy::DagBfs
        );
        // BFS fallback should still work
        let scc_0 = scc.scc_of(NodeId::new(0, 0)).unwrap();
        let scc_1 = scc.scc_of(NodeId::new(1, 0)).unwrap();
        assert!(dag.can_reach(scc_0, scc_1));
    }

    #[test]
    fn test_skip_labels_produces_bfs_strategy() {
        let snapshot = wide_dag_snapshot(10);
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();

        let config = super::super::condensation::LabelBudgetConfig {
            skip_labels: true,
            ..Default::default()
        };
        let dag = CondensationDag::build_with_budget(&scc, &csr, &config).unwrap();
        assert_eq!(
            dag.strategy,
            super::super::condensation::ReachabilityStrategy::DagBfs
        );
        assert!(dag.label_out_data.is_empty());
        assert!(dag.label_in_data.is_empty());

        // BFS reachability still works
        let scc_0 = scc.scc_of(NodeId::new(0, 0)).unwrap();
        let scc_5 = scc.scc_of(NodeId::new(5, 0)).unwrap();
        assert!(dag.can_reach(scc_0, scc_5));
        assert!(!dag.can_reach(scc_5, scc_0));
    }

    /// Build a deep chain DAG: 0 → 1 → 2 → ... → n (max_level = n, scc_count = n+1).
    /// When n > scc_count/4 (always true for chains), group_by_level returns None
    /// and the wavefront falls back to fully sequential processing.
    fn deep_chain_snapshot(n: usize) -> CompactionSnapshot {
        let file = FileId::new(0);
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let edges: Vec<MergedEdge> = (0..n)
            .map(|i| {
                MergedEdge::new(
                    NodeId::new(i as u32, 0),
                    NodeId::new((i + 1) as u32, 0),
                    kind.clone(),
                    (i + 1) as u64,
                    file,
                )
            })
            .collect();
        CompactionSnapshot {
            csr_edges: edges,
            delta_edges: Vec::new(),
            node_count: n + 1,
            csr_version: 0,
        }
    }

    #[test]
    fn test_deep_dag_falls_back_to_sequential() {
        // Chain of 100 nodes: max_level = 99, scc_count = 100.
        // 99 > 100/4 = 25, so group_by_level returns None → sequential fallback.
        let snapshot = deep_chain_snapshot(99);
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let dag = CondensationDag::build(&scc, &csr).unwrap();

        // Verify group_by_level would return None for this shape
        let (levels, max_level) = compute_sink_levels(&dag, dag.scc_count as usize);
        assert!(max_level > dag.scc_count as usize / 4);
        assert!(group_by_level(dag.scc_count as usize, &levels, max_level).is_none());

        // Labels should still be computed correctly via sequential fallback
        assert_eq!(
            dag.strategy,
            super::super::condensation::ReachabilityStrategy::IntervalLabels
        );
        assert!(!dag.label_out_data.is_empty());

        // Verify reachability: 0 can reach all, tail can't reach head
        let scc_0 = scc.scc_of(NodeId::new(0, 0)).unwrap();
        let scc_50 = scc.scc_of(NodeId::new(50, 0)).unwrap();
        let scc_99 = scc.scc_of(NodeId::new(99, 0)).unwrap();
        assert!(dag.can_reach(scc_0, scc_50));
        assert!(dag.can_reach(scc_0, scc_99));
        assert!(dag.can_reach(scc_50, scc_99));
        assert!(!dag.can_reach(scc_99, scc_0));
        assert!(!dag.can_reach(scc_50, scc_0));
    }
}
