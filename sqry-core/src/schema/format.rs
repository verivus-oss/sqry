//! Canonical output format enumeration.
//!
//! Defines output formats for graph exports and visualizations.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Output formats for graph exports and visualizations.
///
/// Used by `export_graph` and visualization tools to specify
/// the desired output format.
///
/// # Serialization
///
/// All variants serialize to lowercase: `"json"`, `"dot"`, etc.
///
/// # Examples
///
/// ```
/// use sqry_core::schema::OutputFormat;
///
/// let fmt = OutputFormat::Mermaid;
/// assert_eq!(fmt.as_str(), "mermaid");
///
/// let parsed = OutputFormat::parse("dot").unwrap();
/// assert_eq!(parsed, OutputFormat::Dot);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum OutputFormat {
    /// JSON format (default).
    ///
    /// Structured JSON with nodes, edges, and metadata.
    /// Best for programmatic consumption and further processing.
    #[default]
    Json,

    /// Graphviz DOT format.
    ///
    /// Standard graph description language for Graphviz tools.
    /// Render with: `dot -Tpng graph.dot -o graph.png`
    Dot,

    /// D2 diagram format.
    ///
    /// Modern declarative diagramming language.
    /// Render with: `d2 graph.d2 graph.svg`
    D2,

    /// Mermaid diagram format.
    ///
    /// Markdown-friendly diagram syntax.
    /// Renders in GitHub, GitLab, and many documentation tools.
    Mermaid,
}

impl OutputFormat {
    /// Returns all variants in definition order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::Json, Self::Dot, Self::D2, Self::Mermaid]
    }

    /// Returns the canonical string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Dot => "dot",
            Self::D2 => "d2",
            Self::Mermaid => "mermaid",
        }
    }

    /// Returns the typical file extension for this format.
    #[must_use]
    pub const fn file_extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Dot => "dot",
            Self::D2 => "d2",
            Self::Mermaid => "mmd",
        }
    }

    /// Parses a string into an `OutputFormat`.
    ///
    /// Returns `None` if the string doesn't match any known format.
    /// Case-insensitive.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "json" => Some(Self::Json),
            "dot" | "graphviz" => Some(Self::Dot),
            "d2" => Some(Self::D2),
            "mermaid" | "mmd" => Some(Self::Mermaid),
            _ => None,
        }
    }

    /// Returns `true` if this format produces text output.
    ///
    /// All current formats are text-based; this is for future
    /// binary format support (e.g., PNG, SVG).
    #[must_use]
    pub const fn is_text_format(self) -> bool {
        true
    }

    /// Returns `true` if this format is a diagram/visualization format.
    #[must_use]
    pub const fn is_diagram_format(self) -> bool {
        matches!(self, Self::Dot | Self::D2 | Self::Mermaid)
    }
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_as_str() {
        assert_eq!(OutputFormat::Json.as_str(), "json");
        assert_eq!(OutputFormat::Dot.as_str(), "dot");
        assert_eq!(OutputFormat::D2.as_str(), "d2");
        assert_eq!(OutputFormat::Mermaid.as_str(), "mermaid");
    }

    #[test]
    fn test_file_extension() {
        assert_eq!(OutputFormat::Json.file_extension(), "json");
        assert_eq!(OutputFormat::Dot.file_extension(), "dot");
        assert_eq!(OutputFormat::D2.file_extension(), "d2");
        assert_eq!(OutputFormat::Mermaid.file_extension(), "mmd");
    }

    #[test]
    fn test_parse() {
        assert_eq!(OutputFormat::parse("json"), Some(OutputFormat::Json));
        assert_eq!(OutputFormat::parse("DOT"), Some(OutputFormat::Dot));
        assert_eq!(OutputFormat::parse("graphviz"), Some(OutputFormat::Dot));
        assert_eq!(OutputFormat::parse("mermaid"), Some(OutputFormat::Mermaid));
        assert_eq!(OutputFormat::parse("mmd"), Some(OutputFormat::Mermaid));
        assert_eq!(OutputFormat::parse("unknown"), None);
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", OutputFormat::Json), "json");
        assert_eq!(format!("{}", OutputFormat::Mermaid), "mermaid");
    }

    #[test]
    fn test_serde_roundtrip() {
        for fmt in OutputFormat::all() {
            let json = serde_json::to_string(fmt).unwrap();
            let deserialized: OutputFormat = serde_json::from_str(&json).unwrap();
            assert_eq!(*fmt, deserialized);
        }
    }

    #[test]
    fn test_classification() {
        assert!(OutputFormat::Json.is_text_format());
        assert!(!OutputFormat::Json.is_diagram_format());
        assert!(OutputFormat::Dot.is_diagram_format());
        assert!(OutputFormat::Mermaid.is_diagram_format());
    }

    #[test]
    fn test_default() {
        assert_eq!(OutputFormat::default(), OutputFormat::Json);
    }
}
