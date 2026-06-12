//! Ordered step trace vocabulary for the Phase 2 binding plane witness.
//!
//! Every transition the resolver makes — scope entry, bucket lookup, alias
//! follow, shadow rejection, candidate consideration, visibility filter,
//! tie-break, rejection, final choice — is represented as one variant of
//! [`ResolutionStep`]. The sequence of steps recorded during a
//! `BindingPlane::resolve()` call is the explainability contract Phases 3-6
//! read when walking witnesses.
//!
//! P2U06 adds `pub steps: Vec<ResolutionStep>` to `SymbolResolutionWitness`
//! so the steps surface on every resolve call. P2U07's `resolve_shared()`
//! helper is the emission point.

use std::fmt;

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use super::super::alias::AliasEntryId;
use super::super::scope::ScopeId;
use crate::graph::unified::edge::id::EdgeId;
use crate::graph::unified::file::id::FileId;
use crate::graph::unified::node::id::NodeId;
use crate::graph::unified::resolution::{ResolutionMode, SymbolCandidateBucket};
use crate::graph::unified::string::id::StringId;

/// One step in an ordered resolution trace. Every variant carries only
/// `Eq`-compatible field types so [`SymbolResolutionWitness`]'s `Eq` derive
/// is preserved when `steps: Vec<ResolutionStep>` is added in P2U06.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolutionStep {
    /// Resolver entered the file-scope boundary for a query. Distinct from
    /// `EnterScope` because files are **not** arena-addressable scopes —
    /// they are addressed via `FileId`.
    EnterFileScope {
        /// File ID for the scope boundary being entered.
        file: FileId,
    },
    /// Resolver stepped into an arena-addressable scope.
    EnterScope {
        /// The scope being entered.
        scope: ScopeId,
    },
    /// Resolver probed a specific candidate bucket.
    LookupInBucket {
        /// Bucket being probed.
        bucket: SymbolCandidateBucket,
    },
    /// Resolver queried the `AliasTable` for the current scope.
    LookupInAliasTable {
        /// Scope whose alias table was queried.
        scope: ScopeId,
    },
    /// Resolver queried the `ShadowTable` for the current scope.
    LookupInShadowChain {
        /// Scope whose shadow chain was queried.
        scope: ScopeId,
    },
    /// Resolver considered a candidate node with its rank in the ordering.
    ConsiderCandidate {
        /// The candidate node being considered.
        node: NodeId,
        /// Zero-based rank of this candidate within the bucket ordering.
        rank: u16,
    },
    /// Resolver followed an alias edge from `from` to `to`.
    FollowAlias {
        /// Dense alias handle for the alias table entry.
        alias: AliasEntryId,
        /// Original symbol name before alias substitution.
        from: StringId,
        /// Alias-substituted symbol name.
        to: StringId,
    },
    /// Resolver detected that an outer binding is shadowed by an inner one.
    ShadowedBy {
        /// Outer scope whose binding is being shadowed.
        outer: ScopeId,
        /// Inner scope that introduces the shadowing binding.
        inner: ScopeId,
        /// Node in the inner scope that shadows the outer binding.
        by_node: NodeId,
    },
    /// Resolver followed an `EdgeKind::Imports` edge.
    FollowImportEdge {
        /// The import edge being followed.
        edge: EdgeId,
    },
    /// Resolver followed an `EdgeKind::Exports` re-export edge.
    FollowExportEdge {
        /// The export edge being followed.
        edge: EdgeId,
        /// Original exported name.
        from: StringId,
        /// Re-exported name at the destination.
        to: StringId,
    },
    /// Resolver performed an attribute / member lookup on a receiver node.
    /// Used for Python `y.method`, JS `obj.field`, Rust `self::Foo::bar`.
    FollowAttributeLookup {
        /// Node on which the attribute lookup is performed.
        receiver: NodeId,
        /// Name of the attribute or member being looked up.
        attribute: StringId,
    },
    /// Resolver rejected a candidate on visibility grounds.
    FilterByVisibility {
        /// Candidate node that was rejected.
        candidate: NodeId,
        /// Reason for the visibility rejection.
        reason: VisibilityReason,
    },
    /// Resolver applied the caller-supplied resolution mode.
    ApplyResolutionMode {
        /// The resolution mode that was applied.
        mode: ResolutionMode,
    },
    /// Resolver broke a tie between otherwise-equivalent candidates.
    TieBreak {
        /// Rule that broke the tie.
        reason: TieBreakReason,
    },
    /// Resolver rejected a candidate for a reason.
    Rejected {
        /// Node that was rejected.
        node: NodeId,
        /// Reason for rejection.
        reason: RejectionReason,
    },
    /// Resolver chose a final winner.
    Chose {
        /// The winning node.
        node: NodeId,
    },
    /// Resolver returned more than one candidate without a tie-break.
    Ambiguous {
        /// All surviving candidates (inline up to 4 to avoid heap allocation).
        candidates: SmallVec<[NodeId; 4]>,
    },
    /// Resolver gave up without a winner.
    Unresolved {
        /// Symbol that could not be resolved.
        symbol: StringId,
        /// Reason no winner was produced.
        reason: UnresolvedReason,
    },
}

impl fmt::Display for ResolutionStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolutionStep::EnterFileScope { file } => {
                write!(f, "enter file scope ({file})")
            }
            ResolutionStep::EnterScope { scope } => {
                write!(f, "enter scope ({scope:?})")
            }
            ResolutionStep::LookupInBucket { bucket } => {
                write!(f, "lookup in bucket: {bucket:?}")
            }
            ResolutionStep::LookupInAliasTable { scope } => {
                write!(f, "lookup in alias table (scope {scope:?})")
            }
            ResolutionStep::LookupInShadowChain { scope } => {
                write!(f, "lookup in shadow chain (scope {scope:?})")
            }
            ResolutionStep::ConsiderCandidate { node, rank } => {
                write!(f, "consider candidate {node} at rank {rank}")
            }
            ResolutionStep::FollowAlias { alias, from, to } => {
                write!(f, "follow alias {alias:?}: {from} → {to}")
            }
            ResolutionStep::ShadowedBy {
                outer,
                inner,
                by_node,
            } => {
                write!(
                    f,
                    "shadowed by {by_node} (outer scope {outer:?}, inner scope {inner:?})"
                )
            }
            ResolutionStep::FollowImportEdge { edge } => {
                write!(f, "follow import edge {edge}")
            }
            ResolutionStep::FollowExportEdge { edge, from, to } => {
                write!(f, "follow export edge {edge}: {from} → {to}")
            }
            ResolutionStep::FollowAttributeLookup {
                receiver,
                attribute,
            } => {
                write!(f, "follow attribute lookup .{attribute} on {receiver}")
            }
            ResolutionStep::FilterByVisibility { candidate, reason } => {
                write!(f, "filter by visibility: {candidate} rejected ({reason:?})")
            }
            ResolutionStep::ApplyResolutionMode { mode } => {
                write!(f, "apply resolution mode: {mode:?}")
            }
            ResolutionStep::TieBreak { reason } => {
                write!(f, "tie-break: {reason:?}")
            }
            ResolutionStep::Rejected { node, reason } => {
                write!(f, "rejected {node}: {reason:?}")
            }
            ResolutionStep::Chose { node } => {
                write!(f, "chose {node}")
            }
            ResolutionStep::Ambiguous { candidates } => {
                write!(f, "ambiguous: {} candidates", candidates.len())
            }
            ResolutionStep::Unresolved { symbol, reason } => {
                write!(f, "unresolved {symbol}: {reason:?}")
            }
        }
    }
}

/// Reason a candidate was filtered out on visibility grounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VisibilityReason {
    /// Symbol has `private` / `pub(self)` visibility.
    Private,
    /// Symbol is `sealed` (e.g., Kotlin sealed class, Java sealed type).
    Sealed,
    /// Symbol has package-internal or module-internal visibility.
    Internal,
    /// Symbol is only visible within the declaring file.
    FileScoped,
    /// Symbol is accessible only via a specific module path.
    ModulePath,
    /// Symbol is not re-exported across a crate boundary.
    CrateBoundary,
    /// Symbol has `protected` visibility (Java, C++, C#, Kotlin, Scala, Ruby,
    /// Swift, PHP, Dart — any language where `protected` is a first-class
    /// access modifier). Appended after `CrateBoundary` so the postcard wire
    /// format remains additive (existing encoded variants are unaffected).
    Protected,
}

/// Reason a tie was broken between otherwise-equivalent candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TieBreakReason {
    /// Candidate in a narrower / more-local scope wins.
    NarrowerScope,
    /// Candidate reachable via a shorter qualified path wins.
    ShorterPath,
    /// Candidate introduced by an explicit import wins over wildcard.
    ExplicitImport,
    /// Earlier declaration (lower line number) wins.
    EarlierDeclaration,
    /// Language-specific priority rule applied (e.g., built-in over user def).
    LanguagePriority,
}

/// Reason a candidate was unconditionally rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectionReason {
    /// Candidate is not reachable from the query scope.
    OutOfScope,
    /// Candidate is shadowed by an inner binding.
    Shadowed,
    /// Candidate's `NodeKind` does not match the expected kind filter.
    WrongKind,
    /// Candidate's visibility is `private` relative to the call site.
    PrivateVisibility,
}

/// Reason the resolver gave up without producing a winner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnresolvedReason {
    /// No binding for the queried symbol was found in any scope.
    NotInAnyScope,
    /// Candidates were found but all were rejected by filters.
    AllCandidatesRejected,
    /// Multiple candidates survived and no tie-break rule applied.
    AmbiguousWithoutTieBreak,
    /// The requested file is valid but is not present in the indexed graph.
    FileNotIndexed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::node::id::NodeId;

    #[test]
    fn chose_round_trips_through_eq() {
        let step = ResolutionStep::Chose {
            node: NodeId::new(42, 1),
        };
        assert_eq!(step.clone(), step);
    }

    #[test]
    fn ambiguous_round_trips_through_postcard() {
        let mut candidates = SmallVec::<[NodeId; 4]>::new();
        candidates.push(NodeId::new(1, 1));
        candidates.push(NodeId::new(2, 1));
        candidates.push(NodeId::new(3, 1));
        let step = ResolutionStep::Ambiguous { candidates };
        let bytes = postcard::to_allocvec(&step).expect("serialize");
        let restored: ResolutionStep = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(restored, step);
    }

    #[test]
    fn smallvec_grows_past_inline_capacity() {
        let mut candidates = SmallVec::<[NodeId; 4]>::new();
        for idx in 0..8 {
            candidates.push(NodeId::new(idx, 1));
        }
        assert_eq!(candidates.len(), 8);
        let step = ResolutionStep::Ambiguous {
            candidates: candidates.clone(),
        };
        let bytes = postcard::to_allocvec(&step).expect("serialize");
        let restored: ResolutionStep = postcard::from_bytes(&bytes).expect("deserialize");
        if let ResolutionStep::Ambiguous { candidates: got } = restored {
            assert_eq!(got.len(), 8);
        } else {
            panic!("expected Ambiguous variant after round-trip");
        }
    }

    #[test]
    fn every_variant_serializes_through_postcard() {
        use super::super::super::alias::AliasEntryId;
        use super::super::super::scope::ScopeId;
        use crate::graph::unified::edge::id::EdgeId;
        use crate::graph::unified::file::id::FileId;
        use crate::graph::unified::resolution::{ResolutionMode, SymbolCandidateBucket};
        use crate::graph::unified::string::id::StringId;

        let steps = vec![
            ResolutionStep::EnterFileScope {
                file: FileId::new(0),
            },
            ResolutionStep::EnterScope {
                scope: ScopeId::new(1, 1),
            },
            ResolutionStep::LookupInBucket {
                bucket: SymbolCandidateBucket::ExactQualified,
            },
            ResolutionStep::LookupInAliasTable {
                scope: ScopeId::new(2, 1),
            },
            ResolutionStep::LookupInShadowChain {
                scope: ScopeId::new(3, 1),
            },
            ResolutionStep::ConsiderCandidate {
                node: NodeId::new(4, 1),
                rank: 0,
            },
            ResolutionStep::FollowAlias {
                alias: AliasEntryId(0),
                from: StringId::new(1),
                to: StringId::new(2),
            },
            ResolutionStep::ShadowedBy {
                outer: ScopeId::new(1, 1),
                inner: ScopeId::new(2, 1),
                by_node: NodeId::new(5, 1),
            },
            ResolutionStep::FollowImportEdge {
                edge: EdgeId::new(0),
            },
            ResolutionStep::FollowExportEdge {
                edge: EdgeId::new(1),
                from: StringId::new(3),
                to: StringId::new(4),
            },
            ResolutionStep::FollowAttributeLookup {
                receiver: NodeId::new(6, 1),
                attribute: StringId::new(5),
            },
            ResolutionStep::FilterByVisibility {
                candidate: NodeId::new(7, 1),
                reason: VisibilityReason::Private,
            },
            ResolutionStep::ApplyResolutionMode {
                mode: ResolutionMode::Strict,
            },
            ResolutionStep::TieBreak {
                reason: TieBreakReason::NarrowerScope,
            },
            ResolutionStep::Rejected {
                node: NodeId::new(8, 1),
                reason: RejectionReason::OutOfScope,
            },
            ResolutionStep::Chose {
                node: NodeId::new(9, 1),
            },
            ResolutionStep::Ambiguous {
                candidates: SmallVec::from_slice(&[NodeId::new(10, 1), NodeId::new(11, 1)]),
            },
            ResolutionStep::Unresolved {
                symbol: StringId::new(6),
                reason: UnresolvedReason::NotInAnyScope,
            },
        ];
        assert_eq!(steps.len(), 18, "must have exactly 18 variants");
        let bytes = postcard::to_allocvec(&steps).expect("serialize");
        let restored: Vec<ResolutionStep> = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(restored, steps);
    }

    /// Every `ResolutionStep` variant's `Display` output must be non-empty and
    /// contain the variant name (case-insensitive keyword match).
    #[test]
    fn all_variants_display_non_empty_and_contains_variant_keyword() {
        use super::super::super::alias::AliasEntryId;
        use super::super::super::scope::ScopeId;
        use crate::graph::unified::edge::id::EdgeId;
        use crate::graph::unified::file::id::FileId;
        use crate::graph::unified::resolution::{ResolutionMode, SymbolCandidateBucket};
        use crate::graph::unified::string::id::StringId;

        let steps: Vec<(ResolutionStep, &str)> = vec![
            (
                ResolutionStep::EnterFileScope {
                    file: FileId::new(0),
                },
                "enter file scope",
            ),
            (
                ResolutionStep::EnterScope {
                    scope: ScopeId::new(1, 1),
                },
                "enter scope",
            ),
            (
                ResolutionStep::LookupInBucket {
                    bucket: SymbolCandidateBucket::ExactQualified,
                },
                "lookup in bucket",
            ),
            (
                ResolutionStep::LookupInAliasTable {
                    scope: ScopeId::new(2, 1),
                },
                "lookup in alias table",
            ),
            (
                ResolutionStep::LookupInShadowChain {
                    scope: ScopeId::new(3, 1),
                },
                "lookup in shadow chain",
            ),
            (
                ResolutionStep::ConsiderCandidate {
                    node: NodeId::new(4, 1),
                    rank: 0,
                },
                "consider candidate",
            ),
            (
                ResolutionStep::FollowAlias {
                    alias: AliasEntryId(0),
                    from: StringId::new(1),
                    to: StringId::new(2),
                },
                "follow alias",
            ),
            (
                ResolutionStep::ShadowedBy {
                    outer: ScopeId::new(1, 1),
                    inner: ScopeId::new(2, 1),
                    by_node: NodeId::new(5, 1),
                },
                "shadowed by",
            ),
            (
                ResolutionStep::FollowImportEdge {
                    edge: EdgeId::new(0),
                },
                "follow import edge",
            ),
            (
                ResolutionStep::FollowExportEdge {
                    edge: EdgeId::new(1),
                    from: StringId::new(3),
                    to: StringId::new(4),
                },
                "follow export edge",
            ),
            (
                ResolutionStep::FollowAttributeLookup {
                    receiver: NodeId::new(6, 1),
                    attribute: StringId::new(5),
                },
                "follow attribute lookup",
            ),
            (
                ResolutionStep::FilterByVisibility {
                    candidate: NodeId::new(7, 1),
                    reason: VisibilityReason::Private,
                },
                "filter by visibility",
            ),
            (
                ResolutionStep::ApplyResolutionMode {
                    mode: ResolutionMode::Strict,
                },
                "apply resolution mode",
            ),
            (
                ResolutionStep::TieBreak {
                    reason: TieBreakReason::NarrowerScope,
                },
                "tie-break",
            ),
            (
                ResolutionStep::Rejected {
                    node: NodeId::new(8, 1),
                    reason: RejectionReason::OutOfScope,
                },
                "rejected",
            ),
            (
                ResolutionStep::Chose {
                    node: NodeId::new(9, 1),
                },
                "chose",
            ),
            (
                ResolutionStep::Ambiguous {
                    candidates: SmallVec::from_slice(&[NodeId::new(10, 1), NodeId::new(11, 1)]),
                },
                "ambiguous",
            ),
            (
                ResolutionStep::Unresolved {
                    symbol: StringId::new(6),
                    reason: UnresolvedReason::NotInAnyScope,
                },
                "unresolved",
            ),
        ];

        assert_eq!(steps.len(), 18, "must cover all 18 ResolutionStep variants");

        for (step, keyword) in &steps {
            let text = format!("{step}");
            assert!(
                !text.is_empty(),
                "Display output for step {step:?} must be non-empty"
            );
            assert!(
                text.to_lowercase().contains(keyword),
                "Display output for step {step:?} must contain keyword {:?}, got: {:?}",
                keyword,
                text
            );
        }
    }

    /// Round-trip every variant of every sub-enum through postcard.  Cheap
    /// insurance that the serde representation is stable and complete — if a
    /// variant is accidentally omitted from the `every_variant_serializes_through_postcard`
    /// test above, this test will still catch it.
    #[test]
    fn all_subenum_variants_round_trip() {
        macro_rules! round_trip {
            ($val:expr) => {{
                let bytes = postcard::to_allocvec(&$val).expect("serialize");
                let restored = postcard::from_bytes(&bytes).expect("deserialize");
                assert_eq!($val, restored);
            }};
        }

        // VisibilityReason — 7 variants (CrateBoundary was last; Protected is
        // appended to preserve the additive wire format).
        round_trip!(VisibilityReason::Private);
        round_trip!(VisibilityReason::Sealed);
        round_trip!(VisibilityReason::Internal);
        round_trip!(VisibilityReason::FileScoped);
        round_trip!(VisibilityReason::ModulePath);
        round_trip!(VisibilityReason::CrateBoundary);
        round_trip!(VisibilityReason::Protected);

        // TieBreakReason — 5 variants
        round_trip!(TieBreakReason::NarrowerScope);
        round_trip!(TieBreakReason::ShorterPath);
        round_trip!(TieBreakReason::ExplicitImport);
        round_trip!(TieBreakReason::EarlierDeclaration);
        round_trip!(TieBreakReason::LanguagePriority);

        // RejectionReason — 4 variants
        round_trip!(RejectionReason::OutOfScope);
        round_trip!(RejectionReason::Shadowed);
        round_trip!(RejectionReason::WrongKind);
        round_trip!(RejectionReason::PrivateVisibility);

        // UnresolvedReason — 4 variants
        round_trip!(UnresolvedReason::NotInAnyScope);
        round_trip!(UnresolvedReason::AllCandidatesRejected);
        round_trip!(UnresolvedReason::AmbiguousWithoutTieBreak);
        round_trip!(UnresolvedReason::FileNotIndexed);
    }
}
