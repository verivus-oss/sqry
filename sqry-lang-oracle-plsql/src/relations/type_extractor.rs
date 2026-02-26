//! Type extraction utilities for Oracle PL/SQL `TypeOf` and Reference edges.
//!
//! PL/SQL types include built-in types (`VARCHAR2`, `NUMBER`, `DATE`, `BOOLEAN`),
//! `%TYPE`/`%ROWTYPE` attribute references, and user-defined types.
//!
//! Due to grammar limitations, type extraction uses text-based pattern matching.

/// Extract the primary type name for `TypeOf` edges.
///
/// Normalizes the type string and returns the primary type.
/// For `%TYPE`/`%ROWTYPE`, returns the full reference (e.g., `employees.name%TYPE`).
#[must_use]
pub fn extract_type_name(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Normalize: collapse whitespace, remove size specifications like VARCHAR2(100)
    let normalized = normalize_type_text(trimmed);
    if normalized.is_empty() {
        return None;
    }
    Some(normalized)
}

/// Extract all referenced type names for References edges.
///
/// For `%TYPE`, extracts the table name (e.g., `employees` from `employees.name%TYPE`).
/// For `%ROWTYPE`, extracts the table/cursor name.
/// For built-in types, returns empty (no References needed).
/// For user-defined types, returns the type name.
#[must_use]
pub fn extract_all_type_names(text: &str) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let upper = trimmed.to_uppercase();

    // Handle %TYPE: extract the table/variable reference
    if upper.contains("%TYPE") && !upper.contains("%ROWTYPE") {
        let before_pct = trimmed.split('%').next().unwrap_or("").trim();
        if let Some(dot_idx) = before_pct.rfind('.') {
            // table.column%TYPE -> reference the table
            let table = before_pct[..dot_idx].trim();
            if !table.is_empty() && is_valid_plsql_identifier(table) {
                return vec![table.to_string()];
            }
        }
        return Vec::new();
    }

    // Handle %ROWTYPE: extract the table/cursor reference
    if upper.contains("%ROWTYPE") {
        let before_pct = trimmed.split('%').next().unwrap_or("").trim();
        if !before_pct.is_empty() && is_valid_plsql_identifier(before_pct) {
            return vec![before_pct.to_string()];
        }
        return Vec::new();
    }

    // Skip built-in types
    let base_type = extract_base_type_name(trimmed);
    if is_plsql_builtin_type(&base_type) {
        return Vec::new();
    }

    // User-defined type
    if !base_type.is_empty() && is_valid_plsql_identifier(&base_type) {
        return vec![base_type];
    }

    Vec::new()
}

/// Check if a type name is a PL/SQL built-in type.
#[must_use]
pub fn is_plsql_builtin_type(name: &str) -> bool {
    let upper = name.to_uppercase();
    matches!(
        upper.as_str(),
        "VARCHAR2"
            | "VARCHAR"
            | "NVARCHAR2"
            | "CHAR"
            | "NCHAR"
            | "CLOB"
            | "NCLOB"
            | "BLOB"
            | "BFILE"
            | "NUMBER"
            | "INTEGER"
            | "INT"
            | "SMALLINT"
            | "FLOAT"
            | "REAL"
            | "DOUBLE"
            | "BINARY_INTEGER"
            | "BINARY_FLOAT"
            | "BINARY_DOUBLE"
            | "PLS_INTEGER"
            | "NATURAL"
            | "NATURALN"
            | "POSITIVE"
            | "POSITIVEN"
            | "SIGNTYPE"
            | "SIMPLE_INTEGER"
            | "SIMPLE_FLOAT"
            | "SIMPLE_DOUBLE"
            | "DATE"
            | "TIMESTAMP"
            | "INTERVAL"
            | "BOOLEAN"
            | "RAW"
            | "LONG"
            | "ROWID"
            | "UROWID"
            | "XMLTYPE"
            | "SYS_REFCURSOR"
            | "REF"
            | "RECORD"
            | "TABLE"
            | "VARRAY"
            | "STRING"
    )
}

/// Normalize type text by removing size specifications and collapsing whitespace.
fn normalize_type_text(text: &str) -> String {
    let mut result = text.to_string();
    // Remove parenthesized size specs: VARCHAR2(100) -> VARCHAR2
    // Don't strip from %TYPE references
    if let Some(paren_idx) = result.find('(')
        && !result.contains('%')
    {
        result = result[..paren_idx].to_string();
    }
    result
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// Extract the base type name (without size specifiers or `%TYPE` suffixes).
fn extract_base_type_name(text: &str) -> String {
    let upper = text.to_uppercase();
    // For %TYPE/%ROWTYPE, the whole thing is the type
    if upper.contains('%') {
        return text.trim().to_string();
    }
    // Remove parenthesized size specs
    let base = if let Some(paren_idx) = text.find('(') {
        text[..paren_idx].trim()
    } else {
        text.trim()
    };
    base.to_string()
}

/// Check if a string looks like a valid PL/SQL identifier.
fn is_valid_plsql_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '#')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_type_name_simple() {
        assert_eq!(extract_type_name("VARCHAR2"), Some("VARCHAR2".to_string()));
        assert_eq!(extract_type_name("NUMBER"), Some("NUMBER".to_string()));
        assert_eq!(extract_type_name("DATE"), Some("DATE".to_string()));
    }

    #[test]
    fn test_extract_type_name_with_size() {
        assert_eq!(
            extract_type_name("VARCHAR2(100)"),
            Some("VARCHAR2".to_string())
        );
        assert_eq!(
            extract_type_name("NUMBER(10,2)"),
            Some("NUMBER".to_string())
        );
    }

    #[test]
    fn test_extract_type_name_pct_type() {
        assert_eq!(
            extract_type_name("employees.name%TYPE"),
            Some("employees.name%TYPE".to_string())
        );
    }

    #[test]
    fn test_extract_type_name_pct_rowtype() {
        assert_eq!(
            extract_type_name("employees%ROWTYPE"),
            Some("employees%ROWTYPE".to_string())
        );
    }

    #[test]
    fn test_extract_all_type_names_builtin() {
        assert!(extract_all_type_names("VARCHAR2").is_empty());
        assert!(extract_all_type_names("NUMBER").is_empty());
        assert!(extract_all_type_names("DATE").is_empty());
        assert!(extract_all_type_names("BOOLEAN").is_empty());
    }

    #[test]
    fn test_extract_all_type_names_pct_type() {
        let names = extract_all_type_names("employees.name%TYPE");
        assert_eq!(names, vec!["employees"]);
    }

    #[test]
    fn test_extract_all_type_names_pct_rowtype() {
        let names = extract_all_type_names("employees%ROWTYPE");
        assert_eq!(names, vec!["employees"]);
    }

    #[test]
    fn test_extract_all_type_names_user_defined() {
        let names = extract_all_type_names("employee_record");
        assert_eq!(names, vec!["employee_record"]);
    }

    #[test]
    fn test_is_plsql_builtin_type() {
        assert!(is_plsql_builtin_type("VARCHAR2"));
        assert!(is_plsql_builtin_type("varchar2"));
        assert!(is_plsql_builtin_type("NUMBER"));
        assert!(is_plsql_builtin_type("DATE"));
        assert!(is_plsql_builtin_type("BOOLEAN"));
        assert!(!is_plsql_builtin_type("my_type"));
        assert!(!is_plsql_builtin_type("employee_record"));
    }

    #[test]
    fn test_empty_and_whitespace() {
        assert!(extract_type_name("").is_none());
        assert!(extract_type_name("   ").is_none());
        assert!(extract_all_type_names("").is_empty());
    }
}
