//! Git worktree management - re-exports from sqry-core for DRY.
//!
//! This module re-exports the shared `WorktreeManager` implementation from
//! sqry-core to avoid code duplication between MCP and LSP.

pub use sqry_core::git::WorktreeManager;
