//! Proc-macro crate detection for Rust.
//!
//! This module detects whether a Rust crate is a proc-macro crate,
//! which affects how symbols are extracted and classified.
//!
//! # Detection Hierarchy
//!
//! 1. `Cargo.toml` with `[lib] proc-macro = true` (preferred)
//! 2. Crate-level attribute `#![crate_type = "proc-macro"]` (fallback)
//! 3. Per-function attributes with confidence limitation
//!
//! # Security Note
//!
//! Proc macros can execute arbitrary code during compilation.
//! Detection does NOT execute any code - it only reads metadata.

use crate::confidence::ConfidenceTracker;
use std::fs;
use std::path::{Path, PathBuf};

/// Source of proc-macro detection for confidence tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcMacroDetectionSource {
    /// Detected from `Cargo.toml` `[lib] proc-macro = true`
    CargoToml,
    /// Detected from `#![crate_type = "proc-macro"]`
    CrateAttribute,
    /// Per-function heuristic (attribute only, no crate context)
    FunctionAttributeOnly,
    /// Unable to determine
    Unknown,
}

impl ProcMacroDetectionSource {
    /// Returns a human-readable string for the detection source.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CargoToml => "cargo_toml",
            Self::CrateAttribute => "crate_attribute",
            Self::FunctionAttributeOnly => "function_attribute_only",
            Self::Unknown => "unknown",
        }
    }

    /// Returns true if this is a confident detection.
    #[must_use]
    pub const fn is_confident(self) -> bool {
        matches!(self, Self::CargoToml | Self::CrateAttribute)
    }
}

/// Proc-macro crate detection result.
#[derive(Debug, Clone)]
pub struct ProcMacroDetector {
    /// Whether this is a proc-macro crate (from Cargo.toml or crate attribute)
    is_proc_macro_crate: Option<bool>,
    /// Detection source for confidence tracking
    detection_source: ProcMacroDetectionSource,
    /// Path to Cargo.toml if found
    cargo_toml_path: Option<PathBuf>,
}

impl Default for ProcMacroDetector {
    fn default() -> Self {
        Self {
            is_proc_macro_crate: None,
            detection_source: ProcMacroDetectionSource::Unknown,
            cargo_toml_path: None,
        }
    }
}

impl ProcMacroDetector {
    /// Create a new detector for a file in the given workspace.
    ///
    /// This searches for `Cargo.toml` and crate-level attributes.
    #[must_use]
    pub fn detect(workspace_root: &Path, file_path: &Path) -> Self {
        // 1. Try Cargo.toml
        if let Some((is_proc_macro, cargo_path)) = Self::check_cargo_toml(workspace_root, file_path)
        {
            return Self {
                is_proc_macro_crate: Some(is_proc_macro),
                detection_source: ProcMacroDetectionSource::CargoToml,
                cargo_toml_path: Some(cargo_path),
            };
        }

        // 2. Try crate-level attribute (parse lib.rs or main.rs root)
        if let Some(is_proc_macro) = Self::check_crate_attribute(workspace_root, file_path) {
            return Self {
                is_proc_macro_crate: Some(is_proc_macro),
                detection_source: ProcMacroDetectionSource::CrateAttribute,
                cargo_toml_path: None,
            };
        }

        // 3. Fallback: unknown crate type
        Self {
            is_proc_macro_crate: None,
            detection_source: ProcMacroDetectionSource::Unknown,
            cargo_toml_path: None,
        }
    }

    /// Create a detector that's known to be a proc-macro crate.
    #[must_use]
    pub fn proc_macro() -> Self {
        Self {
            is_proc_macro_crate: Some(true),
            detection_source: ProcMacroDetectionSource::CargoToml,
            cargo_toml_path: None,
        }
    }

    /// Create a detector that's known to NOT be a proc-macro crate.
    #[must_use]
    pub fn not_proc_macro() -> Self {
        Self {
            is_proc_macro_crate: Some(false),
            detection_source: ProcMacroDetectionSource::CargoToml,
            cargo_toml_path: None,
        }
    }

    /// Get whether this is a proc-macro crate.
    #[must_use]
    pub fn is_proc_macro_crate(&self) -> Option<bool> {
        self.is_proc_macro_crate
    }

    /// Get the detection source.
    #[must_use]
    pub fn detection_source(&self) -> ProcMacroDetectionSource {
        self.detection_source
    }

    /// Get the path to Cargo.toml if found.
    #[must_use]
    pub fn cargo_toml_path(&self) -> Option<&Path> {
        self.cargo_toml_path.as_deref()
    }

    /// Check Cargo.toml for `[lib] proc-macro = true`.
    ///
    /// Returns `(is_proc_macro, cargo_toml_path)` if Cargo.toml was found.
    fn check_cargo_toml(workspace_root: &Path, file_path: &Path) -> Option<(bool, PathBuf)> {
        // Search upward from the file's directory to find Cargo.toml
        let mut current = file_path.parent()?;

        while current.starts_with(workspace_root) || current == workspace_root {
            let cargo_path = current.join("Cargo.toml");
            if cargo_path.exists() {
                let content = fs::read_to_string(&cargo_path).ok()?;
                let is_proc_macro = Self::parse_cargo_toml_for_proc_macro(&content);
                return Some((is_proc_macro, cargo_path));
            }
            current = current.parent()?;
        }

        None
    }

    /// Parse Cargo.toml content for `proc-macro = true`.
    fn parse_cargo_toml_for_proc_macro(content: &str) -> bool {
        // Simple parsing - look for [lib] section with proc-macro = true
        // A proper implementation would use toml crate, but this is sufficient
        // for detection purposes.

        let mut in_lib_section = false;

        for line in content.lines() {
            let line = line.trim();

            // Check for section headers
            if line.starts_with('[') {
                in_lib_section = line == "[lib]";
                continue;
            }

            // In [lib] section, check for proc-macro = true
            if in_lib_section {
                let normalized = line.replace(' ', "");
                if normalized == "proc-macro=true" || normalized == "proc_macro=true" {
                    return true;
                }
            }
        }

        false
    }

    /// Check for crate-level `#![crate_type = "proc-macro"]` attribute.
    fn check_crate_attribute(workspace_root: &Path, file_path: &Path) -> Option<bool> {
        // Find crate root (lib.rs or main.rs in src/)
        let crate_root = Self::find_crate_root(workspace_root, file_path)?;
        let content = fs::read_to_string(&crate_root).ok()?;

        // Look for #![crate_type = "proc-macro"]
        if content.contains(r#"#![crate_type = "proc-macro"]"#)
            || content.contains(r#"#![crate_type="proc-macro"]"#)
        {
            return Some(true);
        }

        None
    }

    /// Find the crate root file (lib.rs or main.rs).
    fn find_crate_root(workspace_root: &Path, file_path: &Path) -> Option<PathBuf> {
        // Search upward from the file to find src/lib.rs or src/main.rs
        let mut current = file_path.parent()?;

        while current.starts_with(workspace_root) || current == workspace_root {
            // Check if we're in a src directory
            if current.file_name().is_some_and(|name| name == "src") {
                let lib_rs = current.join("lib.rs");
                if lib_rs.exists() {
                    return Some(lib_rs);
                }

                let main_rs = current.join("main.rs");
                if main_rs.exists() {
                    return Some(main_rs);
                }
            }

            // Check if parent is the crate root (has src/lib.rs or src/main.rs)
            let src_dir = current.join("src");
            if src_dir.exists() {
                let lib_rs = src_dir.join("lib.rs");
                if lib_rs.exists() {
                    return Some(lib_rs);
                }

                let main_rs = src_dir.join("main.rs");
                if main_rs.exists() {
                    return Some(main_rs);
                }
            }

            current = current.parent()?;
        }

        None
    }

    /// Determine if a function should be extracted as a macro, with confidence impact.
    ///
    /// Returns `(should_extract, limitation_message)`
    pub fn should_extract_as_macro(
        &self,
        has_proc_macro_attr: bool,
        confidence: &mut ConfidenceTracker,
    ) -> bool {
        match (self.is_proc_macro_crate, has_proc_macro_attr) {
            // Confident: proc-macro crate with proc-macro attribute
            (Some(true), true) => true,

            // Contradiction: attribute on non-proc-macro crate
            (Some(false), true) => {
                confidence.add_limitation(
                    "Proc-macro attribute found on non-proc-macro crate; may be helper function",
                );
                false
            }

            // Low confidence: attribute only, no crate context
            (None, true) => {
                confidence.add_limitation(
                    "Proc-macro detection based on attribute only; crate type unknown",
                );
                true
            }

            // No attribute, not a macro
            (_, false) => false,
        }
    }

    /// Check if a function has a proc-macro attribute.
    ///
    /// Looks for `#[proc_macro]`, `#[proc_macro_derive]`, or `#[proc_macro_attribute]`.
    #[must_use]
    pub fn has_proc_macro_attribute(attributes: &str) -> bool {
        attributes.contains("#[proc_macro]")
            || attributes.contains("#[proc_macro_derive")
            || attributes.contains("#[proc_macro_attribute")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_cargo_toml(dir: &Path, content: &str) {
        let cargo_path = dir.join("Cargo.toml");
        let mut file = fs::File::create(&cargo_path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
    }

    fn create_file(dir: &Path, relative_path: &str, content: &str) {
        let path = dir.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn test_proc_macro_detection_from_cargo_toml() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_cargo_toml(
            root,
            r#"
[package]
name = "my_proc_macro"
version = "0.1.0"

[lib]
proc-macro = true
"#,
        );
        create_file(root, "src/lib.rs", "// lib");

        let file_path = root.join("src/lib.rs");
        let detector = ProcMacroDetector::detect(root, &file_path);

        assert_eq!(detector.is_proc_macro_crate(), Some(true));
        assert_eq!(
            detector.detection_source(),
            ProcMacroDetectionSource::CargoToml
        );
    }

    #[test]
    fn test_non_proc_macro_crate() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_cargo_toml(
            root,
            r#"
[package]
name = "regular_crate"
version = "0.1.0"
"#,
        );
        create_file(root, "src/lib.rs", "// lib");

        let file_path = root.join("src/lib.rs");
        let detector = ProcMacroDetector::detect(root, &file_path);

        assert_eq!(detector.is_proc_macro_crate(), Some(false));
        assert_eq!(
            detector.detection_source(),
            ProcMacroDetectionSource::CargoToml
        );
    }

    #[test]
    fn test_proc_macro_from_crate_attribute() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // No Cargo.toml
        create_file(
            root,
            "src/lib.rs",
            r#"#![crate_type = "proc-macro"]
pub fn my_macro() {}
"#,
        );

        let file_path = root.join("src/lib.rs");
        let detector = ProcMacroDetector::detect(root, &file_path);

        assert_eq!(detector.is_proc_macro_crate(), Some(true));
        assert_eq!(
            detector.detection_source(),
            ProcMacroDetectionSource::CrateAttribute
        );
    }

    #[test]
    fn test_unknown_crate_type() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // No Cargo.toml, no crate attribute
        create_file(root, "src/foo.rs", "fn foo() {}");

        let file_path = root.join("src/foo.rs");
        let detector = ProcMacroDetector::detect(root, &file_path);

        assert_eq!(detector.is_proc_macro_crate(), None);
        assert_eq!(
            detector.detection_source(),
            ProcMacroDetectionSource::Unknown
        );
    }

    #[test]
    fn test_has_proc_macro_attribute() {
        assert!(ProcMacroDetector::has_proc_macro_attribute("#[proc_macro]"));
        assert!(ProcMacroDetector::has_proc_macro_attribute(
            "#[proc_macro_derive(MyDerive)]"
        ));
        assert!(ProcMacroDetector::has_proc_macro_attribute(
            "#[proc_macro_attribute]"
        ));
        assert!(!ProcMacroDetector::has_proc_macro_attribute("#[derive]"));
        assert!(!ProcMacroDetector::has_proc_macro_attribute("fn foo() {}"));
    }

    #[test]
    fn test_should_extract_as_macro() {
        let mut confidence = ConfidenceTracker::default();

        // Confident extraction
        let detector = ProcMacroDetector::proc_macro();
        assert!(detector.should_extract_as_macro(true, &mut confidence));
        assert_eq!(confidence.limitation_count(), 0);

        // Low confidence - attribute only
        let detector = ProcMacroDetector::default();
        let mut confidence2 = ConfidenceTracker::default();
        assert!(detector.should_extract_as_macro(true, &mut confidence2));
        assert_eq!(confidence2.limitation_count(), 1);

        // Not a macro
        let detector = ProcMacroDetector::not_proc_macro();
        let mut confidence3 = ConfidenceTracker::default();
        assert!(!detector.should_extract_as_macro(false, &mut confidence3));
    }

    #[test]
    fn test_detection_source_is_confident() {
        assert!(ProcMacroDetectionSource::CargoToml.is_confident());
        assert!(ProcMacroDetectionSource::CrateAttribute.is_confident());
        assert!(!ProcMacroDetectionSource::FunctionAttributeOnly.is_confident());
        assert!(!ProcMacroDetectionSource::Unknown.is_confident());
    }

    #[test]
    fn test_parse_cargo_toml_variations() {
        // Standard format
        assert!(ProcMacroDetector::parse_cargo_toml_for_proc_macro(
            "[lib]\nproc-macro = true"
        ));

        // Underscore variant
        assert!(ProcMacroDetector::parse_cargo_toml_for_proc_macro(
            "[lib]\nproc_macro = true"
        ));

        // With spaces
        assert!(ProcMacroDetector::parse_cargo_toml_for_proc_macro(
            "[lib]\nproc-macro  =  true"
        ));

        // Not in [lib] section
        assert!(!ProcMacroDetector::parse_cargo_toml_for_proc_macro(
            "[package]\nproc-macro = true"
        ));

        // False value
        assert!(!ProcMacroDetector::parse_cargo_toml_for_proc_macro(
            "[lib]\nproc-macro = false"
        ));
    }
}
