//! Redaction rules for different types of sensitive data.
//!
//! This module contains the core logic for redacting paths, URIs, code content,
//! and other sensitive fields from MCP responses.

pub mod content;
pub mod path;
pub mod pattern;
pub mod uri;

pub use content::{redact_code_context, redact_documentation};
pub use path::{CanonicalPath, PathType, canonicalize_for_hash, hash_path};
pub use pattern::detect_paths_in_string;
pub use uri::parse_file_uri;
