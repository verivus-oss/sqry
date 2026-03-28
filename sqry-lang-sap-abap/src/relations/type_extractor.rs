//! Type extraction utilities for SAP ABAP `TypeOf` and Reference edges.
//!
//! ABAP uses explicit type declarations:
//! - `DATA var TYPE type.`
//! - `DATA var TYPE TABLE OF type.`
//! - `DATA: var1 TYPE type1, var2 TYPE type2.` (colon notation)
//! - `TYPES ty TYPE type.`
//! - `FIELD-SYMBOLS <fs> TYPE type.`
//! - `DATA var LIKE other_var.`

/// Information about an ABAP type declaration.
#[derive(Debug, Clone)]
pub struct AbapTypeDecl {
    /// Variable/type/field-symbol name
    pub var_name: String,
    /// The declared type name
    pub type_name: String,
    /// Whether this is a table type (TYPE TABLE OF / STANDARD TABLE OF / etc.)
    #[allow(dead_code)]
    pub is_table_type: bool,
    /// Base type for table types (the type after TABLE OF)
    pub base_type: Option<String>,
    /// Byte offset of the declaration in the source (reserved for future span creation)
    #[allow(dead_code)]
    pub byte_offset: usize,
    /// Whether this uses LIKE instead of TYPE
    pub is_like: bool,
}

/// Extract type declarations from ABAP source content.
///
/// Scans line-by-line for:
/// - `DATA var TYPE type.`
/// - `DATA var TYPE TABLE OF type.` / `STANDARD TABLE OF` / `SORTED TABLE OF` / `HASHED TABLE OF`
/// - `DATA: var1 TYPE type1, var2 TYPE type2.` (colon notation)
/// - `TYPES ty TYPE type.`
/// - `FIELD-SYMBOLS <fs> TYPE type.`
/// - `DATA var LIKE other_var.`
#[must_use]
pub fn extract_type_declarations(content: &str) -> Vec<AbapTypeDecl> {
    let mut decls = Vec::new();
    let mut offset = 0usize;

    for line in content.lines() {
        let trimmed = line.trim();
        let upper = trimmed.to_uppercase();

        // Skip comments
        if trimmed.starts_with('*') || trimmed.starts_with('"') {
            offset += line.len() + 1;
            continue;
        }

        // Check for declaration keywords
        let keyword_len = if upper.starts_with("DATA ") || upper.starts_with("DATA:") {
            4
        } else if upper.starts_with("TYPES ") || upper.starts_with("TYPES:") {
            5
        } else if upper.starts_with("FIELD-SYMBOLS ") || upper.starts_with("FIELD-SYMBOLS:") {
            14
        } else if upper.starts_with("CLASS-DATA ") || upper.starts_with("CLASS-DATA:") {
            10
        } else if upper.starts_with("CONSTANTS ") || upper.starts_with("CONSTANTS:") {
            9
        } else {
            offset += line.len() + 1;
            continue;
        };

        let rest = trimmed.get(keyword_len..).unwrap_or("").trim();
        // Handle colon notation: strip leading colon
        let rest = rest.trim_start_matches(':').trim();

        // Split by comma for colon notation (DATA: a TYPE t1, b TYPE t2.)
        // Also handle single declarations
        for decl_text in split_colon_notation(rest) {
            let decl_text = decl_text.trim().trim_end_matches('.');
            if decl_text.is_empty() {
                continue;
            }

            if let Some(decl) = parse_single_declaration(decl_text, offset) {
                decls.push(decl);
            }
        }

        offset += line.len() + 1;
    }

    decls
}

/// Parse a single declaration like `var TYPE type` or `<fs> TYPE type`.
fn parse_single_declaration(text: &str, byte_offset: usize) -> Option<AbapTypeDecl> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }

    let var_name = parts[0].trim();
    // Clean field-symbol angle brackets for display but keep for identification
    if var_name.is_empty() {
        return None;
    }

    let keyword = parts[1].to_uppercase();

    match keyword.as_str() {
        "TYPE" => {
            let type_parts = &parts[2..];
            parse_type_declaration(var_name, type_parts, byte_offset)
        }
        "LIKE" => {
            // LIKE references another variable's type
            let like_ref = parts[2..]
                .join(" ")
                .trim_end_matches('.')
                .trim()
                .to_string();
            if like_ref.is_empty() {
                return None;
            }
            Some(AbapTypeDecl {
                var_name: var_name.to_string(),
                type_name: like_ref.clone(),
                is_table_type: false,
                base_type: None,
                byte_offset,
                is_like: true,
            })
        }
        _ => None,
    }
}

/// Parse type after `TYPE` keyword.
fn parse_type_declaration(
    var_name: &str,
    type_parts: &[&str],
    byte_offset: usize,
) -> Option<AbapTypeDecl> {
    if type_parts.is_empty() {
        return None;
    }

    let joined = type_parts.join(" ");
    let upper_joined = joined.to_uppercase();

    // Check for TABLE OF patterns
    // TYPE TABLE OF type / TYPE STANDARD TABLE OF type / TYPE SORTED TABLE OF type / TYPE HASHED TABLE OF type
    if let Some(table_of_idx) = upper_joined.find("TABLE OF") {
        let base_type_text = joined.get(table_of_idx + 8..).unwrap_or("").trim();
        // Clean: remove WITH KEY clause, etc.
        let base_type = base_type_text
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches('.')
            .trim();

        if base_type.is_empty() {
            return None;
        }

        // The full type name includes the table qualifier
        let full_type = joined.trim_end_matches('.').trim().to_string();

        return Some(AbapTypeDecl {
            var_name: var_name.to_string(),
            type_name: full_type,
            is_table_type: true,
            base_type: Some(base_type.to_string()),
            byte_offset,
            is_like: false,
        });
    }

    // Check for RANGE OF pattern
    if upper_joined.starts_with("RANGE OF") {
        let base_type = joined
            .get(9..)
            .unwrap_or("")
            .trim()
            .trim_end_matches('.')
            .trim();
        if base_type.is_empty() {
            return None;
        }
        return Some(AbapTypeDecl {
            var_name: var_name.to_string(),
            type_name: joined.trim_end_matches('.').trim().to_string(),
            is_table_type: true,
            base_type: Some(base_type.to_string()),
            byte_offset,
            is_like: false,
        });
    }

    // Check for REF TO <class> pattern
    if type_parts.len() >= 3
        && type_parts[0].eq_ignore_ascii_case("REF")
        && type_parts[1].eq_ignore_ascii_case("TO")
    {
        let class_name = type_parts[2].trim_end_matches('.').trim();
        if class_name.is_empty() {
            return None;
        }
        let full_type = format!("REF TO {class_name}");
        return Some(AbapTypeDecl {
            var_name: var_name.to_string(),
            type_name: full_type,
            is_table_type: false,
            base_type: Some(class_name.to_string()),
            byte_offset,
            is_like: false,
        });
    }

    // Simple type: first token is the type name
    let type_name = type_parts[0].trim_end_matches('.').trim();
    if type_name.is_empty() {
        return None;
    }

    Some(AbapTypeDecl {
        var_name: var_name.to_string(),
        type_name: type_name.to_string(),
        is_table_type: false,
        base_type: None,
        byte_offset,
        is_like: false,
    })
}

/// Split colon notation declarations by comma, respecting nested structures.
fn split_colon_notation(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut paren_depth: i32 = 0;

    for ch in text.chars() {
        match ch {
            '(' => {
                paren_depth += 1;
                current.push(ch);
            }
            ')' => {
                paren_depth -= 1;
                current.push(ch);
            }
            ',' if paren_depth == 0 => {
                parts.push(current.clone());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current);
    }
    parts
}

/// Check if a type name is an ABAP built-in type.
#[must_use]
pub fn is_abap_builtin_type(name: &str) -> bool {
    let upper = name.to_uppercase();
    matches!(
        upper.as_str(),
        "STRING"
            | "XSTRING"
            | "I"
            | "INT8"
            | "F"
            | "D"
            | "T"
            | "N"
            | "C"
            | "X"
            | "P"
            | "DECFLOAT16"
            | "DECFLOAT34"
            | "B"
            | "S"
            | "ABAP_BOOL"
            | "FLAG"
            | "CHAR1"
            | "CHAR2"
            | "CHAR10"
            | "CHAR20"
            | "CHAR30"
            | "CHAR50"
            | "CHAR70"
            | "CHAR80"
            | "CHAR128"
            | "CHAR255"
            | "NUMC2"
            | "NUMC4"
            | "NUMC10"
            | "INT1"
            | "INT2"
            | "INT4"
            | "NUMERIC"
            | "CLIKE"
            | "CSEQUENCE"
            | "XSEQUENCE"
            | "SIMPLE"
            | "ANY"
            | "DATA"
            | "OBJECT"
            | "REF"
            | "SY"
            | "SYST"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_data_declaration() {
        let content = "DATA lv_name TYPE string.\n";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].var_name, "lv_name");
        assert_eq!(decls[0].type_name, "string");
        assert!(!decls[0].is_table_type);
    }

    #[test]
    fn test_data_table_of() {
        let content = "DATA lt_data TYPE TABLE OF zstructure.\n";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].var_name, "lt_data");
        assert!(decls[0].is_table_type);
        assert_eq!(decls[0].base_type.as_deref(), Some("zstructure"));
    }

    #[test]
    fn test_data_standard_table() {
        let content = "DATA lt_items TYPE STANDARD TABLE OF zmaterial.\n";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        assert!(decls[0].is_table_type);
        assert_eq!(decls[0].base_type.as_deref(), Some("zmaterial"));
    }

    #[test]
    fn test_colon_notation() {
        let content = "DATA: lv_count TYPE i, lv_name TYPE string.\n";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].var_name, "lv_count");
        assert_eq!(decls[0].type_name, "i");
        assert_eq!(decls[1].var_name, "lv_name");
        assert_eq!(decls[1].type_name, "string");
    }

    #[test]
    fn test_field_symbols() {
        let content = "FIELD-SYMBOLS <fs_item> TYPE zstructure.\n";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].var_name, "<fs_item>");
        assert_eq!(decls[0].type_name, "zstructure");
    }

    #[test]
    fn test_types_declaration() {
        let content = "TYPES ty_name TYPE string.\n";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].var_name, "ty_name");
        assert_eq!(decls[0].type_name, "string");
    }

    #[test]
    fn test_like_declaration() {
        let content = "DATA lv_copy LIKE lv_original.\n";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].var_name, "lv_copy");
        assert_eq!(decls[0].type_name, "lv_original");
        assert!(decls[0].is_like);
    }

    #[test]
    fn test_builtin_types() {
        assert!(is_abap_builtin_type("string"));
        assert!(is_abap_builtin_type("STRING"));
        assert!(is_abap_builtin_type("i"));
        assert!(is_abap_builtin_type("f"));
        assert!(is_abap_builtin_type("d"));
        assert!(is_abap_builtin_type("t"));
        assert!(!is_abap_builtin_type("zstructure"));
        assert!(!is_abap_builtin_type("zmaterial"));
    }

    #[test]
    fn test_skip_comments() {
        let content = "* This is a comment\nDATA lv_val TYPE string.\n";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].var_name, "lv_val");
    }

    #[test]
    fn test_class_data() {
        let content = "CLASS-DATA gv_instance TYPE REF TO zcl_myclass.\n";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].var_name, "gv_instance");
    }

    #[test]
    fn test_ref_to_type() {
        let content = "DATA lo_obj TYPE REF TO zcl_processor.\n";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].var_name, "lo_obj");
        assert_eq!(decls[0].type_name, "REF TO zcl_processor");
        assert_eq!(decls[0].base_type, Some("zcl_processor".to_string()));
        assert!(!decls[0].is_table_type);
    }

    #[test]
    fn test_ref_to_interface() {
        let content = "DATA lo_intf TYPE REF TO zif_handler.\n";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].type_name, "REF TO zif_handler");
        assert_eq!(decls[0].base_type, Some("zif_handler".to_string()));
    }
}
