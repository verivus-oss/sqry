//! On-disk persisted-state schema for daemon workspace bookkeeping.
//!
//! `STEP_6` (workspace-aware-cross-repo, 2026-04-26) introduced the
//! `workspace_id` dimension on [`super::state::WorkspaceKey`] and a
//! corresponding **v2** wire shape. To keep older sqryd state files —
//! which never carried `workspace_id` and used the legacy `index_root`
//! field name — readable across an upgrade, this module owns:
//!
//! - [`PersistedState::FORMAT_VERSION`] — the current schema version
//!   (bumped from `1` to `2` by `STEP_6`).
//! - [`PersistedStateV1`] — the legacy v1 wire shape.
//! - [`PersistedState`] (the canonical v2 form) — `format_version` +
//!   a vector of `WorkspaceKey`s.
//! - [`load_persisted_state`] — the on-load upconverter. Reads JSON,
//!   peeks the version field, and either deserialises directly into
//!   the v2 form or upconverts a v1 payload by injecting
//!   `workspace_id = None` into every key (the `WorkspaceKey` serde
//!   impl handles the `index_root → source_root` field rename via the
//!   `#[serde(alias)]` attribute set in `state.rs`).
//!
//! No active-user migration is required (per `CLAUDE.md` ground rule
//! #6: "we are in DEVELOPMENT, we DO NOT have to consider providing
//! migration paths"). The upconverter exists as the minimal correctness
//! gate so any future persisted-state writer cannot accidentally
//! produce an unparseable file under the new schema.
//!
//! The daemon does not currently *write* persisted state to disk — the
//! production durability surface is the per-graph snapshot at
//! `.sqry/graph/snapshot.sqry` plus the derived-cache companion at
//! `.sqry/graph/derived.sqry`, both owned by `sqry-core`. This module
//! lays the typed groundwork for the daemon-side state writer that
//! will land alongside the eventual session-resumption work; the
//! v1→v2 upconverter and frozen test fixtures verify the contract
//! today so the writer can be flipped on without revisiting the
//! schema.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::state::WorkspaceKey;

/// The canonical v2 persisted-state shape introduced by `STEP_6`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedState {
    /// On-disk schema version. v2 is current; v1 is upconverted on load.
    pub format_version: u32,
    /// Bookkeeping snapshot — every workspace key the daemon was
    /// tracking when the state was persisted. Per-key state (current
    /// bytes / `last_good_at` / pinned) is recovered from
    /// `.sqry/graph/snapshot.sqry` on reload, so we only persist the
    /// keys themselves.
    pub keys: Vec<WorkspaceKey>,
}

impl PersistedState {
    /// Current persisted-state schema version. v1 lacked
    /// `workspace_id` and used `index_root`; v2 (this constant) carries
    /// `workspace_id: Option<WorkspaceId>` and renames the field to
    /// `source_root`.
    pub const FORMAT_VERSION: u32 = 2;

    /// Construct a fresh v2 state from a key list.
    #[must_use]
    pub fn new_v2(keys: Vec<WorkspaceKey>) -> Self {
        Self {
            format_version: Self::FORMAT_VERSION,
            keys,
        }
    }
}

/// Parse a v1-shaped persisted-state payload. Used by the on-load
/// upconverter; not exposed publicly because v1 is a legacy format.
#[derive(Debug, Clone, Deserialize)]
struct PersistedStateV1 {
    /// v1 marker is exactly `1`. The `format_version` field IS present
    /// in v1 — the upconverter dispatches on its value.
    #[allow(dead_code)] // shape-only — we matched on it before deserialising into this type
    format_version: u32,
    /// v1 keys — each carrying `index_root` (now `source_root` via
    /// `#[serde(alias)]` on `WorkspaceKey`). Deserialising into the v2
    /// `WorkspaceKey` here does the field-rename + `workspace_id =
    /// None` injection in one step.
    keys: Vec<WorkspaceKey>,
}

/// Errors surfaced by the persisted-state loader.
#[derive(Debug, thiserror::Error)]
pub enum PersistedStateError {
    /// I/O failure reading the state file.
    #[error("failed to read persisted state from {path}: {source}", path = path.display())]
    Io {
        /// File path the daemon attempted to read.
        path: std::path::PathBuf,
        /// Underlying transport error.
        #[source]
        source: std::io::Error,
    },
    /// JSON parse failure (top-level shape did not match either schema).
    #[error("failed to parse persisted state JSON: {0}")]
    Parse(#[from] serde_json::Error),
    /// `format_version` was present but greater than
    /// [`PersistedState::FORMAT_VERSION`] — the daemon refuses to
    /// downgrade-parse a forward-version state file.
    #[error("persisted state format_version {found} is newer than supported {supported}")]
    UnsupportedFutureVersion {
        /// Version we read off disk.
        found: u32,
        /// Highest version this daemon understands.
        supported: u32,
    },
}

/// Load a persisted-state payload from disk, upconverting v1 → v2 on
/// the fly. The returned [`PersistedState::format_version`] always
/// equals [`PersistedState::FORMAT_VERSION`] (v2) — callers can rely on
/// the v2 invariants regardless of the on-disk layout.
///
/// # Errors
///
/// - [`PersistedStateError::Io`] for file-system failures.
/// - [`PersistedStateError::Parse`] when the payload is not valid JSON
///   or matches neither v1 nor v2.
/// - [`PersistedStateError::UnsupportedFutureVersion`] when the on-disk
///   `format_version` is greater than [`PersistedState::FORMAT_VERSION`]
///   (`> 2`).
pub fn load_persisted_state(path: &Path) -> Result<PersistedState, PersistedStateError> {
    let bytes = std::fs::read(path).map_err(|source| PersistedStateError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_persisted_state(&bytes)
}

/// Parse a persisted-state payload from raw JSON bytes. Split out
/// from [`load_persisted_state`] so unit tests can drive the
/// upconverter without writing a temp file.
///
/// # Errors
///
/// - [`PersistedStateError::Parse`] when the payload is not valid JSON
///   or matches neither the v1 nor v2 schema.
/// - [`PersistedStateError::UnsupportedFutureVersion`] when the on-disk
///   `format_version` exceeds [`PersistedState::FORMAT_VERSION`].
pub fn parse_persisted_state(bytes: &[u8]) -> Result<PersistedState, PersistedStateError> {
    // Peek the version field first. We do this with a minimal struct
    // so a v1 payload that has been augmented with experimental fields
    // future-versions might add does not break the dispatch.
    #[derive(Deserialize)]
    struct VersionPeek {
        format_version: Option<u32>,
    }
    let peek: VersionPeek = serde_json::from_slice(bytes)?;
    let version = peek.format_version.unwrap_or(1); // missing → assume v1

    match version {
        v if v == PersistedState::FORMAT_VERSION => Ok(serde_json::from_slice(bytes)?),
        1 => {
            // v1 → v2 upconvert: the `WorkspaceKey` Deserialize impl
            // already accepts the legacy `index_root` field name (via
            // `#[serde(alias)]`) and defaults `workspace_id` to `None`,
            // so deserialising the inner `keys` array directly into the
            // v2 `WorkspaceKey` shape is the upconvert.
            let v1: PersistedStateV1 = serde_json::from_slice(bytes)?;
            Ok(PersistedState::new_v2(v1.keys))
        }
        future if future > PersistedState::FORMAT_VERSION => {
            Err(PersistedStateError::UnsupportedFutureVersion {
                found: future,
                supported: PersistedState::FORMAT_VERSION,
            })
        }
        // Any other value (0, etc.) is a corrupt file — surface as a
        // generic parse error so the caller can decide whether to
        // delete or re-derive.
        _ => Err(PersistedStateError::Parse(serde::de::Error::invalid_value(
            serde::de::Unexpected::Unsigned(u64::from(version)),
            &"format_version 1 or 2",
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use sqry_core::project::ProjectRootMode;

    #[test]
    fn parse_v2_round_trips() {
        let original = PersistedState::new_v2(vec![WorkspaceKey::new(
            PathBuf::from("/repos/example"),
            ProjectRootMode::GitRoot,
            0xabcd_ef01,
        )]);
        let wire = serde_json::to_vec(&original).expect("serialise v2");
        let back = parse_persisted_state(&wire).expect("parse v2");
        assert_eq!(back, original);
    }

    #[test]
    fn parse_v1_upconverts_to_v2_with_workspace_id_none() {
        // Synthetic v1 payload: `format_version=1`, legacy `index_root`
        // field name, no `workspace_id`.
        let v1 = serde_json::json!({
            "format_version": 1,
            "keys": [
                {
                    "index_root": "/repos/example",
                    "root_mode": "gitRoot",
                    "config_fingerprint": 0
                }
            ]
        });
        let wire = serde_json::to_vec(&v1).unwrap();
        let upconverted = parse_persisted_state(&wire).expect("upconvert v1");
        assert_eq!(upconverted.format_version, PersistedState::FORMAT_VERSION);
        assert_eq!(upconverted.keys.len(), 1);
        assert!(
            upconverted.keys[0].workspace_id.is_none(),
            "v1 upconvert must inject workspace_id = None"
        );
        assert_eq!(
            upconverted.keys[0].source_root,
            PathBuf::from("/repos/example")
        );
    }

    #[test]
    fn parse_unsupported_future_version_errors() {
        let future = serde_json::json!({
            "format_version": 99,
            "keys": []
        });
        let wire = serde_json::to_vec(&future).unwrap();
        let err = parse_persisted_state(&wire).expect_err("must reject future version");
        assert!(matches!(
            err,
            PersistedStateError::UnsupportedFutureVersion {
                found: 99,
                supported: 2
            }
        ));
    }
}
