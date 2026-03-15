//! Error formatting and exit handling for CLI

#[cfg(test)]
mod tests {
    use sqry_core::query::error::{LexError, QueryError};

    // Test exit code mapping for query errors
    #[test]
    fn test_exit_code_mapping() {
        let lex_err = QueryError::Lex(LexError::UnexpectedEof);
        assert_eq!(lex_err.exit_code(), 2);
    }
}
