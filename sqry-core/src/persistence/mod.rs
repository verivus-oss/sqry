//! On-disk persistence helpers for sqry.
//!
//! This module provides low-level, filesystem-oriented utilities that are
//! shared across the sqry persistence stack (graph snapshots, derived-cache
//! manifests, etc.).
//!
//! # Modules
//!
//! - [`atomic_write`]: canonical atomic-write primitive —
//!   tempfile-in-same-directory + fsync + rename + parent-dir fsync on Unix.
//! - [`path_safety`]: workspace-containment and no-symlink validation for
//!   all persistence read/write paths.

pub mod atomic_write;
pub mod path_safety;

pub use atomic_write::atomic_write_bytes;
pub use path_safety::{PathSafetyError, validate_path_in_workspace};
