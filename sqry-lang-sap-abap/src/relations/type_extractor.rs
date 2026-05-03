//! Type extraction utilities for SAP ABAP `TypeOf` and Reference edges.
//!
//! ABAP uses explicit type declarations:
//! - `DATA var TYPE type.`
//! - `DATA var TYPE TABLE OF type.`
//! - `DATA: var1 TYPE type1, var2 TYPE type2.` (colon notation)
//! - `TYPES ty TYPE type.`
//! - `FIELD-SYMBOLS <fs> TYPE type.`
//! - `DATA var LIKE other_var.`
//!
//! ## Class-Attribute Context (REQ:R0001..R0005, R0009, R0023)
//!
//! When a `DATA` / `CLASS-DATA` / `CONSTANTS` declaration appears inside a
//! `CLASS <name> DEFINITION ... ENDCLASS.` block, the resulting [`AbapTypeDecl`]
//! must be tagged with the enclosing class name, the section visibility
//! (PUBLIC / PRIVATE / PROTECTED), and a static flag (true for `CLASS-DATA`,
//! false for `DATA`). Top-level declarations (report locals) carry no class
//! context.

use sqry_core::graph::{Position, Span};

/// Information about an ABAP type declaration.
//
// The bool fields encode independent facets of an ABAP declaration
// (table-vs-scalar, `LIKE` vs `TYPE`, class-attribute vs report-local,
// static vs instance, immutable vs mutable). They are documented
// individually and used as a flat data carrier rather than a state
// machine, so collapsing them into an enum or bitflags would obscure
// rather than improve the type.
#[allow(clippy::struct_excessive_bools)]
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
    /// Enclosing class name when the declaration sits inside a
    /// `CLASS <name> DEFINITION` block; `None` for top-level (report-local)
    /// declarations.
    pub enclosing_class: Option<String>,
    /// True when the declaration is a class attribute (`DATA` /
    /// `CLASS-DATA` / `CONSTANTS` inside a `CLASS DEFINITION`).
    pub is_class_attribute: bool,
    /// True for `CLASS-DATA` (static class-level state); always false for
    /// instance `DATA`, top-level `DATA`, and report locals.
    pub is_static: bool,
    /// Visibility of the enclosing section: `Some("public")`,
    /// `Some("private")`, `Some("protected")`, or `None` outside a class.
    pub visibility: Option<String>,
    /// Span of the declaration line.
    pub span: Option<Span>,
    /// True when the declaration is immutable: `CONSTANTS` (always) or class
    /// `DATA` / `CLASS-DATA` carrying the `READ-ONLY` attribute.
    pub is_immutable: bool,
}

/// Identify the declaration keyword on a line.
///
/// Returns `(keyword_len, is_static, is_immutable_keyword,
/// is_class_attribute_eligible)`:
/// - `is_static` is true for `CLASS-DATA`
/// - `is_immutable_keyword` is true for `CONSTANTS`
/// - `is_class_attribute_eligible` is true only for keywords that can
///   produce a class attribute when nested in a `CLASS DEFINITION` block:
///   `DATA`, `CLASS-DATA`, `CONSTANTS`. `TYPES` (type aliasing) and
///   `FIELD-SYMBOLS` (reference binding) MUST NOT be tagged as class
///   attributes — they have no field semantics. They retain their
///   pre-existing report-local Variable behavior.
fn classify_decl_keyword(upper: &str) -> Option<(usize, bool, bool, bool)> {
    if upper.starts_with("DATA ") || upper.starts_with("DATA:") {
        Some((4, false, false, true))
    } else if upper.starts_with("TYPES ") || upper.starts_with("TYPES:") {
        Some((5, false, false, false))
    } else if upper.starts_with("FIELD-SYMBOLS ") || upper.starts_with("FIELD-SYMBOLS:") {
        Some((14, false, false, false))
    } else if upper.starts_with("CLASS-DATA ") || upper.starts_with("CLASS-DATA:") {
        Some((10, true, false, true))
    } else if upper.starts_with("CONSTANTS ") || upper.starts_with("CONSTANTS:") {
        Some((9, false, true, true))
    } else {
        None
    }
}

/// Detect a `CLASS <name> DEFINITION ...` line and return the class name.
///
/// Skips `CLASS <name> IMPLEMENTATION` (which contains METHOD bodies, not
/// attribute declarations). Also skips forward-declaration forms that do
/// not open a class body:
/// - `CLASS <name> DEFINITION DEFERRED.`
/// - `CLASS <name> DEFINITION LOAD.`
///
/// Returning `None` for these prevents the line-state machine from leaking
/// class context onto subsequent top-level declarations.
fn parse_class_definition_open(upper: &str, original: &str) -> Option<String> {
    if !upper.starts_with("CLASS ") {
        return None;
    }
    if !upper.contains(" DEFINITION") {
        return None;
    }
    // Forward-declaration forms — class body is not opened on these lines.
    // Tokenize the upper-cased line and look for DEFERRED / LOAD as the
    // token immediately after DEFINITION.
    let upper_tokens: Vec<&str> = upper.trim_end_matches('.').split_whitespace().collect();
    if let Some(def_idx) = upper_tokens.iter().position(|t| *t == "DEFINITION")
        && let Some(next) = upper_tokens.get(def_idx + 1)
        && (*next == "DEFERRED" || *next == "LOAD")
    {
        return None;
    }
    let after_keyword = original.get(6..)?.trim_start();
    let name = after_keyword.split_whitespace().next()?;
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

/// Detect a section keyword and return its lowercase visibility name.
fn parse_section_keyword(upper: &str) -> Option<&'static str> {
    let stripped = upper.trim_end_matches('.').trim();
    match stripped {
        "PUBLIC SECTION" => Some("public"),
        "PRIVATE SECTION" => Some("private"),
        "PROTECTED SECTION" => Some("protected"),
        _ => None,
    }
}

/// Detect the READ-ONLY attribute on a declaration tail.
///
/// READ-ONLY may appear after the type, e.g.
/// `DATA name TYPE string READ-ONLY.`. Strip it from the rest and return
/// whether it was present so the type-parse logic does not consume the
/// keyword as part of the type.
fn extract_read_only(rest: &str) -> (String, bool) {
    let upper = rest.to_uppercase();
    if let Some(idx) = upper.find(" READ-ONLY") {
        let mut cleaned = String::with_capacity(rest.len());
        cleaned.push_str(&rest[..idx]);
        let tail = &rest[idx + " READ-ONLY".len()..];
        cleaned.push_str(tail);
        (cleaned, true)
    } else {
        (rest.to_string(), false)
    }
}

fn span_from_line(line_idx: usize, len: usize) -> Span {
    Span::new(Position::new(line_idx, 0), Position::new(line_idx, len))
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
/// - `CLASS-DATA <attr> TYPE <type>.` (static class attribute)
/// - `CONSTANTS <name> TYPE <type> VALUE <v>.` (immutable, in or out of a class)
///
/// Tracks `CLASS <name> DEFINITION ... ENDCLASS.` blocks plus section
/// keywords (`PUBLIC SECTION.` / `PRIVATE SECTION.` /
/// `PROTECTED SECTION.`) to tag each declaration with its enclosing
/// class, visibility, static-ness, and immutability.
#[must_use]
pub fn extract_type_declarations(content: &str) -> Vec<AbapTypeDecl> {
    let mut decls = Vec::new();
    let mut offset = 0usize;
    let mut current_class: Option<String> = None;
    let mut current_section: Option<&'static str> = None;

    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let upper = trimmed.to_uppercase();

        // Skip comments
        if trimmed.starts_with('*') || trimmed.starts_with('"') {
            offset += line.len() + 1;
            continue;
        }

        // Class context tracking: handle CLASS <n> DEFINITION ... and
        // ENDCLASS. before any declaration parsing. CLASS IMPLEMENTATION
        // blocks are deliberately not entered as attribute context —
        // attribute declarations only live in DEFINITION blocks; method
        // bodies in IMPLEMENTATION may contain local DATA which must
        // remain report-local-style (Variable + TypeOfContext::Variable).
        if let Some(name) = parse_class_definition_open(&upper, trimmed) {
            current_class = Some(name);
            current_section = None;
            offset += line.len() + 1;
            continue;
        }
        if upper.starts_with("ENDCLASS") {
            current_class = None;
            current_section = None;
            offset += line.len() + 1;
            continue;
        }
        if let Some(vis) = parse_section_keyword(&upper) {
            current_section = Some(vis);
            offset += line.len() + 1;
            continue;
        }

        // Check for declaration keywords
        let Some((keyword_len, kw_is_static, kw_is_immutable, kw_class_attr_eligible)) =
            classify_decl_keyword(&upper)
        else {
            offset += line.len() + 1;
            continue;
        };

        let rest = trimmed.get(keyword_len..).unwrap_or("").trim();
        // Handle colon notation: strip leading colon
        let rest = rest.trim_start_matches(':').trim();

        let in_class = current_class.is_some();
        // TYPES (type aliasing) and FIELD-SYMBOLS (reference binding) are
        // never class attributes regardless of nesting context.
        let is_class_attribute = in_class && kw_class_attr_eligible;
        let is_static = is_class_attribute && kw_is_static;
        let visibility = if is_class_attribute {
            current_section.map(str::to_string)
        } else {
            None
        };
        let span = Some(span_from_line(line_idx, line.len()));

        // Split by comma for colon notation (DATA: a TYPE t1, b TYPE t2.)
        // first, then apply READ-ONLY stripping per-individual-declaration.
        // READ-ONLY binds to the single declaration it follows, so applying
        // it line-wide would leak immutability across siblings (e.g.
        // `DATA: a TYPE string READ-ONLY, b TYPE i.` — only `a` is
        // immutable).
        for decl_text in split_colon_notation(rest) {
            let decl_text = decl_text.trim().trim_end_matches('.');
            if decl_text.is_empty() {
                continue;
            }

            // Per-decl READ-ONLY strip + immutability flag.
            let (decl_clean, read_only) = extract_read_only(decl_text);
            let decl_clean = decl_clean.trim().trim_end_matches('.');

            // READ-ONLY is only meaningful for class-attribute-eligible
            // keywords inside a class; CONSTANTS is unconditionally
            // immutable from the keyword itself.
            let is_immutable = kw_is_immutable || (is_class_attribute && read_only);

            if let Some(mut decl) = parse_single_declaration(decl_clean, offset) {
                decl.enclosing_class.clone_from(&current_class);
                decl.is_class_attribute = is_class_attribute;
                decl.is_static = is_static;
                decl.visibility.clone_from(&visibility);
                decl.span = span;
                decl.is_immutable = is_immutable;
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
                enclosing_class: None,
                is_class_attribute: false,
                is_static: false,
                visibility: None,
                span: None,
                is_immutable: false,
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
            enclosing_class: None,
            is_class_attribute: false,
            is_static: false,
            visibility: None,
            span: None,
            is_immutable: false,
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
            enclosing_class: None,
            is_class_attribute: false,
            is_static: false,
            visibility: None,
            span: None,
            is_immutable: false,
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
            enclosing_class: None,
            is_class_attribute: false,
            is_static: false,
            visibility: None,
            span: None,
            is_immutable: false,
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
        enclosing_class: None,
        is_class_attribute: false,
        is_static: false,
        visibility: None,
        span: None,
        is_immutable: false,
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

    // -------------------------------------------------------------------
    // C2_OTHER_ABAP class-attribute context tagging (FAILING ACs)
    // REQ:R0001..R0005, R0009 (class-attrs vs locals), R0023
    //
    // These tests fail until the extractor is refactored to walk
    // CLASS DEFINITION ancestry + section state and tag declarations
    // with enclosing_class / is_class_attribute / is_static / visibility
    // / is_immutable.
    // -------------------------------------------------------------------

    #[test]
    fn test_top_level_data_is_report_local() {
        // REQ:R0009 — top-level DATA outside any CLASS DEFINITION must
        // remain a report-local Variable.
        let content = "DATA lv_total TYPE i.\n";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        let d = &decls[0];
        assert_eq!(d.var_name, "lv_total");
        assert!(
            !d.is_class_attribute,
            "top-level DATA must not be a class attribute"
        );
        assert!(!d.is_static);
        assert!(d.enclosing_class.is_none());
        assert!(d.visibility.is_none());
        assert!(!d.is_immutable);
        assert!(d.span.is_some(), "span must be populated for line tracking");
    }

    #[test]
    fn test_class_data_attribute_public_section() {
        // REQ:R0001..R0003 — DATA inside PUBLIC SECTION of a CLASS
        // DEFINITION → class attribute, instance (not static), public.
        let content = r"
CLASS zcl_foo DEFINITION PUBLIC.
  PUBLIC SECTION.
    DATA: gv_x TYPE i.
ENDCLASS.
";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        let d = &decls[0];
        assert_eq!(d.var_name, "gv_x");
        assert_eq!(d.type_name, "i");
        assert!(
            d.is_class_attribute,
            "DATA inside CLASS DEFINITION must be class attribute"
        );
        assert!(
            !d.is_static,
            "DATA (not CLASS-DATA) must be instance, not static"
        );
        assert_eq!(d.enclosing_class.as_deref(), Some("zcl_foo"));
        assert_eq!(d.visibility.as_deref(), Some("public"));
        assert!(!d.is_immutable);
    }

    #[test]
    fn test_class_data_static_attribute_protected_section() {
        // REQ:R0001..R0003 — CLASS-DATA in PROTECTED SECTION → static + protected.
        let content = r"
CLASS zcl_foo DEFINITION PUBLIC.
  PROTECTED SECTION.
    CLASS-DATA: gv_y TYPE i.
ENDCLASS.
";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        let d = &decls[0];
        assert_eq!(d.var_name, "gv_y");
        assert!(d.is_class_attribute);
        assert!(d.is_static, "CLASS-DATA must be static");
        assert_eq!(d.enclosing_class.as_deref(), Some("zcl_foo"));
        assert_eq!(d.visibility.as_deref(), Some("protected"));
    }

    #[test]
    fn test_private_section_visibility() {
        // REQ:R0001..R0003 — PRIVATE SECTION threading.
        let content = r"
CLASS zcl_foo DEFINITION PUBLIC.
  PRIVATE SECTION.
    DATA: mv_state TYPE i.
ENDCLASS.
";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        let d = &decls[0];
        assert_eq!(d.visibility.as_deref(), Some("private"));
        assert!(d.is_class_attribute);
        assert!(!d.is_static);
    }

    #[test]
    fn test_constants_inside_class_is_immutable() {
        // REQ:R0004 — CONSTANTS inside class → is_immutable=true (Constant).
        let content = r"
CLASS zcl_foo DEFINITION PUBLIC.
  PUBLIC SECTION.
    CONSTANTS: c_max TYPE i VALUE 100.
ENDCLASS.
";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        let d = &decls[0];
        assert_eq!(d.var_name, "c_max");
        assert!(d.is_class_attribute);
        assert!(d.is_immutable, "CONSTANTS must be immutable");
        assert!(
            !d.is_static,
            "CONSTANTS need not be static; static is reserved for CLASS-DATA"
        );
        assert_eq!(d.enclosing_class.as_deref(), Some("zcl_foo"));
        assert_eq!(d.visibility.as_deref(), Some("public"));
    }

    #[test]
    fn test_read_only_data_marks_immutable() {
        // REQ:R0004 — READ-ONLY DATA inside class → is_immutable=true.
        let content = r"
CLASS zcl_foo DEFINITION PUBLIC.
  PUBLIC SECTION.
    DATA: gv_label TYPE string READ-ONLY.
ENDCLASS.
";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        let d = &decls[0];
        assert_eq!(d.var_name, "gv_label");
        assert_eq!(d.type_name, "string");
        assert!(d.is_class_attribute);
        assert!(d.is_immutable, "READ-ONLY DATA must be immutable");
    }

    #[test]
    fn test_top_level_constants_have_no_class_context() {
        // REQ:R0004, R0009 — top-level CONSTANTS still immutable, but no
        // class context → caller will keep it as Variable + Variable edge.
        let content = "CONSTANTS c_year TYPE i VALUE 2026.\n";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        let d = &decls[0];
        assert_eq!(d.var_name, "c_year");
        assert!(!d.is_class_attribute);
        assert!(d.is_immutable, "CONSTANTS keyword always implies immutable");
        assert!(d.enclosing_class.is_none());
        assert!(d.visibility.is_none());
    }

    #[test]
    fn test_endclass_clears_context() {
        // REQ:R0001 — declarations after ENDCLASS revert to report-local.
        let content = r"
CLASS zcl_foo DEFINITION PUBLIC.
  PUBLIC SECTION.
    DATA: gv_x TYPE i.
ENDCLASS.

DATA lv_outside TYPE string.
";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 2);
        assert!(decls[0].is_class_attribute, "first decl is in class");
        assert_eq!(decls[0].enclosing_class.as_deref(), Some("zcl_foo"));
        assert!(
            !decls[1].is_class_attribute,
            "decl after ENDCLASS is report-local"
        );
        assert!(decls[1].enclosing_class.is_none());
        assert!(decls[1].visibility.is_none());
    }

    #[test]
    fn test_class_implementation_does_not_count_as_attribute_context() {
        // REQ:R0009 — DATA inside CLASS IMPLEMENTATION (i.e. local to a
        // method body) must remain a Variable. Only CLASS DEFINITION
        // hosts attribute declarations.
        let content = r"
CLASS zcl_foo IMPLEMENTATION.
  METHOD do_work.
    DATA lv_local TYPE i.
  ENDMETHOD.
ENDCLASS.
";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        let d = &decls[0];
        assert_eq!(d.var_name, "lv_local");
        assert!(
            !d.is_class_attribute,
            "DATA in CLASS IMPLEMENTATION method body must remain Variable"
        );
        assert!(d.enclosing_class.is_none());
    }

    #[test]
    fn test_section_change_within_class() {
        // REQ:R0001..R0003 — section visibility transitions are tracked
        // mid-class. PUBLIC then PRIVATE on different attributes.
        let content = r"
CLASS zcl_foo DEFINITION PUBLIC.
  PUBLIC SECTION.
    DATA: gv_a TYPE i.
  PRIVATE SECTION.
    DATA: mv_b TYPE string.
    CLASS-DATA: gv_c TYPE i.
ENDCLASS.
";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 3);
        assert_eq!(decls[0].var_name, "gv_a");
        assert_eq!(decls[0].visibility.as_deref(), Some("public"));
        assert!(!decls[0].is_static);
        assert_eq!(decls[1].var_name, "mv_b");
        assert_eq!(decls[1].visibility.as_deref(), Some("private"));
        assert!(!decls[1].is_static);
        assert_eq!(decls[2].var_name, "gv_c");
        assert_eq!(decls[2].visibility.as_deref(), Some("private"));
        assert!(decls[2].is_static);
    }

    #[test]
    fn test_inheriting_class_uses_declaring_qualifier() {
        // REQ:R0023, AC-6 — when a subclass declares its own DATA, the
        // qualifier is the declaring (sub)class, not the parent.
        let content = r"
CLASS zcl_parent DEFINITION PUBLIC.
  PUBLIC SECTION.
    DATA: gv_inherited TYPE i.
ENDCLASS.

CLASS zcl_child DEFINITION INHERITING FROM zcl_parent.
  PUBLIC SECTION.
    DATA: gv_own TYPE string.
ENDCLASS.
";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].var_name, "gv_inherited");
        assert_eq!(decls[0].enclosing_class.as_deref(), Some("zcl_parent"));
        assert_eq!(decls[1].var_name, "gv_own");
        assert_eq!(
            decls[1].enclosing_class.as_deref(),
            Some("zcl_child"),
            "subclass-declared attribute uses the declaring (sub)class qualifier"
        );
    }

    #[test]
    fn test_class_definition_deferred_does_not_open_context() {
        // Forward declaration `CLASS X DEFINITION DEFERRED.` must NOT
        // enter class context. Subsequent top-level DATA must remain
        // report-local.
        let content = r"
CLASS zcl_foo DEFINITION DEFERRED.

DATA lv_after TYPE i.
";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        let d = &decls[0];
        assert_eq!(d.var_name, "lv_after");
        assert!(
            !d.is_class_attribute,
            "DATA after CLASS DEFINITION DEFERRED must remain report-local"
        );
        assert!(d.enclosing_class.is_none());
        assert!(d.visibility.is_none());
    }

    #[test]
    fn test_class_definition_load_does_not_open_context() {
        // `CLASS X DEFINITION LOAD.` is also a forward-loading hint, no
        // class body is opened on this line.
        let content = r"
CLASS zcl_foo DEFINITION LOAD.

DATA lv_after TYPE string.
";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        let d = &decls[0];
        assert_eq!(d.var_name, "lv_after");
        assert!(
            !d.is_class_attribute,
            "DATA after CLASS DEFINITION LOAD must remain report-local"
        );
        assert!(d.enclosing_class.is_none());
    }

    #[test]
    fn test_types_inside_class_is_not_class_attribute() {
        // TYPES is type aliasing, not a field — must NOT be tagged as a
        // class attribute even when nested in CLASS DEFINITION.
        let content = r"
CLASS zcl_foo DEFINITION PUBLIC.
  PUBLIC SECTION.
    TYPES: ty_alias TYPE string.
ENDCLASS.
";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        let d = &decls[0];
        assert_eq!(d.var_name, "ty_alias");
        assert!(
            !d.is_class_attribute,
            "TYPES is type aliasing, must not be a class attribute"
        );
        assert!(!d.is_static);
        assert!(!d.is_immutable);
        assert!(d.visibility.is_none());
    }

    #[test]
    fn test_field_symbols_inside_class_is_not_class_attribute() {
        // FIELD-SYMBOLS is reference binding, not a field — must NOT be
        // tagged as a class attribute.
        let content = r"
CLASS zcl_foo DEFINITION PUBLIC.
  PUBLIC SECTION.
    FIELD-SYMBOLS: <fs_item> TYPE i.
ENDCLASS.
";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 1);
        let d = &decls[0];
        assert_eq!(d.var_name, "<fs_item>");
        assert!(
            !d.is_class_attribute,
            "FIELD-SYMBOLS is reference binding, must not be a class attribute"
        );
        assert!(!d.is_static);
        assert!(d.visibility.is_none());
    }

    #[test]
    fn test_read_only_per_decl_in_colon_notation() {
        // REQ:R0004 — READ-ONLY binds to the single declaration it
        // follows. In `DATA: a TYPE string READ-ONLY, b TYPE i.` only
        // `a` is immutable. Pre-fix the line-wide strip leaked
        // immutability across siblings.
        let content = r"
CLASS zcl_foo DEFINITION PUBLIC.
  PUBLIC SECTION.
    DATA: a TYPE string READ-ONLY, b TYPE i.
ENDCLASS.
";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 2);
        let a = &decls[0];
        let b = &decls[1];
        assert_eq!(a.var_name, "a");
        assert!(a.is_class_attribute);
        assert!(a.is_immutable, "a has READ-ONLY → immutable");
        assert_eq!(b.var_name, "b");
        assert!(b.is_class_attribute);
        assert!(
            !b.is_immutable,
            "b has no READ-ONLY → must not inherit immutability from sibling"
        );
    }

    #[test]
    fn test_colon_notation_class_attributes_share_context() {
        // REQ:R0005 — multi-attribute colon notation inside a class block
        // must apply the same enclosing_class / visibility / static flags
        // to every comma-separated declaration on the line.
        let content = r"
CLASS zcl_foo DEFINITION PUBLIC.
  PUBLIC SECTION.
    DATA: gv_a TYPE i, gv_b TYPE string.
ENDCLASS.
";
        let decls = extract_type_declarations(content);
        assert_eq!(decls.len(), 2);
        for d in &decls {
            assert!(d.is_class_attribute);
            assert!(!d.is_static);
            assert_eq!(d.enclosing_class.as_deref(), Some("zcl_foo"));
            assert_eq!(d.visibility.as_deref(), Some("public"));
        }
    }
}
