//! Binding-plane-aware post-filter for [`crate::queries::UnusedQuery`] results.
//!
//! This filter is applied at user-facing command boundaries only. It is not
//! part of [`crate::queries::UnusedQuery`] or [`crate::queries::UnusedKey`],
//! because the cached query must stay graph-exact for topology tests and shared
//! planner semantics.
//!
//! # Suppression rule
//!
//! Suppress a raw unused node `X` only when there is a reachable peer `Y` where:
//!
//! 1. `X.file_id == Y.file_id`.
//! 2. `X.kind == Y.kind`.
//! 3. `simple_name(X) == simple_name(Y)`.
//! 4. `Y` looks like a bare-name phantom: unqualified `qualified_name` and no
//!    visibility.
//! 5. `X` looks like a real definition: qualified `qualified_name`.
//! 6. `qualified_name(X) != qualified_name(Y)`.

use std::collections::HashMap;

use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::file::FileId;
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;

use crate::QueryDb;
use crate::queries::ReachableFromEntryPointsQuery;

fn qualified_name_parts(qualified_name: &str) -> (bool, &str) {
    let separators: [(&str, usize); 3] = [("::", 2), (".", 1), ("/", 1)];
    let mut best_split: Option<usize> = None;

    for (separator, separator_len) in separators {
        if let Some(index) = qualified_name.rfind(separator) {
            let split = index + separator_len;
            best_split = Some(match best_split {
                Some(previous) if previous > split => previous,
                _ => split,
            });
        }
    }

    match best_split {
        Some(split) if split < qualified_name.len() => (true, &qualified_name[split..]),
        _ => (false, qualified_name),
    }
}

#[derive(Debug, Clone)]
struct ProbeAttrs {
    file_id: FileId,
    kind: NodeKind,
    qualified_name: String,
    simple_name: String,
    is_qualified: bool,
    has_no_visibility: bool,
}

fn attrs_for(node_id: NodeId, snapshot: &GraphSnapshot) -> Option<ProbeAttrs> {
    let entry = snapshot.nodes().get(node_id)?;
    let qualified_name = entry
        .qualified_name
        .and_then(|string_id| snapshot.strings().resolve(string_id))?;
    let qualified_name = qualified_name.as_ref().to_string();
    let (is_qualified, simple_name) = qualified_name_parts(&qualified_name);
    let simple_name = simple_name.to_string();

    Some(ProbeAttrs {
        file_id: entry.file,
        kind: entry.kind,
        qualified_name,
        simple_name,
        is_qualified,
        has_no_visibility: entry.visibility.is_none(),
    })
}

/// Apply the binding-plane post-filter to a raw [`crate::queries::UnusedQuery`]
/// result.
///
/// The function is pure from the caller's perspective: it reads `snapshot`,
/// asks `db` for the already-derived entry-point reachable set, and returns a
/// filtered copy of `raw` without mutating query keys, caches, or graph state.
#[must_use]
pub fn apply_binding_plane_post_filter(
    raw: &[NodeId],
    snapshot: &GraphSnapshot,
    db: &QueryDb,
) -> Vec<NodeId> {
    type PhantomKey = (FileId, NodeKind, String);

    let reachable = db.get::<ReachableFromEntryPointsQuery>(&());
    let mut phantom_peers: HashMap<PhantomKey, Vec<String>> = HashMap::new();

    for node_id in reachable.iter().copied() {
        let Some(attrs) = attrs_for(node_id, snapshot) else {
            continue;
        };
        if attrs.is_qualified || !attrs.has_no_visibility {
            continue;
        }
        phantom_peers
            .entry((attrs.file_id, attrs.kind, attrs.simple_name.clone()))
            .or_default()
            .push(attrs.qualified_name);
    }

    raw.iter()
        .copied()
        .filter(|node_id| {
            let Some(attrs) = attrs_for(*node_id, snapshot) else {
                return true;
            };
            if !attrs.is_qualified {
                return true;
            }

            let key = (attrs.file_id, attrs.kind, attrs.simple_name.clone());
            let Some(candidate_names) = phantom_peers.get(&key) else {
                return true;
            };

            !candidate_names
                .iter()
                .any(|peer_name| peer_name != &attrs.qualified_name)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::qualified_name_parts;

    #[test]
    fn rust_path_uses_double_colon_tail() {
        assert_eq!(qualified_name_parts("mod_a::helper"), (true, "helper"));
        assert_eq!(qualified_name_parts("a::b::c::deep"), (true, "deep"));
    }

    #[test]
    fn dotted_path_uses_dot_tail() {
        assert_eq!(qualified_name_parts("pkg.mod.fn"), (true, "fn"));
    }

    #[test]
    fn slash_path_uses_slash_tail() {
        assert_eq!(qualified_name_parts("src/lib/util"), (true, "util"));
    }

    #[test]
    fn bare_name_is_unqualified() {
        assert_eq!(qualified_name_parts("helper"), (false, "helper"));
    }

    #[test]
    fn nearest_tail_separator_wins() {
        assert_eq!(qualified_name_parts("mod::ns.fn"), (true, "fn"));
    }

    #[test]
    fn trailing_separator_is_not_a_valid_tail() {
        assert_eq!(qualified_name_parts("mod::"), (false, "mod::"));
    }
}
