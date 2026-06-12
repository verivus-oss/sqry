//! Admission accounting state + working-set estimate helpers.
//!
//! Corresponds to Task 6 Steps 4 / 4a / 4b / 4d of the sqryd plan
//! (Amendment 2 §G.1, §G.2, §G.5, §G.6, §G.7).
//!
//! The authoritative accounting invariant (§G.5):
//!
//! > At any instant, `loaded_bytes + reserved_bytes + sum(retained_old bytes)`
//! > equals the sum of every `CodeGraph`-worth of memory the daemon is
//! > currently responsible for, including graphs published to a
//! > workspace, graphs being constructed, and old graphs whose `Arc` has
//! > not yet been uniquely held by the admission map.
//!
//! [`AdmissionState`] is the single source of truth for that invariant.
//! The only holder is [`crate::workspace::WorkspaceManager::admission`],
//! wrapped in a `parking_lot::Mutex` and acquired:
//!
//! - by [`crate::workspace::WorkspaceManager::reserve_rebuild`] in three
//!   disjoint phases per §G.1,
//! - by [`crate::workspace::publish::publish_and_retain`] to move bytes
//!   from `reserved` into `loaded` + insert a [`RetainedEntry`] on the
//!   old graph (§G.2),
//! - by the retention reaper task to drop entries whose `Arc::strong_count`
//!   shows the admission map is the last holder (§G.3).
//!
//! Queries and reads never acquire this mutex — it is not on the hot
//! path.

use std::{sync::Arc, time::Instant};

use std::collections::HashMap;

use sqry_core::graph::CodeGraph;

use crate::config::{INTERNER_BUILDER_OVERHEAD_RATIO, WORKING_SET_MULTIPLIER};

use super::state::OldGraphToken;

fn rounded_f64_to_u64(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    format!("{:.0}", value.ceil())
        .parse::<u64>()
        .unwrap_or(u64::MAX)
}

// ---------------------------------------------------------------------------
// RetainedEntry
// ---------------------------------------------------------------------------

/// A published-but-not-yet-dropped old graph held by the admission map.
///
/// Per Amendment 2 §G.2 / §G.4, the admission map is the **sole owner** of
/// the retained `Arc<CodeGraph>`. Slow queries hold additional strong
/// references; the entry's `Arc::strong_count` equals
/// `1 + (outstanding query holders)`. The retention reaper
/// (see [`crate::workspace::WorkspaceManager::spawn_retention_reaper`])
/// is the only code path that removes entries from
/// [`AdmissionState::retained_old`]; it does so when `strong_count` drops
/// to `1`, at which point dropping the entry also drops the `Arc`.
///
/// `warned_past_timeout` serialises the one-shot WARN log line the reaper
/// emits if a retained entry sits past `rebuild_drain_timeout_ms`
/// (Amendment 2 §G.4: that timeout is a logging threshold, **not** an
/// accounting deadline — retained bytes are released only when
/// `strong_count` drops).
#[derive(Debug)]
pub struct RetainedEntry {
    /// The `heap_bytes` estimate of the retained old graph at publish
    /// time. Persisted here so the reaper does not need to recompute it
    /// on the free path.
    pub bytes: u64,

    /// The old graph itself. The admission map owns this strong
    /// reference; only the reaper drops it.
    pub graph: Arc<CodeGraph>,

    /// Wall-clock timestamp at `publish_and_retain` entry. Used by the
    /// reaper to compare against `rebuild_drain_timeout_ms`.
    pub published_at: Instant,

    /// `true` after the reaper has logged the drain-timeout warning
    /// once for this entry; suppresses duplicate log spam on subsequent
    /// reaper ticks while the entry continues to sit retained.
    pub warned_past_timeout: bool,
}

// ---------------------------------------------------------------------------
// AdmissionState
// ---------------------------------------------------------------------------

/// Single mutex-guarded state block for global admission accounting.
///
/// See module docs for the §G.5 invariant. Every mutating operation is
/// exposed as an associated function so call sites can document why each
/// field is being touched; no direct field access outside this module.
#[derive(Debug, Default)]
pub struct AdmissionState {
    /// Sum of `memory_bytes` across every workspace currently in the
    /// [`crate::workspace::WorkspaceState::Loaded`] or
    /// [`crate::workspace::WorkspaceState::Rebuilding`] state.
    pub loaded_bytes: u64,

    /// Sum of per-rebuild working-set reservations currently in flight
    /// (via a live [`RebuildReservation`]). Returned to zero when every
    /// reservation's guard drops.
    ///
    /// [`RebuildReservation`]: super::manager::RebuildReservation
    pub reserved_bytes: u64,

    /// Published-but-not-yet-dropped old graphs. See [`RetainedEntry`].
    ///
    /// Ownership: the admission map holds the strong ref. Removal is
    /// delegated exclusively to the retention reaper.
    pub retained_old: HashMap<OldGraphToken, RetainedEntry>,
}

impl AdmissionState {
    /// Sum of the `bytes` across every [`RetainedEntry`] — equivalent to
    /// `sum(retained_old.values().bytes)`. Used as the third summand in
    /// the §G.5 invariant.
    #[must_use]
    pub fn retained_total_bytes(&self) -> u64 {
        self.retained_old.values().map(|e| e.bytes).sum()
    }

    /// `loaded_bytes + reserved_bytes + retained_total_bytes()` — the
    /// left-hand side of the §G.5 invariant. Reads the admission
    /// accounting atomically with respect to the mutex that must be
    /// held when calling this.
    #[must_use]
    pub fn total_committed_bytes(&self) -> u64 {
        self.loaded_bytes
            .saturating_add(self.reserved_bytes)
            .saturating_add(self.retained_total_bytes())
    }
}

// ---------------------------------------------------------------------------
// Working-set estimate helpers (Amendment 2 §G.6)
// ---------------------------------------------------------------------------

/// Inputs to [`working_set_estimate`] mirroring the §G.6 formula.
///
/// Each term is a `u64` byte count so callers can compose them from
/// whatever primitive they have (file count × avg per-file bytes,
/// live arena byte totals, interner snapshot bytes, etc.) without any
/// f64 arithmetic outside this helper.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkingSetInputs {
    /// Post-publish size estimate of the new committed graph.
    ///
    /// Full rebuild: `file_count * avg_bytes_per_file`.
    /// Incremental:  `current_bytes + closure.len() * avg_bytes_per_file`.
    pub new_graph_final_estimate: u64,

    /// Size estimate of the staging tier held in memory while a rebuild
    /// is in flight. `staging_node_count * sizeof(StagingNode)` is the
    /// lower bound; callers add their own per-plugin staging overhead.
    pub staging_overhead: u64,

    /// Pre-rebuild size of the committed interner snapshot. The
    /// rebuild-local interner builder is seeded from this snapshot and
    /// can grow by up to [`INTERNER_BUILDER_OVERHEAD_RATIO`] of this
    /// value before the finalize-time freeze.
    pub interner_snapshot_bytes: u64,
}

/// Compute the Amendment-2 §G.6 admission reservation size.
///
/// ```text
/// working_set_estimate
///     = new_graph_final_estimate * WORKING_SET_MULTIPLIER
///     + staging_overhead
///     + interner_snapshot_bytes * INTERNER_BUILDER_OVERHEAD_RATIO
/// ```
///
/// Each component is rounded with [`f64::ceil`] before the `u64` cast so
/// the admission floor is never *below* the design estimate. The
/// function is `const`-friendly aside from `ceil` — saturating multiply
/// guards against absurd f64 inputs.
#[must_use]
pub fn working_set_estimate(inputs: WorkingSetInputs) -> u64 {
    let WorkingSetInputs {
        new_graph_final_estimate,
        staging_overhead,
        interner_snapshot_bytes,
    } = inputs;

    let grow = |bytes: u64, factor: f64| -> u64 {
        let bytes_f = bytes.to_string().parse::<f64>().unwrap_or(f64::INFINITY);
        rounded_f64_to_u64(bytes_f * factor)
    };

    grow(new_graph_final_estimate, WORKING_SET_MULTIPLIER)
        .saturating_add(staging_overhead)
        .saturating_add(grow(
            interner_snapshot_bytes,
            INTERNER_BUILDER_OVERHEAD_RATIO,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_state_is_empty_by_default() {
        let state = AdmissionState::default();
        assert_eq!(state.loaded_bytes, 0);
        assert_eq!(state.reserved_bytes, 0);
        assert_eq!(state.retained_total_bytes(), 0);
        assert_eq!(state.total_committed_bytes(), 0);
        assert!(state.retained_old.is_empty());
    }

    #[test]
    fn total_committed_sums_all_three_tiers() {
        let state = AdmissionState {
            loaded_bytes: 100_000_000,
            reserved_bytes: 50_000_000,
            ..AdmissionState::default()
        };
        // Can't cheaply build a CodeGraph here, so exercise the sum
        // with just the two counters; the retained tier gets its own
        // integration test in manager.rs.
        assert_eq!(state.total_committed_bytes(), 150_000_000);
    }

    #[test]
    fn working_set_estimate_matches_spec_example() {
        // Spec: working_set = 1.5 * final + staging + 0.25 * interner
        let inputs = WorkingSetInputs {
            new_graph_final_estimate: 1_000_000,
            staging_overhead: 50_000,
            interner_snapshot_bytes: 200_000,
        };
        // 1.5 * 1_000_000 = 1_500_000
        //     + 50_000  = 1_550_000
        //     + 0.25 * 200_000 = 1_600_000
        assert_eq!(working_set_estimate(inputs), 1_600_000);
    }

    #[test]
    fn working_set_estimate_zero_inputs_is_zero() {
        assert_eq!(working_set_estimate(WorkingSetInputs::default()), 0);
    }

    #[test]
    fn working_set_estimate_ceils_fractional_contributions() {
        // 0.25 * 3 = 0.75 → ceil to 1
        let inputs = WorkingSetInputs {
            new_graph_final_estimate: 0,
            staging_overhead: 0,
            interner_snapshot_bytes: 3,
        };
        assert_eq!(working_set_estimate(inputs), 1);
    }

    #[test]
    fn working_set_estimate_saturates_on_absurd_inputs() {
        let inputs = WorkingSetInputs {
            new_graph_final_estimate: u64::MAX,
            staging_overhead: u64::MAX,
            interner_snapshot_bytes: u64::MAX,
        };
        // Must not panic or wrap; should saturate at u64::MAX.
        let estimate = working_set_estimate(inputs);
        assert_eq!(estimate, u64::MAX);
    }
}
