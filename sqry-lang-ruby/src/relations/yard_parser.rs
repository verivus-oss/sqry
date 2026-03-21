//! YARD comment parsing for Ruby type annotations
//!
//! Extracts type information from YARD comments (@param, @return, @type)
//! to enable `TypeOf` and Reference edge creation.

use tree_sitter::Node;

/// Parsed YARD tags (Phase 1: @param, @return, @type only)
#[derive(Debug, Default)]
pub struct YardTags {
    /// @param [Type] name - description
    pub params: Vec<ParamTag>,
    /// @return [Type] description
    pub returns: Option<String>,
    /// @type [Type]
    pub type_annotation: Option<String>,
}

#[derive(Debug)]
pub struct ParamTag {
    pub name: String,
    pub type_str: String,
    #[allow(dead_code)] // Reserved for future use
    pub description: Option<String>,
}

/// Extract YARD comment immediately preceding a node
/// Returns None if no YARD comment found
///
/// Handles both direct comments and comments on export wrappers.
pub fn extract_yard_comment(node: Node, content: &[u8]) -> Option<String> {
    // Try to extract from current node first
    if let Some(comment) = try_extract_comment(node, content) {
        return Some(comment);
    }

    // If node is wrapped in modifiers (public, protected, private, module_function),
    // check parent's preceding comment
    if let Some(parent) = node.parent() {
        let parent_kind = parent.kind();
        if parent_kind == "visibility_modifier"
            || parent_kind == "body_statement"
            || parent_kind.contains("_statement")
        {
            return try_extract_comment(parent, content);
        }
    }

    None
}

/// Helper to extract comment from a specific node
/// ADJACENCY RULE: YARD must be within 1 blank line of the target node
fn try_extract_comment(node: Node, content: &[u8]) -> Option<String> {
    let node_start_line = node.start_position().row;
    let mut prev_sibling = node.prev_sibling();

    // Collect adjacent comment lines
    let mut comment_lines = Vec::new();
    let mut expected_line = node_start_line;

    while let Some(sibling) = prev_sibling {
        if sibling.kind() == "comment" {
            let comment_text = sibling.utf8_text(content).ok()?;
            let comment_end_line = sibling.end_position().row;

            // Check line distance (allow max 1 blank line between comment and node)
            let line_distance = expected_line.saturating_sub(comment_end_line);

            if line_distance <= 2 {
                // <= 2 means adjacent or 1 blank line in between
                // Check if it's a YARD comment (starts with #)
                if comment_text.trim().starts_with('#') {
                    comment_lines.push(comment_text.to_string());
                    expected_line = sibling.start_position().row;
                    prev_sibling = sibling.prev_sibling();
                    continue;
                }
            }
            // Too far away or not a YARD comment, stop
            break;
        } else if !sibling.kind().contains("whitespace") {
            // Stop if we hit a non-comment, non-whitespace node
            break;
        }

        prev_sibling = sibling.prev_sibling();
    }

    if comment_lines.is_empty() {
        return None;
    }

    // Reverse to get comments in order (we collected backwards)
    comment_lines.reverse();

    // Join comments into single block
    Some(comment_lines.join("\n"))
}

/// Parse YARD comment text into structured tags
/// Handles single-line and multi-line YARD comment blocks.
pub fn parse_yard_tags(yard_comment: &str) -> YardTags {
    let mut tags = YardTags::default();

    // Parse line by line
    for line in yard_comment.lines() {
        let line = line.trim().trim_start_matches('#').trim();

        // @param [Type] name - description
        if line.starts_with("@param") {
            if let Some(param) = parse_param_tag(line) {
                tags.params.push(param);
            }
        }
        // @return [Type] description
        else if line.starts_with("@return") {
            if let Some(type_str) = extract_type_from_tag(line) {
                tags.returns = Some(type_str);
            }
        }
        // @type [Type]
        else if line.starts_with("@type")
            && let Some(type_str) = extract_type_from_tag(line)
        {
            tags.type_annotation = Some(type_str);
        }
        // @property and other tags are Phase 2
    }

    tags
}

/// Parse "@param [Type] name - description" tag
/// Handles optional params, keyword params, splat params, and block params
fn parse_param_tag(line: &str) -> Option<ParamTag> {
    // Skip the @param keyword
    let after_keyword = line.trim_start_matches("@param").trim();

    // Extract type with bracket-balancing (returns type content and end index)
    let (type_str, end_index) = extract_balanced_brackets_with_index(after_keyword)?;

    // Find param name after the closing bracket
    let after_type = &after_keyword[end_index + 1..].trim();

    // Extract parameter name
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
/// Handles: name, *splat, **kwargs, &block, name:, name: value
fn extract_param_name(text: &str) -> Option<String> {
    let text = text.trim();

    // Handle splat param: *args
    let text_to_parse = if let Some(rest) = text.strip_prefix('*') {
        rest.trim()
    } else {
        text
    };

    // Handle keyword splat: **kwargs
    let text_to_parse = if let Some(rest) = text_to_parse.strip_prefix('*') {
        rest.trim()
    } else {
        text_to_parse
    };

    // Handle block param: &block
    let text_to_parse = if let Some(rest) = text_to_parse.strip_prefix('&') {
        rest.trim()
    } else {
        text_to_parse
    };

    // Extract name (until whitespace, hyphen, or colon)
    let name = text_to_parse
        .split(|c: char| c.is_whitespace() || c == '-' || c == ':')
        .next()?
        .trim();

    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Extract type string from "@tag [Type] ..." format
/// Uses bracket-balancing to handle complex types like [Array<String>]
fn extract_type_from_tag(line: &str) -> Option<String> {
    extract_balanced_brackets(line)
}

/// Extract balanced bracket content from "[...]" in a string
/// Returns (`type_content`, `closing_bracket_index`) for correct parsing after nested types
fn extract_balanced_brackets_with_index(s: &str) -> Option<(String, usize)> {
    let start = s.find('[')?;
    let mut depth = 0;
    let mut end = start;

    for (i, ch) in s[start..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
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
        return None; // Unbalanced brackets
    }

    // Return (content between brackets, index of closing bracket)
    Some((s[start + 1..end].to_string(), end))
}

/// Extract balanced bracket content (backward-compatible wrapper)
fn extract_balanced_brackets(s: &str) -> Option<String> {
    extract_balanced_brackets_with_index(s).map(|(content, _)| content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_balanced_brackets() {
        assert_eq!(
            extract_balanced_brackets("@param [String] name"),
            Some("String".to_string())
        );

        assert_eq!(
            extract_balanced_brackets("@param [User, Admin] user"),
            Some("User, Admin".to_string())
        );

        assert_eq!(
            extract_balanced_brackets("@param [Array<String>] items"),
            Some("Array<String>".to_string())
        );

        assert_eq!(
            extract_balanced_brackets("@return [Hash{String => Integer}]"),
            Some("Hash{String => Integer}".to_string())
        );
    }

    #[test]
    fn test_extract_param_name() {
        assert_eq!(extract_param_name("name"), Some("name".to_string()));
        assert_eq!(
            extract_param_name("count - the count"),
            Some("count".to_string())
        );
        assert_eq!(extract_param_name("*args"), Some("args".to_string()));
        assert_eq!(
            extract_param_name("**kwargs - keyword args"),
            Some("kwargs".to_string())
        );
        assert_eq!(extract_param_name("&block"), Some("block".to_string()));
        assert_eq!(extract_param_name("name:"), Some("name".to_string()));
        assert_eq!(
            extract_param_name("value: 10 - default value"),
            Some("value".to_string())
        );
    }

    #[test]
    fn test_parse_param_tag() {
        let tag = parse_param_tag("@param [String] name - description").unwrap();
        assert_eq!(tag.name, "name");
        assert_eq!(tag.type_str, "String");
        assert_eq!(tag.description, Some("description".to_string()));

        let tag = parse_param_tag("@param [Integer] count").unwrap();
        assert_eq!(tag.name, "count");
        assert_eq!(tag.type_str, "Integer");
    }

    #[test]
    fn test_parse_yard_tags() {
        let yard = r#"# @param [String] name
# @param [Integer] age
# @return [User]"#;

        let tags = parse_yard_tags(yard);
        assert_eq!(tags.params.len(), 2);
        assert_eq!(tags.params[0].name, "name");
        assert_eq!(tags.params[1].name, "age");
        assert_eq!(tags.returns, Some("User".to_string()));
    }

    #[test]
    fn test_parse_yard_type_tag() {
        let yard = r#"# @type [String]"#;

        let tags = parse_yard_tags(yard);
        assert_eq!(tags.type_annotation, Some("String".to_string()));
    }

    #[test]
    fn test_parse_yard_union_types() {
        let yard = r#"# @param [String, Integer] value
# @return [Boolean]"#;

        let tags = parse_yard_tags(yard);
        assert_eq!(tags.params[0].type_str, "String, Integer");
        assert_eq!(tags.returns, Some("Boolean".to_string()));
    }

    #[test]
    fn test_parse_yard_array_types() {
        let yard = r#"# @param [Array<User>] users
# @return [Hash{String => Integer}]"#;

        let tags = parse_yard_tags(yard);
        assert_eq!(tags.params[0].type_str, "Array<User>");
        assert_eq!(tags.returns, Some("Hash{String => Integer}".to_string()));
    }

    #[test]
    fn test_parse_yard_nullable_types() {
        let yard = r#"# @param [String, nil] value
# @return [User, nil]"#;

        let tags = parse_yard_tags(yard);
        assert_eq!(tags.params[0].type_str, "String, nil");
        assert_eq!(tags.returns, Some("User, nil".to_string()));
    }
}
