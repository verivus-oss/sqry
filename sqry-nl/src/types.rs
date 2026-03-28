//! Core types for the sqry-nl crate.
//!
//! All types are designed to be `Send + Sync` for thread-safe usage.

use serde::{Deserialize, Serialize};

/// Intent classification result from the NL classifier.
///
/// Each intent maps to a specific sqry command template:
/// - `SymbolQuery` → `sqry query "<expr>"`
/// - `TextSearch` → `sqry search "<pattern>"`
/// - `TracePath` → `sqry graph trace-path "<from>" "<to>"`
/// - `FindCallers` → `sqry graph direct-callers "<symbol>"`
/// - `FindCallees` → `sqry graph direct-callees "<symbol>"`
/// - `Visualize` → `sqry visualize --relation <kind> --symbol "<name>"`
/// - `IndexStatus` → `sqry index --status`
/// - `Ambiguous` → Cannot determine intent, needs clarification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// Search for symbols by name, kind, or pattern
    SymbolQuery,
    /// Text/grep search for patterns in code
    TextSearch,
    /// Trace call path between two symbols
    TracePath,
    /// Find all callers of a symbol
    FindCallers,
    /// Find all callees of a symbol
    FindCallees,
    /// Generate visualization (Mermaid/DOT)
    Visualize,
    /// Check index status
    IndexStatus,
    /// Intent unclear, need disambiguation
    Ambiguous,
}

impl Intent {
    /// Number of intent classes
    pub const NUM_CLASSES: usize = 8;

    /// Convert from classifier output index
    #[must_use]
    pub fn from_index(idx: usize) -> Self {
        match idx {
            0 => Self::SymbolQuery,
            1 => Self::TextSearch,
            2 => Self::TracePath,
            3 => Self::FindCallers,
            4 => Self::FindCallees,
            5 => Self::Visualize,
            6 => Self::IndexStatus,
            _ => Self::Ambiguous,
        }
    }

    /// Convert to classifier output index
    #[must_use]
    pub const fn to_index(self) -> usize {
        match self {
            Self::SymbolQuery => 0,
            Self::TextSearch => 1,
            Self::TracePath => 2,
            Self::FindCallers => 3,
            Self::FindCallees => 4,
            Self::Visualize => 5,
            Self::IndexStatus => 6,
            Self::Ambiguous => 7,
        }
    }

    /// Human-readable name for the intent
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::SymbolQuery => "symbol_query",
            Self::TextSearch => "text_search",
            Self::TracePath => "trace_path",
            Self::FindCallers => "find_callers",
            Self::FindCallees => "find_callees",
            Self::Visualize => "visualize",
            Self::IndexStatus => "index_status",
            Self::Ambiguous => "ambiguous",
        }
    }

    /// Description of what this intent does
    #[must_use]
    pub const fn description(&self) -> &'static str {
        match self {
            Self::SymbolQuery => "Search for symbols by name or pattern",
            Self::TextSearch => "Search for text patterns in code",
            Self::TracePath => "Find call path between two symbols",
            Self::FindCallers => "Find all places that call a symbol",
            Self::FindCallees => "Find all symbols called by a function",
            Self::Visualize => "Generate a diagram of code relationships",
            Self::IndexStatus => "Check the status of the code index",
            Self::Ambiguous => "Intent unclear, needs clarification",
        }
    }
}

impl std::fmt::Display for Intent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Validation status for generated commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    /// Command passed all validation checks
    Valid,
    /// Rejected: contains shell metacharacters
    RejectedMetachar,
    /// Rejected: contains path traversal
    RejectedPathTraversal,
    /// Rejected: attempts write operation
    RejectedWriteMode,
    /// Rejected: contains environment variable
    RejectedEnvVar,
    /// Rejected: exceeds length limit
    RejectedTooLong,
    /// Rejected: doesn't match any allowed template
    RejectedUnknown,
}

impl ValidationStatus {
    /// Whether this status represents a valid command
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        matches!(self, Self::Valid)
    }

    /// Human-readable reason for rejection
    #[must_use]
    pub const fn rejection_reason(&self) -> Option<&'static str> {
        match self {
            Self::Valid => None,
            Self::RejectedMetachar => Some("Contains shell metacharacters"),
            Self::RejectedPathTraversal => Some("Contains path traversal"),
            Self::RejectedWriteMode => Some("Attempts write operation"),
            Self::RejectedEnvVar => Some("Contains environment variable"),
            Self::RejectedTooLong => Some("Exceeds maximum command length"),
            Self::RejectedUnknown => Some("Doesn't match allowed command patterns"),
        }
    }
}

/// Predicate type for CD (Cross-file Discovery) queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredicateType {
    /// Find trait implementations (`impl:Trait`)
    Impl,
    /// Find duplicate code (`duplicates:` or `duplicates:body`)
    Duplicates,
    /// Find circular dependencies (`circular:` or `circular:calls`)
    Circular,
    /// Find unused code (`unused:`)
    Unused,
}

impl PredicateType {
    /// Convert to sqry predicate prefix
    #[must_use]
    pub const fn as_prefix(&self) -> &'static str {
        match self {
            Self::Impl => "impl:",
            Self::Duplicates => "duplicates:",
            Self::Circular => "circular:",
            Self::Unused => "unused:",
        }
    }
}

impl std::fmt::Display for PredicateType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_prefix())
    }
}

/// Visibility filter for symbol queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Public symbols only
    Public,
    /// Private symbols only
    Private,
}

impl Visibility {
    /// Convert to sqry predicate value
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }
}

impl std::fmt::Display for Visibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Symbol kind for filtering queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// Function or method
    Function,
    /// Class definition
    Class,
    /// Struct definition
    Struct,
    /// Enum definition
    Enum,
    /// Trait definition (Rust) or interface (other languages)
    Trait,
    /// Interface definition
    Interface,
    /// Method (attached to a type)
    Method,
    /// Module or package
    Module,
    /// Constant or static variable
    Constant,
    /// Variable definition
    Variable,
    /// Type alias
    TypeAlias,
}

impl SymbolKind {
    /// Convert to CLI argument value
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::Method => "method",
            Self::Module => "module",
            Self::Constant => "constant",
            Self::Variable => "variable",
            Self::TypeAlias => "type_alias",
        }
    }
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Output format for visualization commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    /// Mermaid diagram format
    Mermaid,
    /// Graphviz DOT format
    Dot,
    /// JSON format
    Json,
}

impl OutputFormat {
    /// Convert to CLI argument value
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Mermaid => "mermaid",
            Self::Dot => "dot",
            Self::Json => "json",
        }
    }
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Entities extracted from natural language input.
///
/// This struct contains all the "slots" that can be filled from
/// a natural language query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExtractedEntities {
    /// Symbol names or patterns to search for
    pub symbols: Vec<String>,

    /// Programming languages to filter by
    pub languages: Vec<String>,

    /// Path patterns to filter by
    pub paths: Vec<String>,

    /// Symbol kind filter
    pub kind: Option<SymbolKind>,

    /// Maximum number of results
    pub limit: Option<u32>,

    /// Maximum depth for graph traversal
    pub depth: Option<u32>,

    /// Output format for visualization
    pub format: Option<OutputFormat>,

    /// Source symbol for trace-path
    pub from_symbol: Option<String>,

    /// Target symbol for trace-path
    pub to_symbol: Option<String>,

    /// Relation type for visualization
    pub relation: Option<String>,

    // --- CD Predicate fields ---
    /// Predicate type for CD queries (impl, duplicates, circular, unused)
    pub predicate_type: Option<PredicateType>,

    /// Trait name for impl: predicate (e.g., "Future" in "impl:Future")
    pub impl_trait: Option<String>,

    /// Predicate argument (e.g., "body" in "duplicates:body", "calls" in "circular:calls")
    pub predicate_arg: Option<String>,

    /// Visibility filter (public/private)
    pub visibility: Option<Visibility>,

    /// Async filter (true = find async functions)
    pub is_async: Option<bool>,

    /// Unsafe filter (true = find unsafe code)
    pub is_unsafe: Option<bool>,
}

impl ExtractedEntities {
    /// Create empty entities
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if any symbols were extracted
    #[must_use]
    pub fn has_symbols(&self) -> bool {
        !self.symbols.is_empty()
    }

    /// Check if trace-path entities are complete
    #[must_use]
    pub fn has_trace_path(&self) -> bool {
        self.from_symbol.is_some() && self.to_symbol.is_some()
    }

    /// Get the primary symbol (first one)
    #[must_use]
    pub fn primary_symbol(&self) -> Option<&str> {
        self.symbols.first().map(String::as_str)
    }

    /// Check if this is a CD predicate query
    #[must_use]
    pub fn has_predicate(&self) -> bool {
        self.predicate_type.is_some()
            || self.impl_trait.is_some()
            || self.visibility.is_some()
            || self.is_async.is_some()
            || self.is_unsafe.is_some()
    }

    /// Check if this is an impl: query
    #[must_use]
    pub fn is_impl_query(&self) -> bool {
        self.predicate_type == Some(PredicateType::Impl) || self.impl_trait.is_some()
    }
}

/// Response from the translation pipeline.
///
/// Implements tiered confidence responses (H8 mitigation):
/// - `Execute`: High confidence (≥0.85) - run immediately
/// - `Confirm`: Medium confidence (0.65-0.85) - ask for confirmation
/// - `Disambiguate`: Low confidence (<0.65) - present options
/// - `Reject`: Validation failure or error
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranslationResponse {
    /// High confidence: execute the command
    Execute {
        /// The translated sqry command
        command: String,
        /// Classifier confidence (0.0-1.0)
        confidence: f32,
        /// Classified intent
        intent: Intent,
        /// Whether result was from cache
        cached: bool,
        /// Translation latency in milliseconds
        latency_ms: u64,
    },

    /// Medium confidence: ask user to confirm
    Confirm {
        /// The translated sqry command
        command: String,
        /// Classifier confidence (0.0-1.0)
        confidence: f32,
        /// Human-readable prompt for confirmation
        prompt: String,
    },

    /// Low confidence: present options to disambiguate
    Disambiguate {
        /// Possible interpretations with commands
        options: Vec<DisambiguationOption>,
        /// Human-readable prompt
        prompt: String,
    },

    /// Validation failure or error
    Reject {
        /// Reason for rejection
        reason: String,
        /// Helpful suggestions
        suggestions: Vec<String>,
    },
}

/// A disambiguation option presented when confidence is low.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisambiguationOption {
    /// The translated sqry command for this interpretation
    pub command: String,
    /// The intent this option represents
    pub intent: Intent,
    /// Human-readable description
    pub description: String,
    /// Confidence for this interpretation
    pub confidence: f32,
}

/// Result from preprocessing stage.
#[derive(Debug, Clone)]
pub struct PreprocessResult {
    /// Cleaned and normalized text
    pub text: String,
    /// Quoted spans extracted from input (preserved verbatim)
    pub quoted_spans: Vec<String>,
    /// Whether any normalization was applied
    pub normalized: bool,
    /// Whether any homoglyphs were replaced
    pub homoglyphs_replaced: bool,
}

impl PreprocessResult {
    /// Create a successful preprocess result
    #[must_use]
    pub fn ok(text: String, quoted_spans: Vec<String>) -> Self {
        Self {
            text,
            quoted_spans,
            normalized: false,
            homoglyphs_replaced: false,
        }
    }
}

/// Result from intent classification.
#[derive(Debug, Clone)]
pub struct ClassificationResult {
    /// Classified intent
    pub intent: Intent,
    /// Confidence score (0.0-1.0)
    pub confidence: f32,
    /// All class probabilities
    pub all_probabilities: Vec<f32>,
    /// Model version used for classification
    pub model_version: String,
}

/// Assembled command from template.
#[derive(Debug, Clone)]
pub struct AssembledCommand {
    /// The full command string
    pub command: String,
    /// The template type used
    pub template_type: TemplateType,
}

/// Template type for assembled commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateType {
    /// `sqry query "<expr>" [options]`
    Query,
    /// `sqry search "<pattern>" [options]`
    Search,
    /// `sqry graph trace-path "<from>" "<to>" [options]`
    TracePath,
    /// `sqry graph direct-callers "<symbol>" [options]`
    GraphCallers,
    /// `sqry graph direct-callees "<symbol>" [options]`
    GraphCallees,
    /// `sqry visualize [options]`
    Visualize,
    /// `sqry index --status [options]`
    IndexStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intent_round_trip() {
        for i in 0..Intent::NUM_CLASSES {
            let intent = Intent::from_index(i);
            assert_eq!(intent.to_index(), i);
        }
    }

    #[test]
    fn test_intent_display() {
        assert_eq!(Intent::SymbolQuery.to_string(), "symbol_query");
        assert_eq!(Intent::FindCallers.to_string(), "find_callers");
    }

    #[test]
    fn test_validation_status_is_valid() {
        assert!(ValidationStatus::Valid.is_valid());
        assert!(!ValidationStatus::RejectedMetachar.is_valid());
    }

    #[test]
    fn test_extracted_entities_default() {
        let entities = ExtractedEntities::new();
        assert!(!entities.has_symbols());
        assert!(!entities.has_trace_path());
        assert!(entities.primary_symbol().is_none());
    }

    #[test]
    fn test_extracted_entities_with_symbols() {
        let mut entities = ExtractedEntities::new();
        entities.symbols.push("foo".to_string());
        entities.symbols.push("bar".to_string());

        assert!(entities.has_symbols());
        assert_eq!(entities.primary_symbol(), Some("foo"));
    }

    #[test]
    fn test_translation_response_serde() {
        let response = TranslationResponse::Execute {
            command: "sqry query \"test\"".to_string(),
            confidence: 0.95,
            intent: Intent::SymbolQuery,
            cached: false,
            latency_ms: 42,
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"type\":\"execute\""));
        assert!(json.contains("symbol_query"));

        let parsed: TranslationResponse = serde_json::from_str(&json).unwrap();
        if let TranslationResponse::Execute { confidence, .. } = parsed {
            assert!((confidence - 0.95).abs() < f32::EPSILON);
        } else {
            panic!("Wrong variant");
        }
    }
}
