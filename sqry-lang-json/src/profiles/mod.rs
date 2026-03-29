//! Format-specific extraction profiles for JSON files.
//!
//! Profiles override default node kinds for recognized JSON file formats:
//! - `now-ui.json`: `components.*` entries become `Component` nodes
//! - `package.json`: `dependencies.*` entries become `Import` nodes with `Imports` edges

use std::path::Path;

use sqry_core::graph::unified::node::NodeKind;

/// Format-specific extraction profile, detected from filename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Profile {
    /// `now-ui.json`: `components.*` entries → `Component` nodes
    NowUi,
    /// `package.json`: `dependencies`/`devDependencies` → `Import` nodes + `Imports` edges
    PackageJson,
    /// All other `.json` files: everything → `Variable` nodes
    Generic,
}

impl Profile {
    /// Detect profile from file path by examining the filename.
    pub(crate) fn detect(path: &Path) -> Self {
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_ascii_lowercase())
            .unwrap_or_default();

        match filename.as_str() {
            "now-ui.json" => Self::NowUi,
            "package.json" => Self::PackageJson,
            _ => Self::Generic,
        }
    }

    /// Determine `NodeKind` for a given key at a specific depth under a parent.
    ///
    /// `parent_key` is the immediate parent key name (if any).
    /// `depth` is the nesting level (0 = top-level, 1 = one level deep, etc.).
    pub(crate) fn node_kind_for(&self, parent_key: Option<&str>, depth: u32) -> NodeKind {
        match self {
            Self::NowUi => {
                if depth == 1 && parent_key == Some("components") {
                    NodeKind::Component
                } else {
                    NodeKind::Variable
                }
            }
            Self::PackageJson => {
                if depth == 1 && matches!(parent_key, Some("dependencies" | "devDependencies")) {
                    NodeKind::Import
                } else {
                    NodeKind::Variable
                }
            }
            Self::Generic => NodeKind::Variable,
        }
    }

    /// Whether this node should get an `Imports` edge from the module.
    pub(crate) fn needs_import_edge(&self, parent_key: Option<&str>, depth: u32) -> bool {
        matches!(self, Self::PackageJson)
            && depth == 1
            && matches!(parent_key, Some("dependencies" | "devDependencies"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_now_ui() {
        assert_eq!(Profile::detect(Path::new("now-ui.json")), Profile::NowUi);
        assert_eq!(
            Profile::detect(Path::new("/path/to/NOW-UI.JSON")),
            Profile::NowUi
        );
    }

    #[test]
    fn test_detect_package_json() {
        assert_eq!(
            Profile::detect(Path::new("package.json")),
            Profile::PackageJson
        );
    }

    #[test]
    fn test_detect_generic() {
        assert_eq!(
            Profile::detect(Path::new("tsconfig.json")),
            Profile::Generic
        );
        assert_eq!(Profile::detect(Path::new("foo.json")), Profile::Generic);
    }

    #[test]
    fn test_now_ui_node_kinds() {
        let profile = Profile::NowUi;
        assert_eq!(
            profile.node_kind_for(Some("components"), 1),
            NodeKind::Component
        );
        assert_eq!(
            profile.node_kind_for(Some("components"), 2),
            NodeKind::Variable
        );
        assert_eq!(profile.node_kind_for(None, 0), NodeKind::Variable);
    }

    #[test]
    fn test_package_json_node_kinds() {
        let profile = Profile::PackageJson;
        assert_eq!(
            profile.node_kind_for(Some("dependencies"), 1),
            NodeKind::Import
        );
        assert_eq!(
            profile.node_kind_for(Some("devDependencies"), 1),
            NodeKind::Import
        );
        assert_eq!(
            profile.node_kind_for(Some("scripts"), 1),
            NodeKind::Variable
        );
        assert_eq!(profile.node_kind_for(None, 0), NodeKind::Variable);
    }

    #[test]
    fn test_needs_import_edge() {
        let pkg = Profile::PackageJson;
        assert!(pkg.needs_import_edge(Some("dependencies"), 1));
        assert!(pkg.needs_import_edge(Some("devDependencies"), 1));
        assert!(!pkg.needs_import_edge(Some("scripts"), 1));
        assert!(!pkg.needs_import_edge(Some("dependencies"), 0));

        assert!(!Profile::NowUi.needs_import_edge(Some("dependencies"), 1));
        assert!(!Profile::Generic.needs_import_edge(Some("dependencies"), 1));
    }
}
