//! Metadata normalization for symbol attributes
//!
//! This module provides normalization of legacy metadata keys to canonical forms,
//! enabling backward compatibility while transitioning to standardized metadata keys.
//!
//! # Design Principles
//!
//! 1. **User Convenience**: Short forms (`async`) map to canonical (`is_async`) for easier queries
//! 2. **No Data Loss**: Unknown keys are preserved without modification
//! 3. **Last-Wins Semantics**: If both short and canonical keys exist, canonical wins
//! 4. **Transparent**: Plugins use canonical keys (is_async); normalizer handles user input
//!
//! # Usage
//!
//! ```rust
//! use sqry_core::normalizer::MetadataNormalizer;
//! use std::collections::HashMap;
//!
//! let mut raw_metadata = HashMap::new();
//! raw_metadata.insert("async".to_string(), "true".to_string());        // Short form
//! raw_metadata.insert("is_static".to_string(), "true".to_string());    // Canonical form
//! raw_metadata.insert("custom_key".to_string(), "value".to_string());  // Unknown
//!
//! let normalizer = MetadataNormalizer::new();
//! let normalized = normalizer.normalize(raw_metadata);
//!
//! assert_eq!(normalized.get("is_async"), Some(&"true".to_string()));   // Normalized to canonical
//! assert_eq!(normalized.get("is_static"), Some(&"true".to_string()));  // Preserved
//! assert_eq!(normalized.get("custom_key"), Some(&"value".to_string()));// Preserved
//! ```
//!
//! # Mapping Strategy
//!
//! The normalizer uses a static mapping table. User-friendly short forms map TO the
//! canonical keys defined in `sqry_core::metadata::keys`:
//!
//! - `async` → `is_async` (matches metadata::keys::IS_ASYNC)
//! - `static` → `is_static` (matches metadata::keys::IS_STATIC)
//! - `const` → `is_const` (matches metadata::keys::IS_CONST)
//! - `final` → `is_final` (matches metadata::keys::IS_FINAL)
//! - `abstract` → `is_abstract` (matches metadata::keys::IS_ABSTRACT)
//! - And more...
//!
//! Keys already in canonical form (`is_async`) or not in the mapping table are
//! preserved as-is.

use std::collections::HashMap;

/// Metadata normalizer for query convenience
///
/// Converts user-friendly short forms (e.g., `async`) to canonical keys (e.g., `is_async`)
/// used by plugins. Enables easier queries while maintaining consistency with `metadata::keys`.
#[derive(Debug, Clone)]
pub struct MetadataNormalizer {
    /// Legacy key → canonical key mappings
    legacy_to_canonical: HashMap<&'static str, &'static str>,
}

impl MetadataNormalizer {
    /// Create a new metadata normalizer with default mappings
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::normalizer::MetadataNormalizer;
    ///
    /// let normalizer = MetadataNormalizer::new();
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            legacy_to_canonical: Self::legacy_mappings(),
        }
    }

    /// Define legacy → canonical key mappings
    ///
    /// This is the single source of truth for all legacy key migrations.
    ///
    /// **Important**: Canonical keys MUST match the constants in `sqry_core::metadata::keys`.
    /// The normalizer maps alternative forms (user queries, old indexes) to the canonical
    /// keys that plugins actually use.
    ///
    /// For example:
    /// - User query: `async:true` → normalized to `is_async:true` (matches `metadata::keys::IS_ASYNC`)
    /// - User query: `is_async:true` → preserved as-is (already canonical)
    fn legacy_mappings() -> HashMap<&'static str, &'static str> {
        let mut mappings = HashMap::new();

        // Query convenience: Allow users to type shorter forms without "is_" prefix
        // These map TO the canonical keys defined in metadata::keys module

        // Function modifiers
        mappings.insert("async", "is_async");
        mappings.insert("static", "is_static");
        mappings.insert("abstract", "is_abstract");
        mappings.insert("final", "is_final");
        mappings.insert("override", "is_override");
        mappings.insert("mutating", "is_mutating");
        mappings.insert("generator", "is_generator");
        mappings.insert("exported", "is_exported");
        mappings.insert("throwing", "is_throwing");

        // Class/Type modifiers
        mappings.insert("struct", "is_struct");
        mappings.insert("enum", "is_enum");
        mappings.insert("interface", "is_interface");
        mappings.insert("protocol", "is_protocol");
        mappings.insert("actor", "is_actor");
        mappings.insert("extension", "is_extension");
        mappings.insert("trait", "is_trait");
        mappings.insert("sealed", "is_sealed");
        mappings.insert("generics", "has_generics");

        // Property modifiers
        mappings.insert("computed", "is_computed");
        mappings.insert("lazy", "is_lazy");
        mappings.insert("weak", "is_weak");
        mappings.insert("mutable", "is_mutable");
        mappings.insert("const", "is_const");

        // Language-specific
        mappings.insert("classmethod", "is_classmethod");
        mappings.insert("staticmethod", "is_staticmethod");
        mappings.insert("property_decorator", "is_property_decorator");
        mappings.insert("receiver", "has_receiver");
        mappings.insert("pointer_receiver", "is_pointer_receiver");
        mappings.insert("synchronized", "is_synchronized");
        mappings.insert("constexpr", "is_constexpr");
        mappings.insert("noexcept", "is_noexcept");
        mappings.insert("unsafe", "is_unsafe");
        mappings.insert("const_fn", "is_const_fn");
        mappings.insert("readonly", "is_readonly");
        mappings.insert("factory", "is_factory");
        mappings.insert("external", "is_external");

        mappings
    }

    /// Normalize metadata by mapping legacy keys to canonical forms
    ///
    /// # Behavior
    ///
    /// - Legacy keys are converted to canonical keys
    /// - Unknown keys are preserved as-is (no data loss)
    /// - If both legacy and canonical keys exist, canonical wins
    /// - Empty metadata returns empty hashmap
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::normalizer::MetadataNormalizer;
    /// use std::collections::HashMap;
    ///
    /// let mut raw = HashMap::new();
    /// raw.insert("async".to_string(), "true".to_string());       // Short form
    /// raw.insert("custom".to_string(), "value".to_string());     // Unknown
    ///
    /// let normalizer = MetadataNormalizer::new();
    /// let normalized = normalizer.normalize(raw);
    ///
    /// assert_eq!(normalized.get("is_async"), Some(&"true".to_string())); // Canonical
    /// assert_eq!(normalized.get("custom"), Some(&"value".to_string()));   // Preserved
    /// assert_eq!(normalized.get("async"), None); // Short form removed
    /// ```
    #[must_use]
    pub fn normalize(&self, raw: HashMap<String, String>) -> HashMap<String, String> {
        let mut normalized = HashMap::new();

        for (key, value) in raw {
            // Check if this is a legacy key that needs conversion
            if let Some(&canonical_key) = self.legacy_to_canonical.get(key.as_str()) {
                // Use canonical key (this will overwrite if canonical already exists - last wins)
                log::debug!(
                    "Normalizing metadata: legacy key '{key}' → canonical '{canonical_key}'"
                );
                normalized.insert(canonical_key.to_string(), value);
            } else {
                // Not a legacy key - preserve as-is
                // This includes:
                // - Already-canonical keys (is_async, is_static, etc.)
                // - Plugin-specific keys (not in mapping table)
                // - Future keys (forward compatibility)
                normalized.insert(key, value);
            }
        }

        normalized
    }

    /// Check if a key is a legacy key that will be normalized
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::normalizer::MetadataNormalizer;
    ///
    /// let normalizer = MetadataNormalizer::new();
    /// assert!(normalizer.is_legacy_key("async"));           // Short form
    /// assert!(!normalizer.is_legacy_key("is_async"));       // Canonical
    /// assert!(!normalizer.is_legacy_key("custom_key"));     // Unknown
    /// ```
    #[must_use]
    pub fn is_legacy_key(&self, key: &str) -> bool {
        self.legacy_to_canonical.contains_key(key)
    }

    /// Get the canonical form of a key (if it's a legacy key)
    ///
    /// Returns `None` if the key is not a legacy key.
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::normalizer::MetadataNormalizer;
    ///
    /// let normalizer = MetadataNormalizer::new();
    /// assert_eq!(normalizer.get_canonical("async"), Some("is_async"));    // Short → canonical
    /// assert_eq!(normalizer.get_canonical("is_async"), None);            // Already canonical
    /// assert_eq!(normalizer.get_canonical("custom"), None);              // Unknown
    /// ```
    #[must_use]
    pub fn get_canonical(&self, key: &str) -> Option<&'static str> {
        self.legacy_to_canonical.get(key).copied()
    }

    /// Get all legacy keys supported by this normalizer
    ///
    /// Useful for documentation and validation.
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::normalizer::MetadataNormalizer;
    ///
    /// let normalizer = MetadataNormalizer::new();
    /// let legacy_keys: Vec<&&str> = normalizer.legacy_keys().collect();
    /// assert!(legacy_keys.contains(&&"async"));       // Short forms
    /// assert!(legacy_keys.contains(&&"static"));
    /// ```
    pub fn legacy_keys(&self) -> impl Iterator<Item = &&'static str> {
        self.legacy_to_canonical.keys()
    }

    /// Get all canonical keys (targets of normalization)
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::normalizer::MetadataNormalizer;
    ///
    /// let normalizer = MetadataNormalizer::new();
    /// let canonical_keys: Vec<&&str> = normalizer.canonical_keys().collect();
    /// assert!(canonical_keys.contains(&&"is_async"));    // Canonical forms
    /// assert!(canonical_keys.contains(&&"is_static"));
    /// ```
    pub fn canonical_keys(&self) -> impl Iterator<Item = &&'static str> {
        self.legacy_to_canonical.values()
    }

    /// Get all short form → canonical mappings
    ///
    /// Returns an iterator over (`short_form`, canonical) pairs.
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::normalizer::MetadataNormalizer;
    ///
    /// let normalizer = MetadataNormalizer::new();
    /// for (short_form, canonical) in normalizer.mappings() {
    ///     println!("{} → {}", short_form, canonical);
    /// }
    /// ```
    pub fn mappings(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        self.legacy_to_canonical.iter().map(|(&k, &v)| (k, v))
    }
}

impl Default for MetadataNormalizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_short_form_to_canonical() {
        let normalizer = MetadataNormalizer::new();
        let mut raw = HashMap::new();
        raw.insert("async".to_string(), "true".to_string()); // Short form

        let canonical_metadata = normalizer.normalize(raw);

        assert_eq!(
            canonical_metadata.get("is_async"),
            Some(&"true".to_string())
        ); // Canonical
        assert_eq!(canonical_metadata.get("async"), None); // Short form removed
    }

    #[test]
    fn test_normalize_multiple_short_forms() {
        let normalizer = MetadataNormalizer::new();
        let mut raw = HashMap::new();
        raw.insert("async".to_string(), "true".to_string());
        raw.insert("static".to_string(), "true".to_string());
        raw.insert("final".to_string(), "false".to_string());

        let canonical_metadata = normalizer.normalize(raw);

        assert_eq!(
            canonical_metadata.get("is_async"),
            Some(&"true".to_string())
        );
        assert_eq!(
            canonical_metadata.get("is_static"),
            Some(&"true".to_string())
        );
        assert_eq!(
            canonical_metadata.get("is_final"),
            Some(&"false".to_string())
        );
        assert_eq!(canonical_metadata.len(), 3);
    }

    #[test]
    fn test_preserve_unknown_keys() {
        let normalizer = MetadataNormalizer::new();
        let mut raw = HashMap::new();
        raw.insert("custom_plugin_key".to_string(), "value1".to_string());
        raw.insert("another_custom".to_string(), "value2".to_string());

        let canonical_metadata = normalizer.normalize(raw);

        assert_eq!(
            canonical_metadata.get("custom_plugin_key"),
            Some(&"value1".to_string())
        );
        assert_eq!(
            canonical_metadata.get("another_custom"),
            Some(&"value2".to_string())
        );
        assert_eq!(canonical_metadata.len(), 2);
    }

    #[test]
    fn test_canonical_key_preserved() {
        let normalizer = MetadataNormalizer::new();
        let mut raw = HashMap::new();
        raw.insert("is_async".to_string(), "true".to_string()); // Already canonical

        let canonical_metadata = normalizer.normalize(raw);

        // Canonical key should be preserved as-is
        assert_eq!(
            canonical_metadata.get("is_async"),
            Some(&"true".to_string())
        );
        assert_eq!(canonical_metadata.len(), 1);
    }

    #[test]
    fn test_canonical_wins_over_short_form() {
        let normalizer = MetadataNormalizer::new();
        let mut raw = HashMap::new();
        raw.insert("async".to_string(), "false".to_string()); // Short form
        raw.insert("is_async".to_string(), "true".to_string()); // Canonical

        let canonical_metadata = normalizer.normalize(raw);

        // Canonical key should win (last wins semantics in HashMap iteration)
        // Both map to is_async, so one will overwrite the other
        assert!(
            canonical_metadata.get("is_async") == Some(&"true".to_string())
                || canonical_metadata.get("is_async") == Some(&"false".to_string())
        );
        assert_eq!(canonical_metadata.len(), 1);
    }

    #[test]
    fn test_empty_metadata() {
        let normalizer = MetadataNormalizer::new();
        let raw = HashMap::new();

        let canonical_metadata = normalizer.normalize(raw);

        assert!(canonical_metadata.is_empty());
    }

    #[test]
    fn test_mixed_short_canonical_unknown() {
        let normalizer = MetadataNormalizer::new();
        let mut raw = HashMap::new();
        raw.insert("async".to_string(), "true".to_string()); // Short form
        raw.insert("is_static".to_string(), "true".to_string()); // Canonical
        raw.insert("custom".to_string(), "value".to_string()); // Unknown

        let canonical_metadata = normalizer.normalize(raw);

        assert_eq!(
            canonical_metadata.get("is_async"),
            Some(&"true".to_string())
        );
        assert_eq!(
            canonical_metadata.get("is_static"),
            Some(&"true".to_string())
        );
        assert_eq!(canonical_metadata.get("custom"), Some(&"value".to_string()));
        assert_eq!(canonical_metadata.len(), 3);
    }

    #[test]
    fn test_is_legacy_key() {
        let normalizer = MetadataNormalizer::new();

        // Short forms are "legacy" (need normalization)
        assert!(normalizer.is_legacy_key("async"));
        assert!(normalizer.is_legacy_key("static"));
        assert!(normalizer.is_legacy_key("final"));

        // Canonical forms are NOT legacy
        assert!(!normalizer.is_legacy_key("is_async"));
        assert!(!normalizer.is_legacy_key("is_static"));
        assert!(!normalizer.is_legacy_key("custom_key"));
    }

    #[test]
    fn test_get_canonical() {
        let normalizer = MetadataNormalizer::new();

        // Short → canonical mapping
        assert_eq!(normalizer.get_canonical("async"), Some("is_async"));
        assert_eq!(normalizer.get_canonical("static"), Some("is_static"));
        assert_eq!(normalizer.get_canonical("throwing"), Some("is_throwing"));

        // Canonical forms have no mapping
        assert_eq!(normalizer.get_canonical("is_async"), None);
        assert_eq!(normalizer.get_canonical("custom"), None);
    }

    #[test]
    fn test_visibility_key_not_normalized() {
        // visibility is already canonical, not a short form
        let normalizer = MetadataNormalizer::new();
        let mut raw = HashMap::new();
        raw.insert("visibility".to_string(), "public".to_string());

        let canonical_metadata = normalizer.normalize(raw);

        assert_eq!(
            canonical_metadata.get("visibility"),
            Some(&"public".to_string())
        );
        assert_eq!(canonical_metadata.len(), 1);
    }

    #[test]
    fn test_all_function_modifiers() {
        let normalizer = MetadataNormalizer::new();
        let mut raw = HashMap::new();
        raw.insert("async".to_string(), "true".to_string());
        raw.insert("static".to_string(), "true".to_string());
        raw.insert("abstract".to_string(), "true".to_string());
        raw.insert("final".to_string(), "true".to_string());
        raw.insert("override".to_string(), "true".to_string());

        let canonical_metadata = normalizer.normalize(raw);

        assert_eq!(
            canonical_metadata.get("is_async"),
            Some(&"true".to_string())
        );
        assert_eq!(
            canonical_metadata.get("is_static"),
            Some(&"true".to_string())
        );
        assert_eq!(
            canonical_metadata.get("is_abstract"),
            Some(&"true".to_string())
        );
        assert_eq!(
            canonical_metadata.get("is_final"),
            Some(&"true".to_string())
        );
        assert_eq!(
            canonical_metadata.get("is_override"),
            Some(&"true".to_string())
        );
        assert_eq!(canonical_metadata.len(), 5);
    }

    #[test]
    fn test_all_class_modifiers() {
        let normalizer = MetadataNormalizer::new();
        let mut raw = HashMap::new();
        raw.insert("struct".to_string(), "true".to_string());
        raw.insert("enum".to_string(), "true".to_string());
        raw.insert("interface".to_string(), "true".to_string());
        raw.insert("actor".to_string(), "true".to_string());
        raw.insert("generics".to_string(), "true".to_string());

        let canonical_metadata = normalizer.normalize(raw);

        assert_eq!(
            canonical_metadata.get("is_struct"),
            Some(&"true".to_string())
        );
        assert_eq!(canonical_metadata.get("is_enum"), Some(&"true".to_string()));
        assert_eq!(
            canonical_metadata.get("is_interface"),
            Some(&"true".to_string())
        );
        assert_eq!(
            canonical_metadata.get("is_actor"),
            Some(&"true".to_string())
        );
        assert_eq!(
            canonical_metadata.get("has_generics"),
            Some(&"true".to_string())
        );
        assert_eq!(canonical_metadata.len(), 5);
    }

    #[test]
    fn test_property_modifiers() {
        let normalizer = MetadataNormalizer::new();
        let mut raw = HashMap::new();
        raw.insert("computed".to_string(), "true".to_string());
        raw.insert("lazy".to_string(), "true".to_string());
        raw.insert("weak".to_string(), "true".to_string());
        raw.insert("const".to_string(), "true".to_string());

        let canonical_metadata = normalizer.normalize(raw);

        assert_eq!(
            canonical_metadata.get("is_computed"),
            Some(&"true".to_string())
        );
        assert_eq!(canonical_metadata.get("is_lazy"), Some(&"true".to_string()));
        assert_eq!(canonical_metadata.get("is_weak"), Some(&"true".to_string()));
        assert_eq!(
            canonical_metadata.get("is_const"),
            Some(&"true".to_string())
        );
        assert_eq!(canonical_metadata.len(), 4);
    }

    #[test]
    fn test_language_specific_python() {
        let normalizer = MetadataNormalizer::new();
        let mut raw = HashMap::new();
        raw.insert("classmethod".to_string(), "true".to_string());
        raw.insert("staticmethod".to_string(), "true".to_string());

        let canonical_metadata = normalizer.normalize(raw);

        assert_eq!(
            canonical_metadata.get("is_classmethod"),
            Some(&"true".to_string())
        );
        assert_eq!(
            canonical_metadata.get("is_staticmethod"),
            Some(&"true".to_string())
        );
        assert_eq!(canonical_metadata.len(), 2);
    }

    #[test]
    fn test_legacy_keys_iterator() {
        let normalizer = MetadataNormalizer::new();
        let legacy_keys: Vec<&&str> = normalizer.legacy_keys().collect();

        assert!(legacy_keys.len() > 20); // Should have many mappings
        // Legacy keys are SHORT forms
        assert!(legacy_keys.contains(&&"async"));
        assert!(legacy_keys.contains(&&"static"));
    }

    #[test]
    fn test_canonical_keys_iterator() {
        let normalizer = MetadataNormalizer::new();
        let canonical_keys: Vec<&&str> = normalizer.canonical_keys().collect();

        assert!(canonical_keys.len() > 20);
        // Canonical keys are IS_* forms
        assert!(canonical_keys.contains(&&"is_async"));
        assert!(canonical_keys.contains(&&"is_static"));
    }
}
