//! JSONPath expression parser and matcher.
//!
//! Implements a subset of JSONPath for targeting nested fields in JSON documents.
//!
//! ## Supported Features
//!
//! - `$` — Root object
//! - `.field` — Child field access
//! - `[*]` — All array elements
//! - `[0]`, `[1,2,3]` — Specific array indices
//! - `..field` — Recursive descent (all matching fields at any depth)
//!
//! ## Unsupported Features
//!
//! - `[?(@.field == value)]` — Filter predicates
//! - `[start:end:step]` — Array slicing
//! - `[field1, field2]` — Union of fields
//! - `@` — Current node reference outside filters
//! - `*` — Wildcard on object keys

use crate::RedactionError;

/// A compiled `JSONPath` expression.
#[derive(Debug, Clone)]
pub struct CompiledJsonPath {
    /// The original expression string.
    #[allow(dead_code)]
    pub expression: String,
    /// Parsed segments.
    pub segments: Vec<PathSegment>,
}

/// A segment of a JSONPath expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSegment {
    /// Root element (`$`).
    Root,
    /// Child field access (`.field`).
    Child(String),
    /// Array wildcard (`[*]`).
    ArrayWildcard,
    /// Specific array indices (`[0]`, `[1,2,3]`).
    ArrayIndices(Vec<usize>),
    /// Recursive descent (`..field`).
    RecursiveDescend(String),
}

impl CompiledJsonPath {
    /// Parse a JSONPath expression.
    ///
    /// # Errors
    ///
    /// Returns `RedactionError::InvalidJsonPath` for unsupported or malformed expressions.
    pub fn parse(expression: &str) -> Result<Self, RedactionError> {
        let segments = parse_jsonpath(expression)?;
        Ok(Self {
            expression: expression.to_string(),
            segments,
        })
    }

    /// Check if a JSON path matches this expression.
    ///
    /// The `current_path` is a list of path components from root to current position.
    #[must_use]
    pub fn matches(&self, current_path: &[PathComponent]) -> bool {
        match_segments(&self.segments, current_path, 0, 0)
    }

    /// Get the expression string.
    #[must_use]
    #[allow(dead_code)]
    pub fn as_str(&self) -> &str {
        &self.expression
    }
}

/// A component in a JSON path during traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathComponent {
    /// Object field.
    Field(String),
    /// Array index.
    Index(usize),
}

impl PathComponent {
    /// Get the field name if this is a field component.
    #[must_use]
    #[allow(dead_code)]
    pub fn as_field(&self) -> Option<&str> {
        match self {
            Self::Field(name) => Some(name),
            Self::Index(_) => None,
        }
    }

    /// Get the index if this is an index component.
    #[must_use]
    #[allow(dead_code)]
    pub fn as_index(&self) -> Option<usize> {
        match self {
            Self::Index(i) => Some(*i),
            Self::Field(_) => None,
        }
    }
}

/// Parse a JSONPath expression into segments.
fn parse_jsonpath(expression: &str) -> Result<Vec<PathSegment>, RedactionError> {
    let mut segments = Vec::new();
    let mut chars = expression.chars().peekable();

    parse_root(expression, &mut chars, &mut segments)?;

    while chars.peek().is_some() {
        let segment = match chars.peek() {
            Some('.') => parse_dot_segment(&mut chars)?,
            Some('[') => parse_bracket_segment(&mut chars)?,
            Some(c) => {
                return Err(RedactionError::InvalidJsonPath(format!(
                    "Unexpected character '{}' in JSONPath",
                    c
                )));
            }
            None => break,
        };
        segments.push(segment);
    }

    Ok(segments)
}

fn parse_root(
    expression: &str,
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    segments: &mut Vec<PathSegment>,
) -> Result<(), RedactionError> {
    match chars.next() {
        Some('$') => {
            segments.push(PathSegment::Root);
            Ok(())
        }
        _ => Err(RedactionError::InvalidJsonPath(format!(
            "JSONPath must start with '$': {}",
            expression
        ))),
    }
}

fn parse_dot_segment(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Result<PathSegment, RedactionError> {
    chars.next();
    if chars.peek() == Some(&'.') {
        chars.next();
        parse_recursive_descend_segment(chars)
    } else {
        parse_child_segment(chars)
    }
}

fn parse_recursive_descend_segment(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Result<PathSegment, RedactionError> {
    let field = parse_field_name(chars)?;
    if field.is_empty() {
        return Err(RedactionError::InvalidJsonPath(
            "Recursive descent requires field name after '..'".to_string(),
        ));
    }
    Ok(PathSegment::RecursiveDescend(field))
}

fn parse_child_segment(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Result<PathSegment, RedactionError> {
    let field = parse_field_name(chars)?;
    if field.is_empty() {
        return Err(RedactionError::InvalidJsonPath(
            "Expected field name after '.'".to_string(),
        ));
    }
    Ok(PathSegment::Child(field))
}

fn parse_bracket_segment(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Result<PathSegment, RedactionError> {
    chars.next();
    parse_bracket_content(chars)
}

/// Parse a field name (alphanumeric, underscore, starting with letter or underscore).
fn parse_field_name(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Result<String, RedactionError> {
    let mut name = String::new();

    while let Some(&c) = chars.peek() {
        if c.is_alphanumeric() || c == '_' {
            name.push(c);
            chars.next();
        } else {
            break;
        }
    }

    Ok(name)
}

/// Parse bracket content ([*], [0], [0,1,2]).
fn parse_bracket_content(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Result<PathSegment, RedactionError> {
    let mut content = String::new();

    // Read until closing bracket
    loop {
        match chars.next() {
            Some(']') => break,
            Some(c) => content.push(c),
            None => {
                return Err(RedactionError::InvalidJsonPath(
                    "Unclosed bracket in JSONPath".to_string(),
                ));
            }
        }
    }

    let content = content.trim();

    // Check for wildcard
    if content == "*" {
        return Ok(PathSegment::ArrayWildcard);
    }

    // Check for filter predicates (unsupported)
    if content.starts_with('?') {
        return Err(RedactionError::InvalidJsonPath(
            "Filter predicates [?(...)] are not supported".to_string(),
        ));
    }

    // Check for slice notation (unsupported)
    if content.contains(':') {
        return Err(RedactionError::InvalidJsonPath(
            "Array slicing [start:end:step] is not supported".to_string(),
        ));
    }

    // Parse as comma-separated indices
    let indices: Result<Vec<usize>, _> = content
        .split(',')
        .map(|s| {
            s.trim()
                .parse::<usize>()
                .map_err(|_| RedactionError::InvalidJsonPath(format!("Invalid array index: {}", s)))
        })
        .collect();

    Ok(PathSegment::ArrayIndices(indices?))
}

/// Match segments against a path.
fn match_segments(
    segments: &[PathSegment],
    path: &[PathComponent],
    seg_idx: usize,
    path_idx: usize,
) -> bool {
    // If we've matched all segments, success
    if seg_idx >= segments.len() {
        return true;
    }

    // If we've run out of path but have more segments, fail
    // (except for recursive descent which can match at any depth)
    if path_idx >= path.len() {
        return false;
    }

    match &segments[seg_idx] {
        PathSegment::Root => {
            // Root always matches at the start
            match_segments(segments, path, seg_idx + 1, path_idx)
        }
        PathSegment::Child(name) => match_child_segment(name, segments, path, seg_idx, path_idx),
        PathSegment::ArrayWildcard => {
            match_array_wildcard_segment(segments, path, seg_idx, path_idx)
        }
        PathSegment::ArrayIndices(indices) => {
            match_array_indices_segment(indices, segments, path, seg_idx, path_idx)
        }
        PathSegment::RecursiveDescend(name) => {
            match_recursive_descend_segment(name, segments, path, seg_idx, path_idx)
        }
    }
}

fn match_child_segment(
    name: &str,
    segments: &[PathSegment],
    path: &[PathComponent],
    seg_idx: usize,
    path_idx: usize,
) -> bool {
    if let PathComponent::Field(field) = &path[path_idx] {
        if field == name {
            return match_segments(segments, path, seg_idx + 1, path_idx + 1);
        }
    }
    false
}

fn match_array_wildcard_segment(
    segments: &[PathSegment],
    path: &[PathComponent],
    seg_idx: usize,
    path_idx: usize,
) -> bool {
    if matches!(path[path_idx], PathComponent::Index(_)) {
        return match_segments(segments, path, seg_idx + 1, path_idx + 1);
    }
    false
}

fn match_array_indices_segment(
    indices: &[usize],
    segments: &[PathSegment],
    path: &[PathComponent],
    seg_idx: usize,
    path_idx: usize,
) -> bool {
    if let PathComponent::Index(i) = &path[path_idx] {
        if indices.contains(i) {
            return match_segments(segments, path, seg_idx + 1, path_idx + 1);
        }
    }
    false
}

fn match_recursive_descend_segment(
    name: &str,
    segments: &[PathSegment],
    path: &[PathComponent],
    seg_idx: usize,
    path_idx: usize,
) -> bool {
    for idx in path_idx..path.len() {
        if let PathComponent::Field(field) = &path[idx] {
            if field == name && match_segments(segments, path, seg_idx + 1, idx + 1) {
                return true;
            }
        }
    }
    false
}

/// Convert a path to its JSONPath string representation.
#[must_use]
pub fn path_to_string(path: &[PathComponent]) -> String {
    let mut result = String::from("$");
    for component in path {
        match component {
            PathComponent::Field(name) => {
                result.push('.');
                result.push_str(name);
            }
            PathComponent::Index(i) => {
                result.push('[');
                result.push_str(&i.to_string());
                result.push(']');
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_path() {
        let path = CompiledJsonPath::parse("$.results").unwrap();
        assert_eq!(path.segments.len(), 2);
        assert_eq!(path.segments[0], PathSegment::Root);
        assert_eq!(path.segments[1], PathSegment::Child("results".to_string()));
    }

    #[test]
    fn test_parse_nested_path() {
        let path = CompiledJsonPath::parse("$.results[*].fileUri").unwrap();
        assert_eq!(path.segments.len(), 4);
        assert_eq!(path.segments[0], PathSegment::Root);
        assert_eq!(path.segments[1], PathSegment::Child("results".to_string()));
        assert_eq!(path.segments[2], PathSegment::ArrayWildcard);
        assert_eq!(path.segments[3], PathSegment::Child("fileUri".to_string()));
    }

    #[test]
    fn test_parse_specific_indices() {
        let path = CompiledJsonPath::parse("$.items[0,2,5]").unwrap();
        assert_eq!(path.segments.len(), 3);
        assert_eq!(path.segments[2], PathSegment::ArrayIndices(vec![0, 2, 5]));
    }

    #[test]
    fn test_parse_recursive_descent() {
        let path = CompiledJsonPath::parse("$..uri").unwrap();
        assert_eq!(path.segments.len(), 2);
        assert_eq!(
            path.segments[1],
            PathSegment::RecursiveDescend("uri".to_string())
        );
    }

    #[test]
    fn test_parse_must_start_with_root() {
        let result = CompiledJsonPath::parse(".results");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_unsupported_filter() {
        let result = CompiledJsonPath::parse("$.results[?(@.kind=='file')]");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Filter predicates")
        );
    }

    #[test]
    fn test_parse_unsupported_slice() {
        let result = CompiledJsonPath::parse("$.results[0:5]");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Array slicing"));
    }

    #[test]
    fn test_match_simple() {
        let expr = CompiledJsonPath::parse("$.results").unwrap();
        let path = vec![PathComponent::Field("results".to_string())];
        assert!(expr.matches(&path));
    }

    #[test]
    fn test_match_nested() {
        let expr = CompiledJsonPath::parse("$.results[*].fileUri").unwrap();
        let path = vec![
            PathComponent::Field("results".to_string()),
            PathComponent::Index(0),
            PathComponent::Field("fileUri".to_string()),
        ];
        assert!(expr.matches(&path));
    }

    #[test]
    fn test_match_specific_index() {
        let expr = CompiledJsonPath::parse("$.items[1]").unwrap();

        let path0 = vec![
            PathComponent::Field("items".to_string()),
            PathComponent::Index(0),
        ];
        assert!(!expr.matches(&path0));

        let path1 = vec![
            PathComponent::Field("items".to_string()),
            PathComponent::Index(1),
        ];
        assert!(expr.matches(&path1));
    }

    #[test]
    fn test_match_recursive_descent() {
        let expr = CompiledJsonPath::parse("$..uri").unwrap();

        // Should match at any depth
        let shallow = vec![PathComponent::Field("uri".to_string())];
        assert!(expr.matches(&shallow));

        let deep = vec![
            PathComponent::Field("results".to_string()),
            PathComponent::Index(0),
            PathComponent::Field("edges".to_string()),
            PathComponent::Index(0),
            PathComponent::Field("uri".to_string()),
        ];
        assert!(expr.matches(&deep));
    }

    #[test]
    fn test_path_to_string() {
        let path = vec![
            PathComponent::Field("results".to_string()),
            PathComponent::Index(0),
            PathComponent::Field("fileUri".to_string()),
        ];
        assert_eq!(path_to_string(&path), "$.results[0].fileUri");
    }

    #[test]
    fn test_no_match_wrong_field() {
        let expr = CompiledJsonPath::parse("$.results").unwrap();
        let path = vec![PathComponent::Field("items".to_string())];
        assert!(!expr.matches(&path));
    }

    #[test]
    fn test_parse_deep_nested() {
        let path = CompiledJsonPath::parse("$.results[*].edges[*].from.fileUri").unwrap();
        assert_eq!(path.segments.len(), 7);
    }
}
