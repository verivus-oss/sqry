//! Hybrid language family profiles (2 languages).
//!
//! These languages support multiple syntactic styles (curly braces AND keywords).

use super::LanguageProfile;

/// PHP language profile.
pub struct PhpProfile;

impl LanguageProfile for PhpProfile {
    fn language_name(&self) -> &'static str {
        "php"
    }

    fn generate_deeply_nested(&self, depth: usize) -> Vec<u8> {
        let mut result = Vec::new();

        // PHP can use both curly braces and if/endif keywords
        // We'll use curly braces for simplicity
        result.extend_from_slice(b"<?php ");

        for _ in 0..depth {
            result.extend_from_slice(b"{ ");
        }

        result.extend_from_slice(b"echo 'ok'; ");

        for _ in 0..depth {
            result.extend_from_slice(b"} ");
        }

        result.extend_from_slice(b"?>");

        result
    }

    fn minimal_valid(&self) -> &'static str {
        "<?php echo 'ok'; ?>"
    }
}

/// Ruby language profile.
pub struct RubyProfile;

impl LanguageProfile for RubyProfile {
    fn language_name(&self) -> &'static str {
        "ruby"
    }

    fn generate_deeply_nested(&self, depth: usize) -> Vec<u8> {
        let mut result = Vec::new();

        // Ruby uses begin/end keywords and do/end for blocks
        // We'll use nested blocks
        for _ in 0..depth {
            result.extend_from_slice(b"begin ");
        }

        result.extend_from_slice(b"puts 'ok' ");

        for _ in 0..depth {
            result.extend_from_slice(b"end ");
        }

        result
    }

    fn minimal_valid(&self) -> &'static str {
        "puts 'ok'"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_php_profile() {
        let profile = PhpProfile;
        assert_eq!(profile.language_name(), "php");
        assert_eq!(profile.minimal_valid(), "<?php echo 'ok'; ?>");

        let nested = profile.generate_deeply_nested(3);
        let nested_str = String::from_utf8(nested).unwrap();

        assert!(nested_str.starts_with("<?php"));
        assert!(nested_str.ends_with("?>"));
        assert_eq!(nested_str.matches('{').count(), 3);
        assert_eq!(nested_str.matches('}').count(), 3);
    }

    #[test]
    fn test_php_deep_nesting() {
        let profile = PhpProfile;
        let nested = profile.generate_deeply_nested(500);
        let nested_str = String::from_utf8(nested).unwrap();

        assert_eq!(nested_str.matches('{').count(), 500);
        assert_eq!(nested_str.matches('}').count(), 500);
        assert!(nested_str.contains("echo 'ok'"));
    }

    #[test]
    fn test_ruby_profile() {
        let profile = RubyProfile;
        assert_eq!(profile.language_name(), "ruby");
        assert_eq!(profile.minimal_valid(), "puts 'ok'");

        let nested = profile.generate_deeply_nested(3);
        let nested_str = String::from_utf8(nested).unwrap();

        assert_eq!(nested_str.matches("begin").count(), 3);
        assert_eq!(nested_str.matches("end").count(), 3);
        assert!(nested_str.contains("puts 'ok'"));
    }

    #[test]
    fn test_ruby_deep_nesting() {
        let profile = RubyProfile;
        let nested = profile.generate_deeply_nested(100);
        let nested_str = String::from_utf8(nested).unwrap();

        assert_eq!(nested_str.matches("begin").count(), 100);
        assert_eq!(nested_str.matches("end").count(), 100);
    }

    #[test]
    fn test_all_hybrid_profiles() {
        let profiles: Vec<Box<dyn LanguageProfile>> =
            vec![Box::new(PhpProfile), Box::new(RubyProfile)];

        assert_eq!(profiles.len(), 2, "Should have 2 Hybrid profiles");

        for profile in profiles {
            let nested = profile.generate_deeply_nested(10);
            assert!(!nested.is_empty());
            assert!(!profile.minimal_valid().is_empty());
        }
    }
}
