//! `PHPDoc` comment parsing for PHP type annotations
//!
//! Extracts type information from `PHPDoc` comments (@param, @return, @var)
//! to enable `TypeOf` and Reference edge creation.

use tree_sitter::Node;

/// Parsed `PHPDoc` tags (Phase 1: @param, @return, @var only)
#[derive(Debug, Default)]
pub struct PhpDocTags {
    /// @param Type $name - description
    pub params: Vec<ParamTag>,
    /// @return Type description
    pub returns: Option<String>,
    /// @var Type
    pub var_type: Option<String>,
}

#[derive(Debug)]
pub struct ParamTag {
    pub name: String,
    pub type_str: String,
    #[allow(dead_code)] // Reserved for future use
    pub description: Option<String>,
}

/// Extract `PHPDoc` comment immediately preceding a node
/// Returns None if no `PHPDoc` comment found
///
/// Handles both direct comments and comments on export wrappers.
pub fn extract_phpdoc_comment(node: Node, content: &[u8]) -> Option<String> {
    // Try to extract from current node first
    if let Some(comment) = try_extract_comment(node, content) {
        return Some(comment);
    }

    // If node is wrapped in modifiers (public, protected, private, static, abstract, final),
    // check parent's preceding comment
    if let Some(parent) = node.parent()
        && matches!(
            parent.kind(),
            "visibility_modifier" | "static_modifier" | "abstract_modifier" | "final_modifier"
        )
    {
        return try_extract_comment(parent, content);
    }

    None
}

/// Helper to extract comment from a specific node
/// ADJACENCY RULE: `PHPDoc` must be within 1 blank line of the target node
fn try_extract_comment(node: Node, content: &[u8]) -> Option<String> {
    let node_start_line = node.start_position().row;
    let mut prev_sibling = node.prev_sibling();

    while let Some(sibling) = prev_sibling {
        if sibling.kind() == "comment" {
            let comment_text = sibling.utf8_text(content).ok()?;

            // Check if it's a PHPDoc comment (/** */)
            if comment_text.starts_with("/**") && comment_text.ends_with("*/") {
                // Check line distance (allow max 1 blank line between comment and node)
                let comment_end_line = sibling.end_position().row;
                let line_distance = node_start_line.saturating_sub(comment_end_line);

                if line_distance <= 2 {
                    // <= 2 means adjacent or 1 blank line in between
                    return Some(comment_text.to_string());
                }
                // Too far away, likely belongs to different node
                return None;
            }
        } else if !sibling.kind().contains("whitespace") {
            // Stop if we hit a non-comment, non-whitespace node
            break;
        }

        prev_sibling = sibling.prev_sibling();
    }

    None
}

/// Parse `PHPDoc` comment text into structured tags
/// LIMITATION: Only single-line tags are supported in Phase 1.
/// Multi-line tags (e.g., `@param Type\n     *   $name`) are skipped gracefully.
pub fn parse_phpdoc_tags(phpdoc_comment: &str) -> PhpDocTags {
    let mut tags = PhpDocTags::default();

    // Remove /** and */ delimiters
    let comment_body = phpdoc_comment
        .trim_start_matches("/**")
        .trim_end_matches("*/")
        .trim();

    // Parse line by line (Phase 1: single-line tags only)
    for line in comment_body.lines() {
        let line = line.trim().trim_start_matches('*').trim();

        // @param Type $name - description
        if line.starts_with("@param") {
            if let Some(param) = parse_param_tag(line) {
                tags.params.push(param);
            }
        }
        // @return Type description
        else if line.starts_with("@return") {
            if let Some(type_str) = extract_type_from_tag(line) {
                tags.returns = Some(type_str);
            }
        }
        // @var Type
        else if line.starts_with("@var")
            && let Some(type_str) = extract_type_from_tag(line)
        {
            tags.var_type = Some(type_str);
        }
        // @property and other tags are Phase 2
    }

    tags
}

/// Parse "@param Type $name - description" tag
/// Handles optional params: $name, variadic: ...$name
fn parse_param_tag(line: &str) -> Option<ParamTag> {
    // Skip the @param keyword
    let after_keyword = line.trim_start_matches("@param").trim();

    // Extract type with brace-balancing (returns type content and end index)
    let (type_str, end_index) = extract_balanced_braces_with_index(after_keyword)?;

    // Find param name after the closing brace
    let after_type = &after_keyword[end_index + 1..].trim();

    // Extract parameter name (starts with $)
    let name = extract_param_name(after_type)?;

    // Extract description after name
    let description = after_type
        .split_once(&name)
        .and_then(|(_, rest)| rest.trim().strip_prefix('-'))
        .map(|s| s.trim().to_string());

    Some(ParamTag {
        name,
        type_str,
        description,
    })
}

/// Extract parameter name from text after type
/// Handles: $name, ...$name (variadic)
fn extract_param_name(text: &str) -> Option<String> {
    let text = text.trim();

    // Handle variadic param: ...$name
    let text_to_parse = if let Some(rest) = text.strip_prefix("...") {
        rest.trim()
    } else {
        text
    };

    // Find the parameter name (should start with $)
    if let Some(dollar_idx) = text_to_parse.find('$') {
        let param_part = &text_to_parse[dollar_idx..];
        // Extract until whitespace or hyphen (description start)
        let name = param_part
            .split(|c: char| c.is_whitespace() || c == '-')
            .next()?
            .trim();

        if !name.is_empty() {
            return Some(name.to_string());
        }
    }

    None
}

/// Extract type string from "@tag Type ..." format
/// Uses brace-balancing to handle complex types like {type1|type2}
fn extract_type_from_tag(line: &str) -> Option<String> {
    extract_balanced_braces(line)
}

/// Extract balanced brace content from "{...}" in a string
/// Returns (`type_content`, `closing_brace_index`) for correct parsing after nested types
fn extract_balanced_braces_with_index(s: &str) -> Option<(String, usize)> {
    let start = s.find('{')?;
    let mut depth = 0;
    let mut end = start;

    for (i, ch) in s[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }

    if depth != 0 {
        return None; // Unbalanced braces
    }

    // Return (content between braces, index of closing brace)
    Some((s[start + 1..end].to_string(), end))
}

/// Extract balanced brace content (backward-compatible wrapper)
fn extract_balanced_braces(s: &str) -> Option<String> {
    extract_balanced_braces_with_index(s).map(|(content, _)| content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_balanced_braces() {
        assert_eq!(
            extract_balanced_braces("@param {string} $name"),
            Some("string".to_string())
        );

        assert_eq!(
            extract_balanced_braces("@param {User|Admin} $user"),
            Some("User|Admin".to_string())
        );

        assert_eq!(
            extract_balanced_braces("@param {array<string>} $items"),
            Some("array<string>".to_string())
        );
    }

    #[test]
    fn test_extract_param_name() {
        assert_eq!(extract_param_name("$name"), Some("$name".to_string()));
        assert_eq!(
            extract_param_name("$count - the count"),
            Some("$count".to_string())
        );
        assert_eq!(extract_param_name("...$rest"), Some("$rest".to_string()));
        assert_eq!(
            extract_param_name("...$args - variadic args"),
            Some("$args".to_string())
        );
    }

    #[test]
    fn test_parse_param_tag() {
        let tag = parse_param_tag("@param {string} $name - description").unwrap();
        assert_eq!(tag.name, "$name");
        assert_eq!(tag.type_str, "string");
        assert_eq!(tag.description, Some("description".to_string()));

        let tag = parse_param_tag("@param {int} $count").unwrap();
        assert_eq!(tag.name, "$count");
        assert_eq!(tag.type_str, "int");
    }

    #[test]
    fn test_parse_phpdoc_tags() {
        let phpdoc = r#"/**
         * @param {string} $name
         * @param {int} $age
         * @return {User}
         */"#;

        let tags = parse_phpdoc_tags(phpdoc);
        assert_eq!(tags.params.len(), 2);
        assert_eq!(tags.params[0].name, "$name");
        assert_eq!(tags.params[1].name, "$age");
        assert_eq!(tags.returns, Some("User".to_string()));
    }

    #[test]
    fn test_parse_phpdoc_var_tag() {
        let phpdoc = r#"/**
         * @var {string} $username
         */"#;

        let tags = parse_phpdoc_tags(phpdoc);
        assert_eq!(tags.var_type, Some("string".to_string()));
    }

    #[test]
    fn test_parse_phpdoc_union_types() {
        let phpdoc = r#"/**
         * @param {string|int} $value
         * @return {bool}
         */"#;

        let tags = parse_phpdoc_tags(phpdoc);
        assert_eq!(tags.params[0].type_str, "string|int");
        assert_eq!(tags.returns, Some("bool".to_string()));
    }

    #[test]
    fn test_parse_phpdoc_array_types() {
        let phpdoc = r#"/**
         * @param {User[]} $users
         * @return {array<string, mixed>}
         */"#;

        let tags = parse_phpdoc_tags(phpdoc);
        assert_eq!(tags.params[0].type_str, "User[]");
        assert_eq!(tags.returns, Some("array<string, mixed>".to_string()));
    }
}
