//! Shell metacharacter detection.

use crate::types::ValidationStatus;
use regex::Regex;
use std::sync::LazyLock;

/// Pattern matching dangerous shell metacharacters
static DANGEROUS_CHARS: LazyLock<Regex> = LazyLock::new(|| {
    // Matches: ; | & $ ` < > newline carriage-return control-chars $() ${} && ||
    Regex::new(r"[;|&`<>\n\r\x00-\x1f]|\$\(|\$\{|&&|\|\|").expect("Invalid dangerous chars regex")
});

/// Pattern matching environment variable references
static ENV_VAR_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Matches: $VAR, ${VAR}, $HOME, ${HOME}
    Regex::new(r"\$\{?\w+\}?").expect("Invalid env var regex")
});

/// Pattern matching write/destructive operation flags
static WRITE_FLAGS: LazyLock<Regex> = LazyLock::new(|| {
    // Matches common write/destructive flags
    // Note: Use (^|\s) instead of \b for flags with -- since \b doesn't work with hyphens
    Regex::new(r"(?i)(^|\s)(--force|--delete|--remove|--prune)(\s|$)")
        .expect("Invalid write flags regex")
});

/// Pattern matching write/destructive operation keywords (for unquoted context)
static WRITE_KEYWORDS: LazyLock<Regex> = LazyLock::new(|| {
    // These keywords should only be matched outside quoted strings
    Regex::new(r"(?i)\b(repair|destroy|drop|truncate)\b").expect("Invalid write keywords regex")
});

/// Pattern to match quoted strings (both single and double quotes)
static QUOTED_STRING: LazyLock<Regex> = LazyLock::new(|| {
    // Matches "..." or '...' (including escaped quotes within)
    Regex::new(r#""(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'"#).expect("Invalid quoted string regex")
});

/// Check if command contains dangerous shell metacharacters.
///
/// Returns `Some(ValidationStatus)` if dangerous chars found, `None` if safe.
#[must_use]
pub fn contains_dangerous_chars(command: &str) -> Option<ValidationStatus> {
    // Check for env vars first (more specific error)
    if ENV_VAR_PATTERN.is_match(command) {
        return Some(ValidationStatus::RejectedEnvVar);
    }

    // Check for metacharacters
    if DANGEROUS_CHARS.is_match(command) {
        return Some(ValidationStatus::RejectedMetachar);
    }

    None
}

/// Strip quoted strings from command for write operation detection.
///
/// This prevents false positives when symbols like "`drop_table`" appear in search patterns.
fn strip_quoted_sections(command: &str) -> String {
    QUOTED_STRING.replace_all(command, " ").to_string()
}

/// Check if command contains write/destructive operations.
///
/// This function checks for write operation flags (like --force) and keywords
/// (like repair, drop) that indicate destructive operations. Keywords are only
/// matched outside of quoted strings to allow searching for symbols like
/// "`drop_table`" or "`truncate_string`".
#[must_use]
pub fn contains_write_operation(command: &str) -> bool {
    // Check flags first (these are always dangerous regardless of context)
    if WRITE_FLAGS.is_match(command) {
        return true;
    }

    // For keywords, strip quoted sections first to avoid false positives
    // This allows: sqry query "drop_table" but rejects: sqry index drop
    let unquoted = strip_quoted_sections(command);
    WRITE_KEYWORDS.is_match(&unquoted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_command() {
        assert!(contains_dangerous_chars("sqry query \"foo\"").is_none());
    }

    #[test]
    fn test_semicolon() {
        assert_eq!(
            contains_dangerous_chars("foo; bar"),
            Some(ValidationStatus::RejectedMetachar)
        );
    }

    #[test]
    fn test_pipe() {
        assert_eq!(
            contains_dangerous_chars("foo | bar"),
            Some(ValidationStatus::RejectedMetachar)
        );
    }

    #[test]
    fn test_backtick() {
        assert_eq!(
            contains_dangerous_chars("foo `whoami`"),
            Some(ValidationStatus::RejectedMetachar)
        );
    }

    #[test]
    fn test_command_substitution() {
        assert_eq!(
            contains_dangerous_chars("foo $(whoami)"),
            Some(ValidationStatus::RejectedMetachar)
        );
    }

    #[test]
    fn test_env_var_dollar() {
        assert_eq!(
            contains_dangerous_chars("$HOME/foo"),
            Some(ValidationStatus::RejectedEnvVar)
        );
    }

    #[test]
    fn test_env_var_braces() {
        assert_eq!(
            contains_dangerous_chars("${HOME}/foo"),
            Some(ValidationStatus::RejectedEnvVar)
        );
    }

    #[test]
    fn test_and_and() {
        assert_eq!(
            contains_dangerous_chars("foo && bar"),
            Some(ValidationStatus::RejectedMetachar)
        );
    }

    #[test]
    fn test_or_or() {
        assert_eq!(
            contains_dangerous_chars("foo || bar"),
            Some(ValidationStatus::RejectedMetachar)
        );
    }

    #[test]
    fn test_write_operation_flags() {
        assert!(contains_write_operation("sqry index --force"));
        assert!(contains_write_operation("sqry index --delete"));
        assert!(contains_write_operation("sqry index --prune"));
        // force without -- is not a flag
        assert!(!contains_write_operation("sqry query \"force\""));
    }

    #[test]
    fn test_write_operation_keywords_unquoted() {
        // Keywords outside quotes should be rejected
        assert!(contains_write_operation("sqry index repair"));
        assert!(contains_write_operation("sqry index drop"));
        assert!(contains_write_operation("sqry index truncate"));
        assert!(contains_write_operation("sqry index destroy"));
    }

    #[test]
    fn test_write_operation_keywords_quoted() {
        // Keywords inside quoted strings should NOT be rejected
        // This allows searching for symbols like drop_table, truncate_string
        assert!(!contains_write_operation("sqry query \"drop_table\""));
        assert!(!contains_write_operation("sqry query \"truncate_string\""));
        assert!(!contains_write_operation(
            "sqry query \"repair_connection\""
        ));
        assert!(!contains_write_operation("sqry search \"destroy_session\""));
        // Single quotes too
        assert!(!contains_write_operation("sqry query 'drop_column'"));
    }

    #[test]
    fn test_strip_quoted_sections() {
        assert_eq!(strip_quoted_sections("hello \"world\" foo"), "hello   foo");
        assert_eq!(strip_quoted_sections("hello 'world' foo"), "hello   foo");
        assert_eq!(strip_quoted_sections("no quotes"), "no quotes");
        // Escaped quotes should be handled
        assert_eq!(
            strip_quoted_sections(r#"hello "wor\"ld" foo"#),
            "hello   foo"
        );
    }
}
