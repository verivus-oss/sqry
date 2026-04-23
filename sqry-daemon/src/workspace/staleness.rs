//! Failed-state staleness classification + exponential backoff.
//!
//! Phase 6c of the sqryd plan (Amendment 2 §C + plan Task 6 Step 6).
//!
//! The daemon's IPC router (Task 8) calls [`classify_staleness`] on
//! a Failed workspace to decide whether to:
//!
//! 1. Serve the last-good graph with `meta.stale = true` (freshness
//!    OK; age below the cap).
//! 2. Return JSON-RPC `-32001 workspace_build_failed` (no prior
//!    good graph at all).
//! 3. Return JSON-RPC `-32002 workspace_stale_expired` (last-good
//!    graph is older than `stale_serve_max_age_hours`).
//!
//! The backoff schedule at [`backoff_delay_for`] drives retry
//! scheduling from the workspace's `retry_count`.

use std::time::{Duration, SystemTime};

/// Outcome of [`classify_staleness`] — one of three possible
/// client-visible states when a workspace is in Failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StalenessVerdict {
    /// The workspace has never built successfully. The IPC
    /// router returns JSON-RPC `-32001 workspace_build_failed`
    /// without a result payload.
    NoPriorGood,

    /// A prior-good graph exists and is within the
    /// `stale_serve_max_age_hours` cap (or the cap is 0,
    /// meaning unlimited). Serve the cached graph with
    /// `meta.stale = true` + `age_hours` in the envelope.
    Stale { age_hours: u64 },

    /// A prior-good graph exists but is older than
    /// `stale_serve_max_age_hours`. Return JSON-RPC
    /// `-32002 workspace_stale_expired` with `age_hours`
    /// in `error.data`.
    Expired { age_hours: u64 },
}

/// Classify a Failed workspace's prior-good staleness against
/// the configured cap.
///
/// - `last_good_at` is `None` for workspaces that have never
///   successfully built, in which case the verdict is
///   [`StalenessVerdict::NoPriorGood`].
/// - `cap_hours == 0` means "serve stale indefinitely" (per
///   plan §5 Step 3); any non-`None` `last_good_at` returns
///   [`StalenessVerdict::Stale`] in that case regardless of age.
/// - `now` is typically `SystemTime::now()` but injectable for
///   tests that drive a virtual clock.
#[must_use]
pub fn classify_staleness(
    last_good_at: Option<SystemTime>,
    cap_hours: u32,
    now: SystemTime,
) -> StalenessVerdict {
    let Some(last_good) = last_good_at else {
        return StalenessVerdict::NoPriorGood;
    };

    // Duration since last good. Treat clock-skew backward jumps
    // as "zero seconds elapsed" — never report negative age.
    let age = now.duration_since(last_good).unwrap_or(Duration::ZERO);
    let age_hours = age.as_secs() / 3_600;

    if cap_hours == 0 || age_hours < u64::from(cap_hours) {
        StalenessVerdict::Stale { age_hours }
    } else {
        StalenessVerdict::Expired { age_hours }
    }
}

// ---------------------------------------------------------------------------
// Exponential backoff schedule (plan §5 Step 6)
// ---------------------------------------------------------------------------

/// Ordered backoff schedule: 30 s → 60 s → 120 s → 300 s → 600 s.
///
/// Amendment 2 §G.7 / plan Step 6: "Retry policy: exponential
/// backoff 30s → 60s → 120s → 300s → 600s. Triggers: timer,
/// file change, explicit `daemon/load`."
///
/// `BACKOFF_SCHEDULE[0]` is applied after the first failure
/// (`retry_count == 1`), `[1]` after the second, and so on.
/// Once `retry_count` exceeds the schedule length, the last
/// entry (`600 s`) is reused — the daemon never gives up on a
/// Failed workspace; it just caps the retry rate.
pub const BACKOFF_SCHEDULE: &[Duration] = &[
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(300),
    Duration::from_secs(600),
];

/// Backoff delay for the given `retry_count`.
///
/// `retry_count == 0` returns `Duration::ZERO` — no failure yet,
/// no backoff required. `retry_count >= 1` indexes into
/// [`BACKOFF_SCHEDULE`] with saturating clamp to the last
/// element.
#[must_use]
pub fn backoff_delay_for(retry_count: u32) -> Duration {
    if retry_count == 0 {
        return Duration::ZERO;
    }
    let idx = (retry_count as usize - 1).min(BACKOFF_SCHEDULE.len() - 1);
    BACKOFF_SCHEDULE[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hours(n: u64) -> Duration {
        Duration::from_secs(n * 3_600)
    }

    #[test]
    fn no_prior_good_returns_no_prior_good_verdict() {
        assert_eq!(
            classify_staleness(None, 24, SystemTime::now()),
            StalenessVerdict::NoPriorGood,
        );
    }

    #[test]
    fn stale_verdict_when_age_within_cap() {
        let now = SystemTime::now();
        let last = now - hours(5);
        assert_eq!(
            classify_staleness(Some(last), 24, now),
            StalenessVerdict::Stale { age_hours: 5 },
        );
    }

    #[test]
    fn expired_verdict_at_or_past_cap() {
        let now = SystemTime::now();
        let last = now - hours(24);
        assert_eq!(
            classify_staleness(Some(last), 24, now),
            StalenessVerdict::Expired { age_hours: 24 },
        );
        let older = now - hours(48);
        assert_eq!(
            classify_staleness(Some(older), 24, now),
            StalenessVerdict::Expired { age_hours: 48 },
        );
    }

    #[test]
    fn cap_zero_disables_expiry() {
        let now = SystemTime::now();
        let ancient = now - hours(10_000);
        assert_eq!(
            classify_staleness(Some(ancient), 0, now),
            StalenessVerdict::Stale { age_hours: 10_000 },
        );
    }

    #[test]
    fn clock_skew_backward_reports_zero_age() {
        let now = SystemTime::now();
        let future = now + hours(1);
        assert_eq!(
            classify_staleness(Some(future), 24, now),
            StalenessVerdict::Stale { age_hours: 0 },
            "clock going backward must not report negative or overflow age",
        );
    }

    #[test]
    fn backoff_schedule_matches_plan_spec() {
        assert_eq!(backoff_delay_for(0), Duration::ZERO);
        assert_eq!(backoff_delay_for(1), Duration::from_secs(30));
        assert_eq!(backoff_delay_for(2), Duration::from_secs(60));
        assert_eq!(backoff_delay_for(3), Duration::from_secs(120));
        assert_eq!(backoff_delay_for(4), Duration::from_secs(300));
        assert_eq!(backoff_delay_for(5), Duration::from_secs(600));
    }

    #[test]
    fn backoff_clamps_to_last_entry_for_large_retry_counts() {
        // After exhausting the schedule, delay saturates at the
        // final value (600 s). The daemon never abandons the
        // workspace — it just caps the retry rate.
        assert_eq!(backoff_delay_for(6), Duration::from_secs(600));
        assert_eq!(backoff_delay_for(100), Duration::from_secs(600));
        assert_eq!(backoff_delay_for(u32::MAX), Duration::from_secs(600));
    }
}
