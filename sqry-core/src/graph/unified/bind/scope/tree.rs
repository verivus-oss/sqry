//! Scope chain traversal helpers — `scope_chain(arena, start)` returns the
//! ordered chain of enclosing scopes from innermost to outermost.

use super::arena::{ScopeArena, ScopeId};

/// Returns the chain of enclosing scopes in innermost-first order.
///
/// The chain terminates at the outermost arena-addressable scope; it does
/// **not** include a pseudo "file scope" entry because files are not scopes.
/// Callers that want the containing file read `scope(outermost).file`.
///
/// Returns an empty `Vec` if `start` is `ScopeId::INVALID` or if `start`
/// does not exist in the arena.
#[must_use]
pub fn scope_chain(arena: &ScopeArena, start: ScopeId) -> Vec<ScopeId> {
    let mut chain = Vec::new();
    let mut current = start;
    while !current.is_invalid() {
        let Some(scope) = arena.get(current) else {
            break;
        };
        chain.push(current);
        current = scope.parent;
    }
    chain
}
