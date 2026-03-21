//! Shared type definitions and utilities for Go relation extraction.
//!
//! The actual export heuristic now lives in `sqry-lang-support` so other
//! languages that share the convention can reuse it.

#[cfg(test)]
mod tests {
    use sqry_lang_support::relations::is_uppercase_export as is_exported;

    #[test]
    fn test_is_exported() {
        assert!(is_exported("Connect"));
        assert!(is_exported("HTTPServer"));
        assert!(is_exported("Server"));

        assert!(!is_exported("connect"));
        assert!(!is_exported("httpServer"));
        assert!(!is_exported("_private"));
        assert!(!is_exported(""));
    }
}
