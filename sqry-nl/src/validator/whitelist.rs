//! Command whitelist validation.
//!
//! Only commands matching these templates are allowed.

use regex::Regex;
use std::sync::LazyLock;

/// Safe characters allowed inside quoted arguments.
/// Excludes shell metacharacters: `;`, `|`, `&`, `$`, backtick, `<`, `>`, `(`, `)`, `{`, `}`, `[`, `]`, `!`, `#`.
/// Also excludes: newline, carriage return, control characters
const SAFE_QUOTED_CHARS: &str = r"[a-zA-Z0-9_.\-/:@*?, ]+";

/// Safe characters for path arguments (more restrictive).
const SAFE_PATH_CHARS: &str = r"[a-zA-Z0-9_.\-/*?]+";

/// Allowed command templates.
///
/// Each template is a regex pattern that matches valid sqry commands.
/// All quoted arguments are restricted to safe characters only.
static ALLOWED_TEMPLATES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // sqry query "<expr with predicates>" [--language <lang>]* [--path "<glob>"] [--limit N]
        // Note: kind filter is now inside the query expression as `kind:X` predicate
        Regex::new(&format!(
            r#"^sqry query "{SAFE_QUOTED_CHARS}?"(\s+--language\s+[a-z]+)*(\s+--path\s+"{SAFE_PATH_CHARS}")?(\s+--limit\s+\d+)?$"#
        )).expect("Invalid query template"),

        // sqry search "<pattern>" [--language <lang>]* [--path "<glob>"]
        Regex::new(&format!(
            r#"^sqry search "{SAFE_QUOTED_CHARS}?"(\s+--language\s+[a-z]+)*(\s+--path\s+"{SAFE_PATH_CHARS}")?$"#
        )).expect("Invalid search template"),

        // sqry graph trace-path "<from>" "<to>" [--max-depth N]
        Regex::new(&format!(
            r#"^sqry graph trace-path "{SAFE_QUOTED_CHARS}" "{SAFE_QUOTED_CHARS}"(\s+--max-depth\s+\d+)?$"#
        )).expect("Invalid trace-path template"),

        // sqry graph callers "<symbol>" [--language <lang>]
        Regex::new(&format!(
            r#"^sqry graph callers "{SAFE_QUOTED_CHARS}"(\s+--language\s+[a-z]+)?$"#
        )).expect("Invalid callers template"),

        // sqry graph callees "<symbol>" [--language <lang>]
        Regex::new(&format!(
            r#"^sqry graph callees "{SAFE_QUOTED_CHARS}"(\s+--language\s+[a-z]+)?$"#
        )).expect("Invalid callees template"),

        // sqry visualize --relation <kind> --symbol "<name>" [--format mermaid|dot|json]
        Regex::new(&format!(
            r#"^sqry visualize --relation\s+(call|import|export|inherit|impl)\s+--symbol\s+"{SAFE_QUOTED_CHARS}"(\s+--format\s+(mermaid|dot|json))?$"#
        )).expect("Invalid visualize template"),

        // sqry index --status [--path "<path>"] [--json]
        Regex::new(&format!(
            r#"^sqry index --status(\s+--path\s+"{SAFE_PATH_CHARS}")?(\s+--json)?$"#
        )).expect("Invalid index template"),
    ]
});

/// Check if a command matches any allowed template.
#[must_use]
pub fn matches_allowed_template(command: &str) -> bool {
    let trimmed = command.trim();
    ALLOWED_TEMPLATES
        .iter()
        .any(|template| template.is_match(trimmed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_basic() {
        assert!(matches_allowed_template("sqry query \"foo\""));
    }

    #[test]
    fn test_query_with_language() {
        assert!(matches_allowed_template(
            "sqry query \"foo\" --language rust"
        ));
    }

    #[test]
    fn test_query_with_multiple_languages() {
        assert!(matches_allowed_template(
            "sqry query \"foo\" --language rust --language python"
        ));
    }

    #[test]
    fn test_query_with_kind() {
        // kind is now part of the query expression (kind:function), not a CLI flag
        assert!(matches_allowed_template("sqry query \"foo kind:function\""));
    }

    #[test]
    fn test_query_with_limit() {
        assert!(matches_allowed_template("sqry query \"foo\" --limit 10"));
    }

    #[test]
    fn test_query_with_path() {
        assert!(matches_allowed_template(
            "sqry query \"foo\" --path \"src/**\""
        ));
    }

    #[test]
    fn test_search_basic() {
        assert!(matches_allowed_template("sqry search \"TODO\""));
    }

    #[test]
    fn test_graph_callers() {
        assert!(matches_allowed_template(
            "sqry graph callers \"authenticate\""
        ));
    }

    #[test]
    fn test_graph_callees() {
        assert!(matches_allowed_template("sqry graph callees \"main\""));
    }

    #[test]
    fn test_trace_path() {
        assert!(matches_allowed_template(
            "sqry graph trace-path \"login\" \"database\""
        ));
    }

    #[test]
    fn test_trace_path_with_depth() {
        assert!(matches_allowed_template(
            "sqry graph trace-path \"login\" \"database\" --max-depth 5"
        ));
    }

    #[test]
    fn test_visualize() {
        assert!(matches_allowed_template(
            "sqry visualize --relation call --symbol \"main\""
        ));
    }

    #[test]
    fn test_visualize_with_format() {
        assert!(matches_allowed_template(
            "sqry visualize --relation call --symbol \"main\" --format mermaid"
        ));
    }

    #[test]
    fn test_index_status() {
        assert!(matches_allowed_template("sqry index --status"));
    }

    #[test]
    fn test_index_status_with_json() {
        assert!(matches_allowed_template("sqry index --status --json"));
    }

    #[test]
    fn test_reject_unknown_command() {
        assert!(!matches_allowed_template("sqry unknown \"foo\""));
    }

    #[test]
    fn test_reject_shell_in_quotes() {
        // Even though this looks like a valid query, the shell command is in quotes
        // The metachar check would catch this, but whitelist also rejects non-matching
        assert!(!matches_allowed_template("sqry query \"foo; rm -rf /\""));
    }

    #[test]
    fn test_kind_in_query_expression() {
        // Kind validation moved to query parser, whitelist just checks syntax
        // Valid kind predicate syntax should pass whitelist
        assert!(matches_allowed_template("sqry query \"kind:function\""));
        // Invalid kind values are caught by query parser, not whitelist
    }
}
