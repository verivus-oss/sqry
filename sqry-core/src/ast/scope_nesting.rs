//! Scope nesting algorithm for establishing parent-child relationships.

use super::scope::{Scope, ScopeId};

/// Link nested scopes by assigning parent IDs and sequential `ScopeIds`.
///
/// Scopes must be pre-sorted by (`start_line`, `start_column`).
///
/// The algorithm:
/// 1. Assigns sequential `ScopeIds` (0, 1, 2, ...) to all scopes
/// 2. Determines parent-child relationships based on containment
///
/// # Panics
///
/// Panics if scopes are not sorted by (`start_line`, `start_column`).
pub fn link_nested_scopes(scopes: &mut [Scope]) {
    // Assign sequential ScopeIds
    for (i, scope) in scopes.iter_mut().enumerate() {
        scope.id = ScopeId::new(i);
    }

    if scopes.is_empty() {
        return;
    }

    // Validate sorted order
    for i in 1..scopes.len() {
        if scopes[i].start_line < scopes[i - 1].start_line
            || (scopes[i].start_line == scopes[i - 1].start_line
                && scopes[i].start_column < scopes[i - 1].start_column)
        {
            panic!(
                "Scopes must be sorted by (start_line, start_column). \
                Scope at index {} starts before index {}",
                i,
                i - 1
            );
        }
    }

    // Stack of indices (which correspond to ScopeIds since ID = index)
    let mut stack: Vec<usize> = Vec::new();

    for i in 0..scopes.len() {
        let scope = &scopes[i];

        // Pop stack until we find a scope that contains this one
        while let Some(&parent_idx) = stack.last() {
            let parent = &scopes[parent_idx];
            if contains_scope(parent, scope) {
                break;
            }
            stack.pop();
        }

        // Set parent if found
        if let Some(&parent_idx) = stack.last() {
            scopes[i].parent_id = Some(ScopeId::new(parent_idx));
        }

        // Push this scope onto stack
        stack.push(i);
    }
}

fn contains_scope(outer: &Scope, inner: &Scope) -> bool {
    (outer.start_line < inner.start_line
        || (outer.start_line == inner.start_line && outer.start_column <= inner.start_column))
        && (outer.end_line > inner.end_line
            || (outer.end_line == inner.end_line && outer.end_column >= inner.end_column))
}

/// Get the parent scope for a given scope.
#[must_use]
pub fn get_parent_scope<'a>(scopes: &'a [Scope], scope: &Scope) -> Option<&'a Scope> {
    scope
        .parent_id
        .and_then(|parent_id| scopes.get(parent_id.0))
}

/// Get all ancestor scopes for a given scope.
#[must_use]
pub fn get_ancestor_scopes<'a>(scopes: &'a [Scope], scope: &Scope) -> Vec<&'a Scope> {
    let mut ancestors = Vec::new();
    let mut current = scope;

    while let Some(parent) = get_parent_scope(scopes, current) {
        ancestors.push(parent);
        current = parent;
    }

    ancestors
}

/// Check if a scope is nested within the given ancestor scope.
#[must_use]
pub fn is_nested_in(scopes: &[Scope], scope: &Scope, ancestor_id: ScopeId) -> bool {
    let mut current = scope;
    while let Some(parent) = get_parent_scope(scopes, current) {
        if parent.id == ancestor_id {
            return true;
        }
        current = parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_scope(
        name: &str,
        start_line: usize,
        start_column: usize,
        end_line: usize,
        end_column: usize,
    ) -> Scope {
        Scope {
            id: ScopeId::new(0),
            scope_type: "function".to_string(),
            name: name.to_string(),
            file_path: PathBuf::from("test.rs"),
            start_line,
            start_column,
            end_line,
            end_column,
            parent_id: None,
        }
    }

    #[test]
    fn test_link_nested_scopes_basic() {
        let mut scopes = vec![
            make_scope("outer", 1, 0, 10, 0),
            make_scope("inner", 2, 0, 5, 0),
        ];

        link_nested_scopes(&mut scopes);

        assert_eq!(scopes[0].id, ScopeId::new(0));
        assert_eq!(scopes[1].id, ScopeId::new(1));
        assert_eq!(scopes[1].parent_id, Some(ScopeId::new(0)));
    }

    #[test]
    fn test_get_ancestor_scopes() {
        let mut scopes = vec![
            make_scope("outer", 1, 0, 10, 0),
            make_scope("inner", 2, 0, 5, 0),
            make_scope("deep", 3, 0, 4, 0),
        ];

        link_nested_scopes(&mut scopes);

        let ancestors = get_ancestor_scopes(&scopes, &scopes[2]);
        assert_eq!(ancestors.len(), 2);
        assert_eq!(ancestors[0].name, "inner");
        assert_eq!(ancestors[1].name, "outer");
    }

    #[test]
    fn test_is_nested_in() {
        let mut scopes = vec![
            make_scope("outer", 1, 0, 10, 0),
            make_scope("inner", 2, 0, 5, 0),
            make_scope("deep", 3, 0, 4, 0),
        ];

        link_nested_scopes(&mut scopes);

        assert!(is_nested_in(&scopes, &scopes[2], ScopeId::new(1)));
        assert!(is_nested_in(&scopes, &scopes[2], ScopeId::new(0)));
        assert!(!is_nested_in(&scopes, &scopes[1], ScopeId::new(2)));
    }
}
