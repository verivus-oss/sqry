//! Input preprocessing for natural language queries.
//!
//! Handles:
//! - Unicode normalization (NFKC)
//! - Zero-width character stripping
//! - Homoglyph detection and normalization
//! - Quoted span extraction

mod confusables;
mod homoglyph;
mod unicode;

use crate::error::{NlResult, PreprocessError};
use crate::types::PreprocessResult;

/// Maximum input length in bytes (4KB per spec)
pub const MAX_INPUT_LENGTH: usize = 4096;

/// Preprocess natural language input for translation.
///
/// # Steps
/// 1. Validate length (≤4KB)
/// 2. Extract quoted spans FIRST (preserve exact quoted content)
/// 3. NFKC Unicode normalization on unquoted portions
/// 4. Strip zero-width characters
/// 5. Homoglyph normalization (Cyrillic→Latin) - but NOT inside quotes
///
/// # Errors
///
/// Returns [`PreprocessError`] if:
/// - Input exceeds 4KB
/// - Input is empty or whitespace-only
pub fn preprocess_input(input: &str) -> NlResult<PreprocessResult> {
    // 1. Validate length
    if input.len() > MAX_INPUT_LENGTH {
        return Err(PreprocessError::InputTooLong {
            len: input.len(),
            max: MAX_INPUT_LENGTH,
        }
        .into());
    }

    // 2. Extract quoted spans FIRST to preserve exact content
    let (text_without_quotes, quoted_spans, quote_positions) =
        extract_quoted_spans_with_positions(input);

    // 3. NFKC normalization (on the text portions)
    let normalized = unicode::normalize_nfkc(&text_without_quotes);

    // 4. Strip zero-width characters
    let stripped = unicode::strip_zero_width(&normalized);

    // 5. Homoglyph normalization (only on non-quoted text)
    let (dehomoglyphed, homoglyphs_replaced) = homoglyph::replace_confusables(&stripped);

    // 6. Check for empty input after normalization
    let trimmed = dehomoglyphed.trim();
    if trimmed.is_empty() && quoted_spans.is_empty() {
        return Err(PreprocessError::EmptyInput.into());
    }

    // 7. Reconstruct text with original quoted content
    let final_text = reconstruct_with_quotes(trimmed, &quoted_spans, &quote_positions);

    Ok(PreprocessResult {
        text: final_text,
        quoted_spans,
        normalized: normalized != text_without_quotes,
        homoglyphs_replaced,
    })
}

/// Extract quoted strings from input with position tracking.
///
/// Returns:
/// - Text with quotes replaced by placeholder markers
/// - Vector of quoted content
/// - Vector of placeholder positions (char offsets in the result)
fn extract_quoted_spans_with_positions(input: &str) -> (String, Vec<String>, Vec<usize>) {
    let mut result = String::with_capacity(input.len());
    let mut quoted_spans = Vec::new();
    let mut positions = Vec::new();
    let chars = input.chars().peekable();
    let mut in_quote = false;
    let mut quote_char = '"';
    let mut current_span = String::new();

    for c in chars {
        if !in_quote && (c == '"' || c == '\'') {
            in_quote = true;
            quote_char = c;
            current_span.clear();
        } else if in_quote && c == quote_char {
            in_quote = false;
            if !current_span.is_empty() {
                // Record position where this quote will be reinserted
                positions.push(result.chars().count());
                quoted_spans.push(current_span.clone());
                // Add a placeholder marker
                result.push('\x00'); // NUL as placeholder
            }
        } else if in_quote {
            current_span.push(c);
        } else {
            result.push(c);
        }
    }

    // Handle unclosed quote
    if in_quote && !current_span.is_empty() {
        positions.push(result.chars().count());
        quoted_spans.push(current_span.clone());
        result.push('\x00');
    }

    (result, quoted_spans, positions)
}

/// Reconstruct text by replacing placeholder markers with quoted content.
fn reconstruct_with_quotes(text: &str, quotes: &[String], _positions: &[usize]) -> String {
    if quotes.is_empty() {
        return text.to_string();
    }

    let mut result =
        String::with_capacity(text.len() + quotes.iter().map(|s| s.len() + 2).sum::<usize>());
    let mut quote_iter = quotes.iter();

    for c in text.chars() {
        if c == '\x00' {
            // Replace placeholder with original quoted content
            if let Some(quoted) = quote_iter.next() {
                result.push('"');
                result.push_str(quoted);
                result.push('"');
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Extract quoted strings from input, preserving them for entity extraction.
///
/// Supports both single and double quotes. Nested quotes are not supported.
#[allow(dead_code)]
fn extract_quoted_spans(input: &str) -> (String, Vec<String>) {
    let mut result = String::with_capacity(input.len());
    let mut quoted_spans = Vec::new();
    let chars = input.chars().peekable();
    let mut in_quote = false;
    let mut quote_char = '"';
    let mut current_span = String::new();

    for c in chars {
        if !in_quote && (c == '"' || c == '\'') {
            in_quote = true;
            quote_char = c;
            current_span.clear();
        } else if in_quote && c == quote_char {
            in_quote = false;
            if !current_span.is_empty() {
                quoted_spans.push(current_span.clone());
                // Keep a placeholder in the text
                result.push('"');
                result.push_str(&current_span);
                result.push('"');
            }
        } else if in_quote {
            current_span.push(c);
        } else {
            result.push(c);
        }
    }

    // Handle unclosed quote
    if in_quote && !current_span.is_empty() {
        quoted_spans.push(current_span.clone());
        result.push_str(&current_span);
    }

    (result, quoted_spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_preprocess_basic() {
        let result = preprocess_input("find authentication").unwrap();
        assert_eq!(result.text, "find authentication");
        assert!(result.quoted_spans.is_empty());
    }

    #[test]
    fn test_preprocess_with_quotes() {
        let result = preprocess_input("find \"UserAuth::login\"").unwrap();
        assert!(result.quoted_spans.contains(&"UserAuth::login".to_string()));
        assert!(result.text.contains("\"UserAuth::login\""));
    }

    #[test]
    fn test_preprocess_too_long() {
        let long_input = "x".repeat(MAX_INPUT_LENGTH + 1);
        let result = preprocess_input(&long_input);
        assert!(matches!(
            result,
            Err(crate::error::NlError::Preprocess(
                PreprocessError::InputTooLong { .. }
            ))
        ));
    }

    #[test]
    fn test_preprocess_empty() {
        let result = preprocess_input("   ");
        assert!(matches!(
            result,
            Err(crate::error::NlError::Preprocess(
                PreprocessError::EmptyInput
            ))
        ));
    }

    #[test]
    fn test_extract_quoted_spans() {
        let (text, spans) = extract_quoted_spans("find \"foo\" and \"bar\"");
        assert_eq!(spans, vec!["foo", "bar"]);
        assert!(text.contains("\"foo\""));
        assert!(text.contains("\"bar\""));
    }
}
