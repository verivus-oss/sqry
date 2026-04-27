//! Cross-surface integration test scaffolding for the
//! workspace-aware-cross-repo workstream (STEP_11).
//!
//! This crate intentionally exposes a **single helper module**
//! (`fixtures`) that constructs realistic on-disk
//! `LogicalWorkspace` fixtures, then leaves all observable behaviour
//! to the `tests/*.rs` integration test files. There is no library
//! API to call from outside this crate.
//!
//! See:
//! - `tests/runtime_path_parity.rs` — exercises the SAME logical
//!   workspace through CLI, LSP, MCP, daemon, and VS Code extension
//!   classifier (stubbed) and asserts identical Source/Member/Excluded
//!   classification across all five surfaces.
//! - `tests/multi_root_workspace.rs` — multi-root workspace fixture
//!   (two source roots, one excluded folder) and asserts the
//!   classification matches what the LSP / daemon / MCP-redaction
//!   surfaces produce.
//! - `tests/operational_folder_regression.rs` — the
//!   "no-index-found-for-member-folder" regression that motivated
//!   this workstream. Synthesizes 2 source roots + 1 operational
//!   member + 1 excluded folder; asserts ZERO `No index found` log
//!   lines for member folders and exactly ONE aggregate prompt for
//!   source roots without an index.

pub mod fixtures;
