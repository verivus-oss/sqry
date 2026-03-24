//! Indentation-based language family profiles (2 languages).
//!
//! These languages use indentation for block structuring, making them
//! susceptible to deeply nested indentation levels.

use super::LanguageProfile;

/// Python language profile.
pub struct PythonProfile;

impl LanguageProfile for PythonProfile {
    fn language_name(&self) -> &'static str {
        "python"
    }

    fn generate_deeply_nested(&self, depth: usize) -> Vec<u8> {
        let mut result = Vec::new();

        // Generate deeply nested if statements with indentation
        for i in 0..depth {
            let indent = "    ".repeat(i); // 4 spaces per level
            result.extend_from_slice(indent.as_bytes());
            result.extend_from_slice(b"if True:\n");
        }

        // Add a pass statement at the deepest level
        let indent = "    ".repeat(depth);
        result.extend_from_slice(indent.as_bytes());
        result.extend_from_slice(b"pass\n");

        result
    }

    fn minimal_valid(&self) -> &'static str {
        "pass"
    }
}

/// ABAP language profile.
pub struct AbapProfile;

impl LanguageProfile for AbapProfile {
    fn language_name(&self) -> &'static str {
        "abap"
    }

    fn generate_deeply_nested(&self, depth: usize) -> Vec<u8> {
        let mut result = Vec::new();

        // Generate deeply nested IF statements
        for i in 0..depth {
            let indent = "  ".repeat(i); // 2 spaces per level
            result.extend_from_slice(indent.as_bytes());
            result.extend_from_slice(b"IF 1 = 1.\n");
        }

        // Add empty statement at deepest level
        let indent = "  ".repeat(depth);
        result.extend_from_slice(indent.as_bytes());
        result.extend_from_slice(b"ENDIF.\n");

        // Close all IF statements
        for i in (0..depth).rev() {
            let indent = "  ".repeat(i);
            result.extend_from_slice(indent.as_bytes());
            result.extend_from_slice(b"ENDIF.\n");
        }

        result
    }

    fn minimal_valid(&self) -> &'static str {
        "WRITE 'Hello'."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_profile() {
        let profile = PythonProfile;
        assert_eq!(profile.language_name(), "python");
        assert_eq!(profile.minimal_valid(), "pass");

        let nested = profile.generate_deeply_nested(3);
        let nested_str = String::from_utf8(nested).unwrap();

        // Should have 3 levels of indentation
        assert!(nested_str.contains("if True:"));
        assert!(nested_str.contains("            pass")); // 12 spaces (3 * 4)
    }

    #[test]
    fn test_python_deep_nesting() {
        let profile = PythonProfile;
        let nested = profile.generate_deeply_nested(500);
        let nested_str = String::from_utf8(nested).unwrap();

        // Should have 500 if statements
        assert_eq!(nested_str.matches("if True:").count(), 500);
        assert!(nested_str.contains("pass"));
    }

    #[test]
    fn test_abap_profile() {
        let profile = AbapProfile;
        assert_eq!(profile.language_name(), "abap");
        assert_eq!(profile.minimal_valid(), "WRITE 'Hello'.");

        let nested = profile.generate_deeply_nested(3);
        let nested_str = String::from_utf8(nested).unwrap();

        // Should have 3 IF statements and 4 ENDIFs (3 + 1 for deepest level)
        assert_eq!(nested_str.matches("IF 1 = 1.").count(), 3);
        assert_eq!(nested_str.matches("ENDIF.").count(), 4);
    }

    #[test]
    fn test_abap_deep_nesting() {
        let profile = AbapProfile;
        let nested = profile.generate_deeply_nested(500);
        let nested_str = String::from_utf8(nested).unwrap();

        // Should have 500 IF statements and 501 ENDIFs
        assert_eq!(nested_str.matches("IF 1 = 1.").count(), 500);
        assert_eq!(nested_str.matches("ENDIF.").count(), 501);
    }
}
