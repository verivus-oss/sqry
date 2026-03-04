//! String formatting utilities for command assembly.

/// Escape double quotes in a string for shell safety.
#[must_use]
pub fn escape_quotes(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_quotes() {
        assert_eq!(escape_quotes("hello"), "hello");
        assert_eq!(escape_quotes("hello\"world"), "hello\\\"world");
        assert_eq!(escape_quotes("foo\\bar"), "foo\\\\bar");
    }
}
