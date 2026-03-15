//! Template definitions for command assembly.
//!
//! This module contains the template patterns used for validation
//! and documentation purposes.

/// Template pattern documentation.
///
/// Each template shows the expected format of assembled commands.
pub const TEMPLATES: &[(&str, &str)] = &[
    (
        "query",
        r#"sqry query "<expr> [kind:<kind>] [predicate:value]..." [--language <lang>]* [--path "<glob>"] [--limit N]"#,
    ),
    (
        "search",
        r#"sqry search "<pattern>" [--language <lang>]* [--path "<glob>"]"#,
    ),
    (
        "trace-path",
        r#"sqry graph trace-path "<from>" "<to>" [--max-depth N]"#,
    ),
    (
        "direct-callers",
        r#"sqry graph direct-callers "<symbol>" [--language <lang>]"#,
    ),
    (
        "direct-callees",
        r#"sqry graph direct-callees "<symbol>" [--language <lang>]"#,
    ),
    (
        "visualize",
        r#"sqry visualize --relation <kind> --symbol "<name>" [--format mermaid|dot|json]"#,
    ),
    (
        "index-status",
        r#"sqry index --status [--path "<path>"] [--json]"#,
    ),
];

/// Get template documentation by name.
#[must_use]
pub fn get_template(name: &str) -> Option<&'static str> {
    TEMPLATES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, template)| *template)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_template() {
        assert!(get_template("query").is_some());
        assert!(get_template("unknown").is_none());
    }

    #[test]
    fn test_all_templates_exist() {
        assert_eq!(TEMPLATES.len(), 7);
    }
}
