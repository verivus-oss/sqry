//! Regex builder for constructing regex patterns with flags

use crate::query::{RegexFlags, RegexValue};

/// Builder for regex patterns with compilation flags
///
/// Provides a fluent API for constructing regex patterns with optional flags
/// like case-insensitivity, multiline mode, and dot-all mode.
///
/// # Example
///
/// ```ignore
/// use sqry_core::query::builder::RegexBuilder;
///
/// // Simple pattern
/// let regex = RegexBuilder::new("test.*").build()?;
///
/// // With flags
/// let regex = RegexBuilder::new("Test.*")
///     .case_insensitive()
///     .multiline()
///     .build()?;
/// ```
#[derive(Clone, Debug)]
#[must_use = "RegexBuilder does nothing until .build() is called"]
pub struct RegexBuilder {
    /// The regex pattern string
    pub(crate) pattern: String,
    /// Case-insensitive flag
    pub(crate) case_insensitive: bool,
    /// Multiline flag
    pub(crate) multiline: bool,
    /// Dot-all flag (dot matches newlines)
    pub(crate) dot_all: bool,
}

impl RegexBuilder {
    /// Create a new regex builder with the given pattern
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            case_insensitive: false,
            multiline: false,
            dot_all: false,
        }
    }

    /// Enable case-insensitive matching (flag: i)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let regex = RegexBuilder::new("Test")
    ///     .case_insensitive()
    ///     .build()?;
    /// // Matches "test", "TEST", "Test", etc.
    /// ```
    pub fn case_insensitive(mut self) -> Self {
        self.case_insensitive = true;
        self
    }

    /// Enable multiline mode (flag: m)
    ///
    /// In multiline mode, `^` and `$` match line boundaries instead of
    /// just the start and end of the input string.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let regex = RegexBuilder::new("^fn ")
    ///     .multiline()
    ///     .build()?;
    /// // Matches "fn " at the start of any line
    /// ```
    pub fn multiline(mut self) -> Self {
        self.multiline = true;
        self
    }

    /// Enable dot-all mode (flag: s)
    ///
    /// In dot-all mode, `.` matches newline characters as well as
    /// other characters.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let regex = RegexBuilder::new("fn.*}")
    ///     .dot_all()
    ///     .build()?;
    /// // Matches function bodies that span multiple lines
    /// ```
    pub fn dot_all(mut self) -> Self {
        self.dot_all = true;
        self
    }

    /// Build and validate the regex pattern
    ///
    /// This validates the regex syntax with the configured flags applied.
    /// Returns an error if the pattern is invalid.
    ///
    /// # Errors
    ///
    /// Returns `regex::Error` if the pattern is syntactically invalid.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Valid pattern
    /// let regex = RegexBuilder::new("test.*").build()?;
    ///
    /// // Invalid pattern returns error
    /// let result = RegexBuilder::new("[invalid").build();
    /// assert!(result.is_err());
    /// ```
    pub fn build(self) -> Result<RegexValue, regex::Error> {
        // Validate regex syntax WITH flags applied
        let mut builder = regex::RegexBuilder::new(&self.pattern);
        builder.case_insensitive(self.case_insensitive);
        builder.multi_line(self.multiline);
        builder.dot_matches_new_line(self.dot_all);
        builder.build()?;

        Ok(RegexValue {
            pattern: self.pattern,
            flags: RegexFlags {
                case_insensitive: self.case_insensitive,
                multiline: self.multiline,
                dot_all: self.dot_all,
            },
        })
    }

    /// Convert to `RegexValue` without validation
    ///
    /// This creates a `RegexValue` directly without checking if the pattern
    /// is syntactically valid. Validation will occur at `QueryBuilder::build()` time.
    ///
    /// This is used internally by the `*_matches_with()` methods to defer
    /// validation to the build phase.
    pub(crate) fn into_regex_value(self) -> RegexValue {
        RegexValue {
            pattern: self.pattern,
            flags: RegexFlags {
                case_insensitive: self.case_insensitive,
                multiline: self.multiline,
                dot_all: self.dot_all,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_pattern() {
        let regex = RegexBuilder::new("test.*").build().unwrap();
        assert_eq!(regex.pattern, "test.*");
        assert!(!regex.flags.case_insensitive);
        assert!(!regex.flags.multiline);
        assert!(!regex.flags.dot_all);
    }

    #[test]
    fn test_case_insensitive() {
        let regex = RegexBuilder::new("Test")
            .case_insensitive()
            .build()
            .unwrap();
        assert!(regex.flags.case_insensitive);
        assert!(!regex.flags.multiline);
        assert!(!regex.flags.dot_all);
    }

    #[test]
    fn test_multiline() {
        let regex = RegexBuilder::new("^fn ").multiline().build().unwrap();
        assert!(!regex.flags.case_insensitive);
        assert!(regex.flags.multiline);
        assert!(!regex.flags.dot_all);
    }

    #[test]
    fn test_dot_all() {
        let regex = RegexBuilder::new("fn.*}").dot_all().build().unwrap();
        assert!(!regex.flags.case_insensitive);
        assert!(!regex.flags.multiline);
        assert!(regex.flags.dot_all);
    }

    #[test]
    fn test_multiple_flags() {
        let regex = RegexBuilder::new("Test.*")
            .case_insensitive()
            .multiline()
            .dot_all()
            .build()
            .unwrap();
        assert!(regex.flags.case_insensitive);
        assert!(regex.flags.multiline);
        assert!(regex.flags.dot_all);
    }

    #[test]
    fn test_invalid_pattern() {
        let result = RegexBuilder::new("[invalid").build();
        assert!(result.is_err());
    }

    #[test]
    fn test_into_regex_value_no_validation() {
        // Even invalid patterns work with into_regex_value
        let regex = RegexBuilder::new("[invalid").into_regex_value();
        assert_eq!(regex.pattern, "[invalid");
    }

    #[test]
    fn test_clone() {
        let builder = RegexBuilder::new("test").case_insensitive();
        let cloned = builder.clone();
        assert_eq!(cloned.pattern, "test");
        assert!(cloned.case_insensitive);
    }
}
