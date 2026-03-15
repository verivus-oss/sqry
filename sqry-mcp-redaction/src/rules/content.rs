//! Content redaction rules for code context and documentation.

/// Redact code context, replacing it with a line count placeholder.
///
/// # Example
///
/// ```rust
/// use sqry_mcp_redaction::rules::redact_code_context;
///
/// let code = "fn main() {\n    println!(\"Hello\");\n}";
/// let redacted = redact_code_context(code, "[REDACTED]");
/// assert_eq!(redacted, "[REDACTED: 3 lines of code]");
/// ```
#[must_use]
pub fn redact_code_context(content: &str, placeholder: &str) -> String {
    let line_count = content.lines().count();
    if line_count == 0 {
        format!("{}: empty code]", placeholder.trim_end_matches(']'))
    } else if line_count == 1 {
        format!("{}: 1 line of code]", placeholder.trim_end_matches(']'))
    } else {
        format!(
            "{}: {} lines of code]",
            placeholder.trim_end_matches(']'),
            line_count
        )
    }
}

/// Redact documentation string.
///
/// # Example
///
/// ```rust
/// use sqry_mcp_redaction::rules::redact_documentation;
///
/// let doc = "/// This is a doc comment explaining the function";
/// let redacted = redact_documentation(doc, "[REDACTED]");
/// assert_eq!(redacted, "[REDACTED: documentation]");
/// ```
#[must_use]
pub fn redact_documentation(content: &str, placeholder: &str) -> String {
    if content.is_empty() {
        format!(
            "{}: empty documentation]",
            placeholder.trim_end_matches(']')
        )
    } else {
        format!("{}: documentation]", placeholder.trim_end_matches(']'))
    }
}

/// Count the number of lines in a string.
#[inline]
#[must_use]
pub fn count_lines(content: &str) -> usize {
    content.lines().count()
}

/// Check if content appears to be code (contains common code patterns).
#[must_use]
pub fn looks_like_code(content: &str) -> bool {
    // Check for common code patterns
    let code_patterns = [
        "fn ",
        "func ",
        "function ",
        "def ",
        "class ",
        "struct ",
        "impl ",
        "pub ",
        "private ",
        "public ",
        "const ",
        "let ",
        "var ",
        "if (",
        "if(",
        "for (",
        "for(",
        "while (",
        "while(",
        "return ",
        "import ",
        "from ",
        "use ",
        "require(",
        "include ",
        "#include",
        "package ",
    ];

    code_patterns
        .iter()
        .any(|pattern| content.contains(pattern))
}

/// Check if content appears to be documentation.
#[must_use]
pub fn looks_like_documentation(content: &str) -> bool {
    // Check for common documentation patterns
    let doc_patterns = [
        "///",
        "//!",
        "/**",
        "/*",
        "'''",
        "\"\"\"",
        "# ",
        "## ",
        "### ",
        "@param",
        "@return",
        "@throws",
        ":param",
        ":return:",
        "Args:",
        "Returns:",
        "Raises:",
        "Example:",
        "Examples:",
        "Note:",
        "Warning:",
    ];

    doc_patterns.iter().any(|pattern| content.contains(pattern))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_code_context() {
        let code = "fn main() {\n    println!(\"Hello\");\n}";
        let redacted = redact_code_context(code, "[REDACTED]");
        assert_eq!(redacted, "[REDACTED: 3 lines of code]");
    }

    #[test]
    fn test_redact_code_context_single_line() {
        let code = "let x = 42;";
        let redacted = redact_code_context(code, "[REDACTED]");
        assert_eq!(redacted, "[REDACTED: 1 line of code]");
    }

    #[test]
    fn test_redact_code_context_empty() {
        let redacted = redact_code_context("", "[REDACTED]");
        assert_eq!(redacted, "[REDACTED: empty code]");
    }

    #[test]
    fn test_redact_documentation() {
        let doc = "/// This is a doc comment";
        let redacted = redact_documentation(doc, "[REDACTED]");
        assert_eq!(redacted, "[REDACTED: documentation]");
    }

    #[test]
    fn test_redact_documentation_empty() {
        let redacted = redact_documentation("", "[REDACTED]");
        assert_eq!(redacted, "[REDACTED: empty documentation]");
    }

    #[test]
    fn test_looks_like_code() {
        assert!(looks_like_code("fn main() {}"));
        assert!(looks_like_code("class Foo:"));
        assert!(looks_like_code("function test() {}"));
        assert!(looks_like_code("def hello():"));
        assert!(!looks_like_code("This is just text."));
    }

    #[test]
    fn test_looks_like_documentation() {
        assert!(looks_like_documentation("/// This is a doc comment"));
        assert!(looks_like_documentation("/**\n * JavaDoc\n */"));
        assert!(looks_like_documentation("Args:\n  x: the value"));
        assert!(!looks_like_documentation("let x = 42;"));
    }

    #[test]
    fn test_count_lines() {
        assert_eq!(count_lines(""), 0);
        assert_eq!(count_lines("one"), 1);
        assert_eq!(count_lines("one\ntwo"), 2);
        assert_eq!(count_lines("one\ntwo\nthree"), 3);
    }

    #[test]
    fn test_custom_placeholder() {
        let code = "fn main() {}";
        let redacted = redact_code_context(code, "[HIDDEN]");
        assert_eq!(redacted, "[HIDDEN: 1 line of code]");
    }
}
