//! Alias name validation.
//!
//! Validates alias names according to the specification:
//! - Must start with a letter (a-z, A-Z)
//! - Can contain only letters, numbers, dashes, and underscores
//! - Must be 1-64 characters long
//! - Cannot be a reserved word

use std::collections::HashSet;
use std::sync::LazyLock;

/// Reserved words that cannot be used as alias names.
static RESERVED_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // Subcommands
        "alias", "history", "help", "version", "search", "query", "index", "export", "import",
        // Common CLI terms
        "config", "init", "list", "show", "delete", "rename", "clear", "run", "save",
    ]
    .into_iter()
    .collect()
});

/// Minimum alias name length.
pub const MIN_ALIAS_LENGTH: usize = 1;

/// Maximum alias name length.
pub const MAX_ALIAS_LENGTH: usize = 64;

/// Error type for alias name validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasNameError {
    /// Name is empty.
    Empty,
    /// Name is too long.
    TooLong { length: usize, max: usize },
    /// Name doesn't start with a letter.
    InvalidStart { char: char },
    /// Name contains invalid character.
    InvalidChar { char: char, position: usize },
    /// Name is a reserved word.
    Reserved { word: String },
}

impl std::fmt::Display for AliasNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "alias name cannot be empty"),
            Self::TooLong { length, max } => {
                write!(f, "alias name is too long ({length} chars, max {max})")
            }
            Self::InvalidStart { char } => {
                write!(f, "alias name must start with a letter, found '{char}'")
            }
            Self::InvalidChar { char, position } => {
                write!(
                    f,
                    "alias name contains invalid character '{char}' at position {position}"
                )
            }
            Self::Reserved { word } => {
                write!(
                    f,
                    "'{word}' is a reserved word and cannot be used as an alias name"
                )
            }
        }
    }
}

impl std::error::Error for AliasNameError {}

/// Validate an alias name according to the specification.
///
/// # Rules
///
/// - Must start with a letter (a-z, A-Z)
/// - Can contain only letters, numbers, dashes, and underscores
/// - Must be 1-64 characters long
/// - Cannot be a reserved word
///
/// # Errors
///
/// Returns an error if the name violates any of the validation rules.
pub fn validate_alias_name(name: &str) -> Result<(), AliasNameError> {
    // Check empty
    if name.is_empty() {
        return Err(AliasNameError::Empty);
    }

    // Check length
    if name.len() > MAX_ALIAS_LENGTH {
        return Err(AliasNameError::TooLong {
            length: name.len(),
            max: MAX_ALIAS_LENGTH,
        });
    }

    // Check first character is a letter
    let Some(first_char) = name.chars().next() else {
        return Err(AliasNameError::Empty);
    };
    if !first_char.is_ascii_alphabetic() {
        return Err(AliasNameError::InvalidStart { char: first_char });
    }

    // Check all characters are valid
    for (i, c) in name.chars().enumerate() {
        if !is_valid_alias_char(c) {
            return Err(AliasNameError::InvalidChar {
                char: c,
                position: i,
            });
        }
    }

    // Check reserved words (case-insensitive)
    let lower = name.to_lowercase();
    if RESERVED_WORDS.contains(lower.as_str()) {
        return Err(AliasNameError::Reserved {
            word: name.to_string(),
        });
    }

    Ok(())
}

/// Check if a character is valid in an alias name.
///
/// Valid characters are:
/// - ASCII letters (a-z, A-Z)
/// - ASCII digits (0-9)
/// - Dash (-)
/// - Underscore (_)
#[inline]
fn is_valid_alias_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Suggest a valid alias name based on an invalid input.
///
/// This is used to provide helpful error messages.
#[must_use]
pub fn suggest_alias_name(input: &str) -> Option<String> {
    if input.is_empty() {
        return None;
    }

    let mut suggestion = String::with_capacity(input.len());

    for (i, c) in input.chars().enumerate() {
        append_suggestion_char(&mut suggestion, c, i == 0);
    }

    normalize_suggestion(&mut suggestion, input)?;
    Some(suggestion)
}

fn append_suggestion_char(buffer: &mut String, c: char, first: bool) {
    if first {
        if c.is_ascii_alphabetic() {
            buffer.push(c);
        } else if c.is_ascii_digit() {
            // Prefix with 'q' for numbers
            buffer.push('q');
            buffer.push(c);
        }
        return;
    }

    if is_valid_alias_char(c) {
        buffer.push(c);
    } else if c == ' ' || c == '.' {
        // Replace spaces and dots with dashes
        buffer.push('-');
    }
}

fn normalize_suggestion(suggestion: &mut String, input: &str) -> Option<()> {
    // Truncate if too long
    if suggestion.len() > MAX_ALIAS_LENGTH {
        suggestion.truncate(MAX_ALIAS_LENGTH);
    }

    // Check if suggestion is valid and different from input
    if suggestion.is_empty() || suggestion == input {
        return None;
    }

    // Check if it's a reserved word
    let lower = suggestion.to_lowercase();
    if RESERVED_WORDS.contains(lower.as_str()) {
        suggestion.push_str("-query");
        if suggestion.len() > MAX_ALIAS_LENGTH {
            return None;
        }
    }

    // Final validation
    validate_alias_name(suggestion).ok()?;
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_names() {
        let valid = [
            "a",
            "test",
            "my-query",
            "find_functions",
            "Query1",
            "test123",
            "a-b-c",
            "a_b_c",
            "A",
            "MyQuery",
        ];

        for name in valid {
            assert!(
                validate_alias_name(name).is_ok(),
                "expected '{name}' to be valid"
            );
        }
    }

    #[test]
    fn test_empty_name() {
        assert_eq!(validate_alias_name(""), Err(AliasNameError::Empty));
    }

    #[test]
    fn test_too_long_name() {
        let long_name = "a".repeat(65);
        assert_eq!(
            validate_alias_name(&long_name),
            Err(AliasNameError::TooLong {
                length: 65,
                max: 64
            })
        );

        // Exactly 64 should be fine
        let max_name = "a".repeat(64);
        assert!(validate_alias_name(&max_name).is_ok());
    }

    #[test]
    fn test_invalid_start() {
        let invalid_starts = ["1test", "-test", "_test", "0query", ".foo"];

        for name in invalid_starts {
            let result = validate_alias_name(name);
            assert!(
                matches!(result, Err(AliasNameError::InvalidStart { .. })),
                "expected InvalidStart for '{name}', got {result:?}"
            );
        }
    }

    #[test]
    fn test_invalid_chars() {
        let invalid = [
            ("test query", ' ', 4),
            ("test.query", '.', 4),
            ("test@query", '@', 4),
            ("test/query", '/', 4),
            ("test$query", '$', 4),
        ];

        for (name, expected_char, expected_pos) in invalid {
            let result = validate_alias_name(name);
            assert_eq!(
                result,
                Err(AliasNameError::InvalidChar {
                    char: expected_char,
                    position: expected_pos
                }),
                "unexpected result for '{name}'"
            );
        }
    }

    #[test]
    fn test_reserved_words() {
        let reserved = ["help", "HELP", "Help", "alias", "history", "version"];

        for name in reserved {
            let result = validate_alias_name(name);
            assert!(
                matches!(result, Err(AliasNameError::Reserved { .. })),
                "expected Reserved for '{name}', got {result:?}"
            );
        }
    }

    #[test]
    fn test_suggest_alias_name() {
        // Numbers at start get prefixed
        assert_eq!(suggest_alias_name("123test"), Some("q123test".to_string()));

        // Spaces become dashes
        assert_eq!(suggest_alias_name("my query"), Some("my-query".to_string()));

        // Dots become dashes
        assert_eq!(
            suggest_alias_name("test.query"),
            Some("test-query".to_string())
        );

        // Empty returns None
        assert_eq!(suggest_alias_name(""), None);

        // Already valid returns None
        assert_eq!(suggest_alias_name("valid"), None);
    }

    #[test]
    fn test_error_display() {
        assert_eq!(
            AliasNameError::Empty.to_string(),
            "alias name cannot be empty"
        );

        assert_eq!(
            AliasNameError::TooLong {
                length: 65,
                max: 64
            }
            .to_string(),
            "alias name is too long (65 chars, max 64)"
        );

        assert_eq!(
            AliasNameError::InvalidStart { char: '1' }.to_string(),
            "alias name must start with a letter, found '1'"
        );

        assert_eq!(
            AliasNameError::InvalidChar {
                char: ' ',
                position: 4
            }
            .to_string(),
            "alias name contains invalid character ' ' at position 4"
        );

        assert_eq!(
            AliasNameError::Reserved {
                word: "help".to_string()
            }
            .to_string(),
            "'help' is a reserved word and cannot be used as an alias name"
        );
    }
}
