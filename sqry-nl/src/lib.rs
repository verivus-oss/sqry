//! # sqry-nl: Natural Language Translation Layer for sqry
//!
//! This crate provides translation from natural language queries to sqry commands.
//! It uses a MiniLM-L6-v2-based intent classifier combined with regex-based entity
//! extraction to produce validated, safe sqry commands.
//!
//! ## Architecture
//!
//! The translation pipeline consists of:
//!
//! 1. **Preprocessor** - Unicode normalization, homoglyph detection, input sanitization
//! 2. **Entity Extractor** - Regex-based slot filling for symbols, languages, etc.
//! 3. **Intent Classifier** - MiniLM-L6-v2 ONNX model for intent classification
//! 4. **Template Assembler** - Maps (intent, entities) to sqry command templates
//! 5. **Safety Validator** - Whitelist validation, metachar rejection, path guards
//! 6. **Translation Cache** - LRU cache for repeated queries
//!
//! ## Example
//!
//! ```rust,ignore
//! use sqry_nl::{Translator, TranslatorConfig, TranslationResponse};
//!
//! let config = TranslatorConfig::default();
//! let translator = Translator::load(config)?;
//!
//! match translator.translate("find authentication functions in rust") {
//!     TranslationResponse::Execute { command, confidence, .. } => {
//!         println!("Command: {} (confidence: {:.1}%)", command, confidence * 100.0);
//!         // Execute the command via sqry CLI
//!     }
//!     TranslationResponse::Confirm { command, prompt, .. } => {
//!         println!("{}", prompt);
//!         // Ask user for confirmation
//!     }
//!     TranslationResponse::Disambiguate { options, prompt } => {
//!         println!("{}", prompt);
//!         // Present options to user
//!     }
//!     TranslationResponse::Reject { reason, suggestions } => {
//!         eprintln!("Cannot translate: {}", reason);
//!         // Show suggestions
//!     }
//! }
//! ```
//!
//! ## Safety
//!
//! All generated commands are validated against a strict whitelist of allowed
//! command templates. The following are always rejected:
//!
//! - Shell metacharacters (`;`, `|`, `&`, `$`, etc.)
//! - Environment variable expansion (`$HOME`, `${VAR}`)
//! - Path traversal (`..`, absolute paths)
//! - Write-mode operations (`--force`, `repair`, `prune`)
//!
//! ## Feature Flags
//!
//! - `classifier` (default) - Enable the MiniLM-L6-v2 classifier. Requires ONNX Runtime.
//!   Disable for minimal builds that only need rule-based classification.

// Public modules
pub mod assembler;
pub mod cache;
pub mod classifier;
pub mod error;
pub mod extractor;
pub mod preprocess;
pub mod translator;
pub mod types;
pub mod validator;

// Re-exports for convenience
pub use cache::{CacheConfig, CacheStats};
#[cfg(feature = "classifier")]
pub use classifier::onnx_runtime_install_hint;
pub use error::{NlError, NlResult};
pub use translator::{Translator, TranslatorConfig};
pub use types::{
    DisambiguationOption, ExtractedEntities, Intent, OutputFormat, SymbolKind, TranslationResponse,
    ValidationStatus,
};

/// Crate version for model compatibility checks
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_types_are_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}

        // Core types must be thread-safe
        assert_send::<Intent>();
        assert_sync::<Intent>();
        assert_send::<ValidationStatus>();
        assert_sync::<ValidationStatus>();
        assert_send::<SymbolKind>();
        assert_sync::<SymbolKind>();
        assert_send::<OutputFormat>();
        assert_sync::<OutputFormat>();
        assert_send::<ExtractedEntities>();
        assert_sync::<ExtractedEntities>();
        assert_send::<TranslationResponse>();
        assert_sync::<TranslationResponse>();
    }

    #[test]
    fn test_version_available() {
        // VERSION is always non-empty (from Cargo.toml) but we test
        // that the package version is properly exposed and matches expected format
        assert!(
            VERSION.contains('.'),
            "VERSION should contain a dot separator"
        );
    }
}
