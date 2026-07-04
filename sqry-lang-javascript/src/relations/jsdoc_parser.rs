//! `JSDoc` comment parsing for JavaScript type annotations
//!
//! Extracts type information from `JSDoc` comments (@param, @returns, @type)
//! to enable `TypeOf` and Reference edge creation.

use tree_sitter::Node;

/// Parsed `JSDoc` tags (Phase 1: @param, @returns, @type only)
#[derive(Debug, Default)]
pub struct JsDocTags {
    /// @param {Type} name - description
    pub params: Vec<ParamTag>,
    /// @returns {Type} description
    pub returns: Option<String>,
    /// @type {Type}
    pub type_annotation: Option<String>,
}

#[derive(Debug)]
pub struct ParamTag {
    pub name: String,
    pub type_str: String,
}

/// Extract `JSDoc` comment immediately preceding a node
/// Returns None if no `JSDoc` comment found
///
/// Handles both direct comments and comments on export wrappers:
/// - `/** ... */ function foo() {}` - direct
/// - `/** ... */ export function foo() {}` - comment on export node
pub fn extract_jsdoc_comment(node: Node, content: &[u8]) -> Option<String> {
    // Try to extract from current node first
    if let Some(comment) = try_extract_comment(node, content) {
        return Some(comment);
    }

    // If node is wrapped in export, check parent's preceding comment
    if let Some(parent) = node.parent()
        && parent.kind().contains("export")
    {
        return try_extract_comment(parent, content);
    }

    None
}

/// Helper to extract comment from a specific node
/// ADJACENCY RULE: `JSDoc` must be within 1 blank line of the target node
fn try_extract_comment(node: Node, content: &[u8]) -> Option<String> {
    let node_start_line = node.start_position().row;
    let mut prev_sibling = node.prev_sibling();

    while let Some(sibling) = prev_sibling {
        if sibling.kind() == "comment" {
            let comment_text = sibling.utf8_text(content).ok()?;

            // Check if it's a JSDoc comment (/** */)
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

/// Parse `JSDoc` comment text into structured tags
/// LIMITATION: Only single-line tags are supported in Phase 1.
/// Multi-line tags (e.g., `@param {Type}\n  *   name`) are skipped gracefully.
pub fn parse_jsdoc_tags(jsdoc_comment: &str) -> JsDocTags {
    let mut tags = JsDocTags::default();

    // Remove /** and */ delimiters
    let comment_body = jsdoc_comment
        .trim_start_matches("/**")
        .trim_end_matches("*/")
        .trim();

    // Parse line by line (Phase 1: single-line tags only)
    for line in comment_body.lines() {
        let line = line.trim().trim_start_matches('*').trim();

        // @param {Type} name - description
        if line.starts_with("@param") {
            if let Some(param) = parse_param_tag(line) {
                tags.params.push(param);
            }
        }
        // @returns {Type} description
        else if line.starts_with("@returns") || line.starts_with("@return") {
            if let Some(type_str) = extract_type_from_tag(line) {
                tags.returns = Some(type_str);
            }
        }
        // @type {Type}
        else if line.starts_with("@type")
            && let Some(type_str) = extract_type_from_tag(line)
        {
            tags.type_annotation = Some(type_str);
        }
        // @property and @template are Phase 2
    }

    tags
}

/// Parse "@param {Type} name - description" tag
/// Handles optional params: [name], rest params: ...name, dotted: options.name
/// Correctly handles nested braces in object types: {{id: string}}
fn parse_param_tag(line: &str) -> Option<ParamTag> {
    // Extract type with brace-balancing (returns type content and end index)
    let (type_str, end_index) = extract_balanced_braces_with_index(line)?;

    // Find param name after the closing brace
    // Use end_index+1 to skip past the matched closing brace
    let after_type = &line[end_index + 1..].trim();

    // Regex matches: optional [...], rest ..., JS identifiers (including $), dotted names
    // Supports: name, [optional], [...rest], [name=default], options.name, $ctx
    // NOTE: Using simple parsing instead of regex for better maintainability
    let name = extract_param_name(after_type)?;

    Some(ParamTag { name, type_str })
}

/// Extract parameter name from text after type
/// Handles: name, [optional], [...rest], [name=default], options.name, $ctx
fn extract_param_name(text: &str) -> Option<String> {
    let text = text.trim();

    // Handle [optional] or [name=default] syntax
    if let Some(stripped) = text.strip_prefix('[') {
        // Find closing bracket
        let close_idx = stripped.find(']')?;
        let inner = &stripped[..close_idx];

        // Handle rest param: [...name]
        let name_part = if let Some(rest) = inner.strip_prefix("...") {
            rest
        } else {
            inner
        };

        // Handle default value: [name=value]
        let name = if let Some((n, _)) = name_part.split_once('=') {
            n.trim()
        } else {
            name_part.trim()
        };

        return Some(name.to_string());
    }

    // Handle rest param without brackets: ...name
    if let Some(rest) = text.strip_prefix("...") {
        let name = rest.split_whitespace().next()?;
        return Some(name.to_string());
    }

    // Regular name (possibly dotted like options.name)
    // Stop at whitespace or hyphen (description start)
    let name = text
        .split(|c: char| c.is_whitespace() || c == '-')
        .next()?
        .trim();

    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Extract type string from "@tag {Type} ..." format
/// Uses brace-balancing to handle nested types like {{id: string, meta: {tags: string[]}}}
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
            extract_balanced_braces("@param {string} name"),
            Some("string".to_string())
        );

        assert_eq!(
            extract_balanced_braces("@param {{id: string}} obj"),
            Some("{id: string}".to_string())
        );

        assert_eq!(
            extract_balanced_braces("@param {{id: string, meta: {tags: string[]}}} obj"),
            Some("{id: string, meta: {tags: string[]}}".to_string())
        );
    }

    #[test]
    fn test_extract_param_name() {
        assert_eq!(extract_param_name("name"), Some("name".to_string()));
        assert_eq!(
            extract_param_name("[optional]"),
            Some("optional".to_string())
        );
        assert_eq!(extract_param_name("[count=10]"), Some("count".to_string()));
        assert_eq!(extract_param_name("...rest"), Some("rest".to_string()));
        assert_eq!(extract_param_name("[...args]"), Some("args".to_string()));
        assert_eq!(
            extract_param_name("options.name"),
            Some("options.name".to_string())
        );
        assert_eq!(extract_param_name("$ctx"), Some("$ctx".to_string()));
    }

    #[test]
    fn test_parse_param_tag() {
        let tag = parse_param_tag("@param {string} name - description").unwrap();
        assert_eq!(tag.name, "name");
        assert_eq!(tag.type_str, "string");

        let tag = parse_param_tag("@param {number} [count=10]").unwrap();
        assert_eq!(tag.name, "count");
        assert_eq!(tag.type_str, "number");
    }

    #[test]
    fn test_parse_jsdoc_tags() {
        let jsdoc = r"/**
         * @param {string} name
         * @param {number} age
         * @returns {User}
         */";

        let tags = parse_jsdoc_tags(jsdoc);
        assert_eq!(tags.params.len(), 2);
        assert_eq!(tags.params[0].name, "name");
        assert_eq!(tags.params[1].name, "age");
        assert_eq!(tags.returns, Some("User".to_string()));
    }
}
