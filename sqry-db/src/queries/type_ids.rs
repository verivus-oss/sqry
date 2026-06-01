//! Stable on-disk discriminators for `DerivedQuery` implementations.
//!
//! These `u32` IDs are the persistence key in PN3 derived-cache files
//! (`.sqry/graph/derived.sqry`). They are process-independent and must
//! never change or be reused across crate versions. A retired built-in's
//! ID is permanently off-limits; new built-ins take the next free slot.
//!
//! ## Reserved ranges
//!
//! - `0x0000` — invalid / unknown. Must never appear on disk.
//! - `0x0001..=0x000F` — original built-in queries (15 IDs).
//! - `0x0010..=0x0FFF` — additional built-ins (Phase A C indirect-call
//!   precision claims `0x0010` and `0x0011`; T3.7 context-propagation
//!   claims `0x0012`).
//! - `0x1000..=0xFFFF` — reserved for downstream / future built-ins.

pub const CALLERS: u32 = 0x0001;
pub const CALLEES: u32 = 0x0002;
pub const IMPORTS: u32 = 0x0003;
pub const EXPORTS: u32 = 0x0004;
pub const REFERENCES: u32 = 0x0005;
pub const IMPLEMENTS: u32 = 0x0006;
pub const CYCLES: u32 = 0x0007;
pub const IS_IN_CYCLE: u32 = 0x0008;
pub const UNUSED: u32 = 0x0009;
pub const IS_NODE_UNUSED: u32 = 0x000A;
pub const REACHABILITY: u32 = 0x000B;
pub const ENTRY_POINTS: u32 = 0x000C;
pub const REACHABLE_FROM_ENTRY_POINTS: u32 = 0x000D;
pub const SCC: u32 = 0x000E;
pub const CONDENSATION: u32 = 0x000F;
// Phase A — C indirect-call precision (U13).
pub const ADDRESS_TAKEN: u32 = 0x0010;
pub const CALLSITE_PROMISCUOUS: u32 = 0x0011;
/// T3.7 — context-propagation leak detection. Next free slot after the
/// Phase A C indirect-call IDs (0x0010, 0x0011); see 02_DESIGN §5.2.
pub const CONTEXT_PROPAGATION: u32 = 0x0012;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queries::{
        AddressTakenQuery, CalleesQuery, CallersQuery, CallsitePromiscuousQuery, CondensationQuery,
        ContextPropagationQuery, CyclesQuery, EntryPointsQuery, ExportsQuery, ImplementsQuery,
        ImportsQuery, IsInCycleQuery, IsNodeUnusedQuery, ReachabilityQuery,
        ReachableFromEntryPointsQuery, ReferencesQuery, SccQuery, UnusedQuery,
    };
    use crate::query::DerivedQuery;

    #[test]
    fn builtin_query_type_ids_are_unique() {
        let ids: Vec<u32> = vec![
            CallersQuery::QUERY_TYPE_ID,
            CalleesQuery::QUERY_TYPE_ID,
            ImportsQuery::QUERY_TYPE_ID,
            ExportsQuery::QUERY_TYPE_ID,
            ReferencesQuery::QUERY_TYPE_ID,
            ImplementsQuery::QUERY_TYPE_ID,
            CyclesQuery::QUERY_TYPE_ID,
            IsInCycleQuery::QUERY_TYPE_ID,
            UnusedQuery::QUERY_TYPE_ID,
            IsNodeUnusedQuery::QUERY_TYPE_ID,
            ReachabilityQuery::QUERY_TYPE_ID,
            EntryPointsQuery::QUERY_TYPE_ID,
            ReachableFromEntryPointsQuery::QUERY_TYPE_ID,
            SccQuery::QUERY_TYPE_ID,
            CondensationQuery::QUERY_TYPE_ID,
            ContextPropagationQuery::QUERY_TYPE_ID,
            AddressTakenQuery::QUERY_TYPE_ID,
            CallsitePromiscuousQuery::QUERY_TYPE_ID,
        ];

        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "QUERY_TYPE_ID collision: {ids:?}");
        assert!(
            !ids.contains(&0),
            "0x0000 reserved — must never appear as a QUERY_TYPE_ID"
        );

        // Assert each module constant matches its query's associated constant.
        assert_eq!(CallersQuery::QUERY_TYPE_ID, CALLERS);
        assert_eq!(CalleesQuery::QUERY_TYPE_ID, CALLEES);
        assert_eq!(ImportsQuery::QUERY_TYPE_ID, IMPORTS);
        assert_eq!(ExportsQuery::QUERY_TYPE_ID, EXPORTS);
        assert_eq!(ReferencesQuery::QUERY_TYPE_ID, REFERENCES);
        assert_eq!(ImplementsQuery::QUERY_TYPE_ID, IMPLEMENTS);
        assert_eq!(CyclesQuery::QUERY_TYPE_ID, CYCLES);
        assert_eq!(IsInCycleQuery::QUERY_TYPE_ID, IS_IN_CYCLE);
        assert_eq!(UnusedQuery::QUERY_TYPE_ID, UNUSED);
        assert_eq!(IsNodeUnusedQuery::QUERY_TYPE_ID, IS_NODE_UNUSED);
        assert_eq!(ReachabilityQuery::QUERY_TYPE_ID, REACHABILITY);
        assert_eq!(EntryPointsQuery::QUERY_TYPE_ID, ENTRY_POINTS);
        assert_eq!(
            ReachableFromEntryPointsQuery::QUERY_TYPE_ID,
            REACHABLE_FROM_ENTRY_POINTS
        );
        assert_eq!(SccQuery::QUERY_TYPE_ID, SCC);
        assert_eq!(CondensationQuery::QUERY_TYPE_ID, CONDENSATION);
        assert_eq!(ContextPropagationQuery::QUERY_TYPE_ID, CONTEXT_PROPAGATION);
        assert_eq!(AddressTakenQuery::QUERY_TYPE_ID, ADDRESS_TAKEN);
        assert_eq!(
            CallsitePromiscuousQuery::QUERY_TYPE_ID,
            CALLSITE_PROMISCUOUS
        );
    }

    #[test]
    #[allow(
        clippy::assertions_on_constants,
        reason = "the whole point of this test is to lock the PERSISTENT = true invariant; \
                  each assertion guards against a future regression where a built-in opts out"
    )]
    fn builtin_queries_are_persistent_by_default() {
        // Spec §5.1 critical decision: PERSISTENT defaults to true; no
        // built-in opts out. Any future built-in that cannot satisfy the
        // serde bounds required by PN3 must explicitly set PERSISTENT =
        // false — so this test fails loudly if an opt-out slips in.
        assert!(
            CallersQuery::PERSISTENT,
            "CallersQuery::PERSISTENT must be true"
        );
        assert!(
            CalleesQuery::PERSISTENT,
            "CalleesQuery::PERSISTENT must be true"
        );
        assert!(
            ImportsQuery::PERSISTENT,
            "ImportsQuery::PERSISTENT must be true"
        );
        assert!(
            ExportsQuery::PERSISTENT,
            "ExportsQuery::PERSISTENT must be true"
        );
        assert!(
            ReferencesQuery::PERSISTENT,
            "ReferencesQuery::PERSISTENT must be true"
        );
        assert!(
            ImplementsQuery::PERSISTENT,
            "ImplementsQuery::PERSISTENT must be true"
        );
        assert!(
            CyclesQuery::PERSISTENT,
            "CyclesQuery::PERSISTENT must be true"
        );
        assert!(
            IsInCycleQuery::PERSISTENT,
            "IsInCycleQuery::PERSISTENT must be true"
        );
        assert!(
            UnusedQuery::PERSISTENT,
            "UnusedQuery::PERSISTENT must be true"
        );
        assert!(
            IsNodeUnusedQuery::PERSISTENT,
            "IsNodeUnusedQuery::PERSISTENT must be true"
        );
        assert!(
            ReachabilityQuery::PERSISTENT,
            "ReachabilityQuery::PERSISTENT must be true"
        );
        assert!(
            EntryPointsQuery::PERSISTENT,
            "EntryPointsQuery::PERSISTENT must be true"
        );
        assert!(
            ReachableFromEntryPointsQuery::PERSISTENT,
            "ReachableFromEntryPointsQuery::PERSISTENT must be true"
        );
        assert!(SccQuery::PERSISTENT, "SccQuery::PERSISTENT must be true");
        assert!(
            CondensationQuery::PERSISTENT,
            "CondensationQuery::PERSISTENT must be true"
        );
        assert!(
            ContextPropagationQuery::PERSISTENT,
            "ContextPropagationQuery::PERSISTENT must be true"
        );
        assert!(
            AddressTakenQuery::PERSISTENT,
            "AddressTakenQuery::PERSISTENT must be true"
        );
        assert!(
            CallsitePromiscuousQuery::PERSISTENT,
            "CallsitePromiscuousQuery::PERSISTENT must be true"
        );
    }
}
