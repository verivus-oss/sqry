//! Framework route metadata — Phase β joint-stubs.
//!
//! This module defines the shared types that the framework-route-extractors
//! work (Plan A — `docs/superpowers/plans/2026-05-25-framework-route-extractors-dag.toml`)
//! will populate. It lands as part of the V12 schema bump alongside Plan B's
//! `DispatchTables` so a single V11 → V12 upconvert covers both surfaces.
//!
//! # Population frontier
//!
//! The MCP `framework:` filter param and the `Predicate::FrameworkEq`
//! planner predicate that ship in this same PR are **complete capabilities**
//! — they parse, compile, fuse, cost-gate, and evaluate end-to-end via
//! `NodeMetadataStore::framework_route` (see
//! `sqry-db/src/planner/execute.rs` `CompiledPredicate::FrameworkEq`).
//! Coverage in `sqry-db/tests/phase_beta_predicate_evaluation.rs`
//! exercises empty / match / non-match / multi-target / AND-composition
//! paths against fixture graphs that populate the side store directly.
//!
//! What is *not* in this PR is the **data-population frontier**: no
//! production language plugin in `sqry-core` writes
//! `FrameworkRouteMetadata` entries yet. Plan A's downstream
//! (`feat/framework-route-extractors`) wires the Phase 4f extractor
//! pipeline that emits entries — at which point the matched node set
//! widens without further changes to the planner predicate or MCP
//! filter wire shape.
//!
//! # Wire stability
//!
//! [`FrameworkId`] is `#[repr(u16)]` with explicit discriminants `0..=18`
//! pinned for V12 on-disk stability (per Plan A unit `U01_CRATE_SCAFFOLD`
//! line 119 and `critical_decisions` line 124 in the DAG). Discriminants
//! must not be re-ordered without a snapshot-format bump.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::super::edge::kind::HttpMethod;
use super::super::node::id::NodeId;

/// Stable, on-disk-pinned identifier for a supported web framework.
///
/// Discriminants `0..=18` are explicit and pinned for V12 on-disk stability —
/// see Plan A's `U01_CRATE_SCAFFOLD` acceptance criterion
/// "`FrameworkId` discriminants pinned with `#[repr(u16)]` and explicit
/// values 0-18". The ordering below mirrors the Plan A 02_DESIGN §2 variant
/// set; **variant names match the 02_DESIGN doc 1:1** (e.g. `Actix`, not
/// `ActixWeb`) so design references and code citations stay in lockstep.
/// Discriminants must remain stable across releases once V12 ships.
///
/// # Wire compatibility
///
/// This enum is `#[repr(u16)]` so each variant has a fixed numeric value
/// stable across releases. (Postcard, sqry's on-disk format, encodes serde
/// enum variants by *declaration index* — a varint — so the contract is
/// "do not re-order variants", with `#[repr(u16)]` reinforcing that intent
/// at the Rust ABI level and making the discriminants directly queryable
/// via [`Self::as_u16`].)
#[allow(clippy::upper_case_acronyms)]
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameworkId {
    /// ASP.NET Core (C#).
    AspNetCore = 0,
    /// Actix Web (Rust). Plan A 02_DESIGN §2 names this `Actix`.
    Actix = 1,
    /// Axum (Rust).
    Axum = 2,
    /// chi (Go).
    Chi = 3,
    /// Django (Python).
    Django = 4,
    /// Express (Node.js / TypeScript).
    Express = 5,
    /// FastAPI (Python).
    FastApi = 6,
    /// Fastify (Node.js / TypeScript).
    Fastify = 7,
    /// Flask (Python).
    Flask = 8,
    /// gin (Go).
    Gin = 9,
    /// Koa (Node.js / TypeScript).
    Koa = 10,
    /// Laravel (PHP).
    Laravel = 11,
    /// NestJS (TypeScript).
    NestJs = 12,
    /// Rails (Ruby).
    Rails = 13,
    /// Rocket (Rust).
    Rocket = 14,
    /// Sinatra (Ruby).
    Sinatra = 15,
    /// Spring Web (Java / Kotlin).
    Spring = 16,
    /// Starlette (Python).
    Starlette = 17,
    /// Symfony (PHP).
    Symfony = 18,
}

impl FrameworkId {
    /// All supported framework identifiers, in pinned discriminant order.
    ///
    /// Provided as a helper for tests and downstream consumers that need a
    /// stable enumeration set. The pinning contract means this list is
    /// append-only across V12-compatible releases.
    pub const ALL: &'static [FrameworkId] = &[
        FrameworkId::AspNetCore,
        FrameworkId::Actix,
        FrameworkId::Axum,
        FrameworkId::Chi,
        FrameworkId::Django,
        FrameworkId::Express,
        FrameworkId::FastApi,
        FrameworkId::Fastify,
        FrameworkId::Flask,
        FrameworkId::Gin,
        FrameworkId::Koa,
        FrameworkId::Laravel,
        FrameworkId::NestJs,
        FrameworkId::Rails,
        FrameworkId::Rocket,
        FrameworkId::Sinatra,
        FrameworkId::Spring,
        FrameworkId::Starlette,
        FrameworkId::Symfony,
    ];

    /// Returns the pinned `u16` discriminant.
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        self as u16
    }
}

/// Resolution status of a synthesized framework route — Plan A 02_DESIGN §2.
///
/// Captures the static-vs-runtime nature of the extracted route declaration
/// so downstream consumers (planner, MCP filters, agent diagnostics) can
/// distinguish a fully-resolved route from one whose path or handler is
/// known only at runtime.
///
/// # Stub discipline
///
/// No extractor populates this field in the joint-stubs PR; Plan A's
/// extractor units (`U05_*` … `U09_*`) write `Static` / `RequiresRuntime`
/// / `Ambiguous` per the per-framework recognizer logic in DESIGN §4 +
/// §7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStatus {
    /// Path template and handler both resolved at static-analysis time.
    Static,
    /// Path template (or handler) depends on a runtime expression and is
    /// preserved as the source-form template; no `HttpRequest` edge from
    /// external callers will match (per DESIGN §7 line 244).
    RequiresRuntime,
    /// Multiple candidate handlers matched the route declaration (e.g.
    /// Spring class-level prefix + multiple methods with matching path);
    /// the route entry carries an ambiguity envelope rather than a single
    /// resolved `NodeId`.
    Ambiguous,
}

/// Normalised path template for a synthesized framework route — Plan A
/// 02_DESIGN §2 (line 135) + §2 prose (line 153).
///
/// # Joint-stub shape
///
/// DESIGN §2 specifies `PathTemplate` as a sequence of
/// `Segment::Literal(ArcStr)` + `Segment::Param { name, constraint }`
/// after parsing. The joint-stub stores the **source-form template
/// string** in [`Self::template`] — extractors haven't landed yet, so no
/// per-framework parser exists to produce the parsed segment list.
/// Plan A's `U03_PATHTEMPLATE_PARSER` unit replaces the body with the
/// parsed-segment form and adds a `segments` field; the surface name
/// (`PathTemplate`) is the V12-stable wire identity, and the field
/// substitution stays additive (existing `template` field is preserved
/// as the canonical reconstructed-source-form serialisation).
///
/// Two `PathTemplate`s compare equal when their normalized
/// representations match — the cross-framework equality contract from
/// DESIGN §2 line 153.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PathTemplate {
    /// Source-form template string (e.g. `"/users/{id}"`, `"/users/:id"`,
    /// `"/users/<int:id>"`). Plan A's parser unit will preserve this verbatim
    /// in addition to producing the parsed segment list, so downstream
    /// V12-compatible reads see a stable wire field.
    pub template: String,
}

impl PathTemplate {
    /// Construct a [`PathTemplate`] from its source-form template string.
    #[must_use]
    pub fn new<S: Into<String>>(template: S) -> Self {
        Self {
            template: template.into(),
        }
    }
}

/// Per-node framework-route metadata — Plan A 02_DESIGN §6 line 233.
///
/// One entry per Endpoint / Service / Resource node that a framework
/// extractor classified as a synthesized route. The DESIGN field set is
/// `{framework_id, path_template, method, resolution_status}`; this
/// joint-stub matches that shape exactly so downstream PRs only have to
/// populate the fields, not re-thread the storage envelope.
///
/// # Stub discipline
///
/// No extractor populates [`FrameworkRouteMetadata`] in the joint-stubs PR.
/// `Default` returns a sentinel entry with `method = HttpMethod::Get` and
/// `resolution_status = ResolutionStatus::Static`; downstream extractors
/// always overwrite these defaults with the per-route classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameworkRouteMetadata {
    /// Which framework classified this node as a route.
    pub framework_id: FrameworkId,
    /// Normalised path template — see [`PathTemplate`] for the joint-stub
    /// shape.
    pub path_template: PathTemplate,
    /// HTTP method for the route declaration. Reuses the existing
    /// [`HttpMethod`] enum already used by `EdgeKind::HttpRequest` so the
    /// route side-table and the cross-language HTTP-edge plane share one
    /// vocabulary (DESIGN §6 line 151).
    pub method: HttpMethod,
    /// Static / runtime / ambiguous resolution classification.
    pub resolution_status: ResolutionStatus,
}

impl Default for FrameworkRouteMetadata {
    fn default() -> Self {
        // Sentinel default — extractors always overwrite. Choosing
        // `AspNetCore = 0` keeps the default fully deterministic
        // (smallest pinned discriminant). `HttpMethod::Get` and
        // `ResolutionStatus::Static` are the most common per-route shape
        // and the cheapest sentinel for postcard round-trip tests.
        Self {
            framework_id: FrameworkId::AspNetCore,
            path_template: PathTemplate::default(),
            method: HttpMethod::Get,
            resolution_status: ResolutionStatus::Static,
        }
    }
}

impl FrameworkRouteMetadata {
    /// Construct a static-resolution entry tagged with the given framework
    /// and path template. Convenience for downstream extractors and tests.
    #[must_use]
    pub fn new<S: Into<String>>(
        framework_id: FrameworkId,
        path_template: S,
        method: HttpMethod,
    ) -> Self {
        Self {
            framework_id,
            path_template: PathTemplate::new(path_template),
            method,
            resolution_status: ResolutionStatus::Static,
        }
    }

    /// Construct an entry tagged with the given framework, with default
    /// path template / method / resolution status. Used by tests and the
    /// planner predicate's `framework`-only filter when no other facts are
    /// known yet.
    #[must_use]
    pub fn for_framework(framework_id: FrameworkId) -> Self {
        Self {
            framework_id,
            ..Self::default()
        }
    }
}

/// On-disk wire shape for [`super::NodeMetadataStore::framework_routes`].
///
/// Serializes as a postcard-flat `Vec<(NodeId, FrameworkRouteMetadata)>`
/// because postcard does not natively support `BTreeMap` keys with custom
/// non-string types and we want bit-for-bit deterministic snapshots. The
/// helpers below convert between the in-memory `BTreeMap` and the wire
/// `Vec` shape.
pub type FrameworkRoutesMap = BTreeMap<NodeId, FrameworkRouteMetadata>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framework_id_discriminants_are_pinned() {
        // Spot-check a handful of the pinned values from Plan A's DAG.
        // Reordering these constants is a V12-breaking change.
        assert_eq!(FrameworkId::AspNetCore.as_u16(), 0);
        assert_eq!(FrameworkId::Actix.as_u16(), 1);
        assert_eq!(FrameworkId::Axum.as_u16(), 2);
        assert_eq!(FrameworkId::Symfony.as_u16(), 18);
    }

    #[test]
    fn framework_id_all_covers_every_variant() {
        // If a new variant is added, ALL must be updated alongside it.
        // 19 variants, discriminants 0..=18 inclusive.
        assert_eq!(FrameworkId::ALL.len(), 19);
        for (idx, fw) in FrameworkId::ALL.iter().enumerate() {
            assert_eq!(fw.as_u16() as usize, idx);
        }
    }

    #[test]
    fn framework_route_metadata_default_has_sentinel_fields() {
        let meta = FrameworkRouteMetadata::default();
        assert_eq!(meta.framework_id, FrameworkId::AspNetCore);
        assert_eq!(meta.path_template, PathTemplate::default());
        assert_eq!(meta.method, HttpMethod::Get);
        assert_eq!(meta.resolution_status, ResolutionStatus::Static);
    }

    #[test]
    fn framework_route_metadata_round_trips_through_postcard() {
        let meta = FrameworkRouteMetadata::new(FrameworkId::Flask, "/users/{id}", HttpMethod::Post);
        let bytes = postcard::to_allocvec(&meta).expect("serialize");
        let round: FrameworkRouteMetadata =
            postcard::from_bytes(&bytes).expect("round-trip deserialize");
        assert_eq!(meta, round);
        assert_eq!(round.framework_id, FrameworkId::Flask);
        assert_eq!(round.path_template.template, "/users/{id}");
        assert_eq!(round.method, HttpMethod::Post);
        assert_eq!(round.resolution_status, ResolutionStatus::Static);
    }

    #[test]
    fn resolution_status_round_trips_through_postcard() {
        for status in [
            ResolutionStatus::Static,
            ResolutionStatus::RequiresRuntime,
            ResolutionStatus::Ambiguous,
        ] {
            let bytes = postcard::to_allocvec(&status).expect("serialize");
            let round: ResolutionStatus = postcard::from_bytes(&bytes).expect("deserialize");
            assert_eq!(status, round);
        }
    }
}
