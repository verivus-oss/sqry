//! Witness rendering — `WitnessRendering { text, json }` plus a stable
//! renderer that turns an ordered step trace into human-readable text and
//! a deterministic JSON shape.
//!
//! # Stable JSON schema
//!
//! The JSON shape produced by `render_witness` is the external contract for
//! the CLI `--explain` output in P2U10. Changes to keys or value types are a
//! breaking public-API change.
//!
//! ```json
//! {
//!   "outcome":          <serde_json::Value for SymbolResolutionOutcome>,
//!   "selected_bucket":  <serde_json::Value for SymbolCandidateBucket> | null,
//!   "candidate_count":  <usize>,
//!   "steps":            [<serde_json::Value per ResolutionStep>]
//! }
//! ```

use serde::{Deserialize, Serialize};

use crate::graph::unified::resolution::SymbolResolutionWitness;

use super::step::ResolutionStep;

/// Structured rendering of an ordered step trace.
///
/// `text` is a human-readable numbered list, one line per step.
/// `json` is a deterministic `serde_json::Value` whose schema is the stable
/// external contract for the P2U10 CLI proof point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitnessRendering {
    /// Human-readable numbered step list. Each line has the form
    /// `"  N. <Display repr of step>\n"`.
    pub text: String,
    /// Deterministic JSON representation.
    pub json: serde_json::Value,
}

/// Renders a [`SymbolResolutionWitness`] as a [`WitnessRendering`].
///
/// This is the canonical entry point for the `explain()` facade method on
/// [`crate::graph::unified::bind::plane::BindingPlane`].
#[must_use]
pub fn render_witness(witness: &SymbolResolutionWitness) -> WitnessRendering {
    let text = render_text(&witness.steps);
    let json = render_json(witness);
    WitnessRendering { text, json }
}

/// Builds the human-readable step list: one numbered line per step.
fn render_text(steps: &[ResolutionStep]) -> String {
    use std::fmt::Write as _;

    let mut buf = String::new();
    for (idx, step) in steps.iter().enumerate() {
        let _ = writeln!(buf, "{:3}. {step}", idx + 1);
    }
    buf
}

/// Builds the stable JSON representation.
///
/// Every `ResolutionStep` variant is `Serialize` (added in P2U06), so the
/// steps array is deterministic. The outer object fields mirror the
/// `SymbolResolutionWitness` public fields that callers care about.
fn render_json(witness: &SymbolResolutionWitness) -> serde_json::Value {
    serde_json::json!({
        "outcome": serde_json::to_value(&witness.outcome).unwrap_or(serde_json::Value::Null),
        "selected_bucket": witness
            .selected_bucket
            .as_ref()
            .map(|b| serde_json::to_value(b).unwrap_or(serde_json::Value::Null)),
        "candidate_count": witness.candidates.len(),
        "steps": witness.steps.iter().map(|s| {
            serde_json::to_value(s).unwrap_or(serde_json::Value::Null)
        }).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::resolution::SymbolResolutionOutcome;

    #[test]
    fn empty_trace_renders_cleanly() {
        let witness = SymbolResolutionWitness {
            normalized_query: None,
            outcome: SymbolResolutionOutcome::NotFound,
            selected_bucket: None,
            candidates: Vec::new(),
            symbol: None,
            steps: Vec::new(),
        };
        let rendering = render_witness(&witness);
        assert_eq!(
            rendering.text, "",
            "empty step list must render as empty string"
        );
        assert_eq!(
            rendering.json["steps"].as_array().unwrap().len(),
            0,
            "JSON steps array must be empty"
        );
        assert_eq!(rendering.json["candidate_count"], 0);
        assert!(rendering.json["selected_bucket"].is_null());
    }

    #[test]
    fn single_step_text_is_numbered() {
        use crate::graph::unified::file::id::FileId;
        let witness = SymbolResolutionWitness {
            normalized_query: None,
            outcome: SymbolResolutionOutcome::NotFound,
            selected_bucket: None,
            candidates: Vec::new(),
            symbol: None,
            steps: vec![ResolutionStep::EnterFileScope {
                file: FileId::new(7),
            }],
        };
        let rendering = render_witness(&witness);
        assert!(
            rendering.text.starts_with("  1. enter file scope"),
            "first step must start with '  1. enter file scope' (Display output), got: {:?}",
            rendering.text
        );
    }

    #[test]
    fn multi_step_text_numbering_is_sequential() {
        use crate::graph::unified::resolution::SymbolCandidateBucket;
        let witness = SymbolResolutionWitness {
            normalized_query: None,
            outcome: SymbolResolutionOutcome::NotFound,
            selected_bucket: None,
            candidates: Vec::new(),
            symbol: None,
            steps: vec![
                ResolutionStep::LookupInBucket {
                    bucket: SymbolCandidateBucket::ExactQualified,
                },
                ResolutionStep::LookupInBucket {
                    bucket: SymbolCandidateBucket::ExactSimple,
                },
                ResolutionStep::Unresolved {
                    symbol: crate::graph::unified::string::id::StringId::new(0),
                    reason: super::super::step::UnresolvedReason::NotInAnyScope,
                },
            ],
        };
        let rendering = render_witness(&witness);
        let lines: Vec<&str> = rendering.text.lines().collect();
        assert_eq!(lines.len(), 3, "expected 3 numbered lines");
        assert!(lines[0].starts_with("  1."));
        assert!(lines[1].starts_with("  2."));
        assert!(lines[2].starts_with("  3."));
    }

    #[test]
    fn json_outcome_field_matches_serde_format() {
        // Unit variants serialize as plain strings under serde, so
        // FileNotIndexed → "FileNotIndexed".
        let witness = SymbolResolutionWitness {
            normalized_query: None,
            outcome: SymbolResolutionOutcome::FileNotIndexed,
            selected_bucket: None,
            candidates: Vec::new(),
            symbol: None,
            steps: Vec::new(),
        };
        let rendering = render_witness(&witness);
        assert_eq!(
            rendering.json["outcome"].as_str().unwrap(),
            "FileNotIndexed"
        );
    }

    #[test]
    fn json_steps_array_has_correct_length() {
        use crate::graph::unified::node::id::NodeId;
        use crate::graph::unified::resolution::SymbolCandidateBucket;
        let witness = SymbolResolutionWitness {
            normalized_query: None,
            outcome: SymbolResolutionOutcome::Resolved(NodeId::new(3, 1)),
            selected_bucket: Some(SymbolCandidateBucket::ExactSimple),
            candidates: Vec::new(),
            symbol: None,
            steps: vec![
                ResolutionStep::LookupInBucket {
                    bucket: SymbolCandidateBucket::ExactSimple,
                },
                ResolutionStep::Chose {
                    node: NodeId::new(3, 1),
                },
            ],
        };
        let rendering = render_witness(&witness);
        assert_eq!(
            rendering.json["steps"].as_array().unwrap().len(),
            2,
            "JSON steps array length must match witness.steps.len()"
        );
        assert_eq!(
            rendering.json["selected_bucket"].as_str().unwrap(),
            "ExactSimple"
        );
        assert_eq!(rendering.json["candidate_count"], 0);
    }

    #[test]
    fn witness_rendering_roundtrips_through_serde_json() {
        use crate::graph::unified::node::id::NodeId;
        let rendering = WitnessRendering {
            text: "  1. Chose { node: 0:1 }\n".to_string(),
            json: serde_json::json!({
                "outcome": "Resolved(0:1)",
                "selected_bucket": null,
                "candidate_count": 1,
                "steps": [],
            }),
        };
        let serialized = serde_json::to_string(&rendering).expect("serialize");
        let deserialized: WitnessRendering =
            serde_json::from_str(&serialized).expect("deserialize");
        assert_eq!(rendering, deserialized);
        let _ = NodeId::new(0, 1); // keep NodeId import alive
    }
}
