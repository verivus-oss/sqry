//! On-demand source JAR documentation extractor.
//!
//! Extracts Javadoc/KDoc/Scaladoc comments from source JARs when requested
//! for hover information. Results are cached in an LRU cache to avoid
//! re-reading JARs.
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_classpath::resolve::source_jars::SourceJarProvider;
//! use sqry_classpath::resolve::ClasspathEntry;
//!
//! let provider = SourceJarProvider::new(&entries, 1000);
//! let docs = provider.get_docs("com.google.common.collect.ImmutableList", jar_path);
//! ```
//!
//! # Source file location
//!
//! Given FQN `com.example.MyClass`, the source file is located at
//! `com/example/MyClass.java` within the source JAR. For Kotlin and Scala,
//! `.kt` and `.scala` extensions are tried as fallbacks.
//!
//! # Doc comment extraction
//!
//! Extracts `/** ... */` Javadoc-style blocks and converts to plain text by:
//! - Stripping leading `*` from each line
//! - Converting HTML tags (`<p>`, `<code>`, `<pre>`) to plain text equivalents
//! - Converting inline tags (`{@code ...}`, `{@link ...}`) to readable form
//! - Formatting block tags (`@param`, `@return`, `@throws`) for display

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use log::warn;

use super::ClasspathEntry;

/// Default LRU cache capacity for extracted documentation strings.
const DEFAULT_CACHE_SIZE: usize = 1000;

/// File extensions to search in source JARs, in priority order.
const SOURCE_EXTENSIONS: &[&str] = &["java", "kt", "scala"];

/// On-demand source JAR documentation extractor.
///
/// Extracts Javadoc/KDoc/Scaladoc comments from source JARs when requested.
/// Results are cached in an LRU cache to avoid re-reading JARs.
pub struct SourceJarProvider {
    /// Map from binary JAR path to source JAR path.
    source_jar_map: HashMap<PathBuf, PathBuf>,
    /// LRU cache: cache key -> extracted documentation string.
    /// Cache key format: `"{jar_path}::{fqn}"` or `"{jar_path}::{fqn}::{member}"`.
    /// `None` values are cached to avoid re-reading JARs for missing docs.
    cache: Mutex<lru::LruCache<String, Option<String>>>,
}

impl SourceJarProvider {
    /// Create a new provider from classpath entries.
    ///
    /// Scans entries for `source_jar` paths and builds the mapping from
    /// binary JAR path to source JAR path.
    #[must_use]
    #[allow(clippy::missing_errors_doc)] // Internal helper function
    #[allow(clippy::missing_panics_doc)] // Panic condition documented in body
    pub fn new(entries: &[ClasspathEntry], cache_size: usize) -> Self {
        let size = if cache_size == 0 {
            DEFAULT_CACHE_SIZE
        } else {
            cache_size
        };
        let mut source_jar_map = HashMap::new();
        for entry in entries {
            if let Some(ref source_jar) = entry.source_jar {
                source_jar_map.insert(entry.jar_path.clone(), source_jar.clone());
            }
        }
        Self {
            source_jar_map,
            cache: Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(size).expect("cache size must be non-zero"),
            )),
        }
    }

    /// Create a new provider with the default cache size.
    #[must_use]
    pub fn with_defaults(entries: &[ClasspathEntry]) -> Self {
        Self::new(entries, DEFAULT_CACHE_SIZE)
    }

    /// Extract documentation for a class by fully qualified name.
    ///
    /// Returns the doc comment as plain text, or `None` if:
    /// - No source JAR is available for the given binary JAR
    /// - The class source file is not found in the source JAR
    /// - The source file has no doc comment before the class declaration
    #[allow(clippy::missing_panics_doc)] // Panic condition documented in body
    pub fn get_docs(&self, fqn: &str, jar_path: &Path) -> Option<String> {
        let cache_key = format!("{}::{fqn}", jar_path.display());

        // Check cache first.
        {
            let mut cache = self.cache.lock().expect("cache lock poisoned");
            if let Some(cached) = cache.get(&cache_key) {
                return cached.clone();
            }
        }

        let result = self.extract_class_docs(fqn, jar_path);

        // Cache the result (including None for missing docs).
        {
            let mut cache = self.cache.lock().expect("cache lock poisoned");
            cache.put(cache_key, result.clone());
        }

        result
    }

    /// Extract documentation for a specific member (method/field) within a class.
    ///
    /// Searches the source file for a doc comment immediately preceding a
    /// declaration that matches `member_name`.
    #[allow(clippy::missing_panics_doc)] // Panic documented in body
    pub fn get_member_docs(&self, fqn: &str, member_name: &str, jar_path: &Path) -> Option<String> {
        let cache_key = format!("{}::{fqn}::{member_name}", jar_path.display());

        // Check cache first.
        {
            let mut cache = self.cache.lock().expect("cache lock poisoned");
            if let Some(cached) = cache.get(&cache_key) {
                return cached.clone();
            }
        }

        let result = self.extract_member_docs(fqn, member_name, jar_path);

        // Cache the result (including None for missing docs).
        {
            let mut cache = self.cache.lock().expect("cache lock poisoned");
            cache.put(cache_key, result.clone());
        }

        result
    }

    /// Read the source file content for a given FQN from the source JAR.
    ///
    /// Tries `.java`, `.kt`, and `.scala` extensions in order.
    fn read_source_from_jar(&self, fqn: &str, jar_path: &Path) -> Option<String> {
        let source_jar_path = self.source_jar_map.get(jar_path)?;

        let file = match std::fs::File::open(source_jar_path) {
            Ok(f) => f,
            Err(e) => {
                warn!(
                    "Failed to open source JAR {}: {e}",
                    source_jar_path.display()
                );
                return None;
            }
        };

        let mut archive = match zip::ZipArchive::new(file) {
            Ok(a) => a,
            Err(e) => {
                warn!(
                    "Failed to read source JAR {} as ZIP: {e}",
                    source_jar_path.display()
                );
                return None;
            }
        };

        // Convert FQN to path: com.example.MyClass -> com/example/MyClass
        let base_path = fqn.replace('.', "/");

        for ext in SOURCE_EXTENSIONS {
            let entry_path = format!("{base_path}.{ext}");
            if let Ok(mut entry) = archive.by_name(&entry_path) {
                let mut content = String::new();
                if let Err(e) = entry.read_to_string(&mut content) {
                    warn!("Failed to read {entry_path} from source JAR: {e}");
                    return None;
                }
                return Some(content);
            }
        }

        None
    }

    /// Extract the class-level doc comment from a source file.
    fn extract_class_docs(&self, fqn: &str, jar_path: &Path) -> Option<String> {
        let source = self.read_source_from_jar(fqn, jar_path)?;
        let class_name = fqn.rsplit('.').next().unwrap_or(fqn);
        extract_class_doc_comment(&source, class_name)
    }

    /// Extract a member-level doc comment from a source file.
    fn extract_member_docs(&self, fqn: &str, member_name: &str, jar_path: &Path) -> Option<String> {
        let source = self.read_source_from_jar(fqn, jar_path)?;
        extract_member_doc_comment(&source, member_name)
    }
}

/// Extract the doc comment for a class declaration from source text.
///
/// Finds the last `/** ... */` block that appears before the class/interface/object
/// declaration matching `class_name`.
fn extract_class_doc_comment(source: &str, class_name: &str) -> Option<String> {
    // Find the class declaration line.
    let class_patterns = [
        format!("class {class_name}"),
        format!("interface {class_name}"),
        format!("enum {class_name}"),
        format!("object {class_name}"),
        format!("trait {class_name}"),
        format!("record {class_name}"),
    ];

    let class_pos = class_patterns
        .iter()
        .filter_map(|pat| find_declaration_position(source, pat))
        .min()?;

    // Find the doc comment preceding this position.
    extract_preceding_doc_comment(source, class_pos)
}

/// Extract the doc comment for a member (method/field) from source text.
///
/// Finds the `/** ... */` block immediately preceding the first occurrence
/// of `member_name` in a declaration context (i.e., followed by `(` for methods
/// or preceded by a type for fields).
fn extract_member_doc_comment(source: &str, member_name: &str) -> Option<String> {
    // Search for the member name in a declaration context.
    // We look for patterns like: `memberName(`, `memberName =`, `memberName;`,
    // or just the member name preceded by whitespace and a type.
    let mut search_start = 0;
    while search_start < source.len() {
        let remaining = &source[search_start..];
        let offset = remaining.find(member_name)?;
        let abs_pos = search_start + offset;

        // Verify this is a declaration context (not a reference/call inside a method body).
        // The member name must be preceded by whitespace or declaration keywords.
        if is_declaration_context(source, abs_pos, member_name)
            && let Some(doc) = extract_preceding_doc_comment(source, abs_pos)
        {
            return Some(doc);
        }

        search_start = abs_pos + member_name.len();
    }

    None
}

/// Check if the position is likely a member declaration rather than a usage.
///
/// Simple heuristic: the character before the member name (after skipping whitespace)
/// should be a type name character, a generic closing bracket, or an annotation.
fn is_declaration_context(source: &str, pos: usize, member_name: &str) -> bool {
    // Check the character before the match.
    if pos == 0 {
        return false;
    }

    let before = &source[..pos];
    let before_trimmed = before.trim_end();
    if before_trimmed.is_empty() {
        return false;
    }

    let last_char = before_trimmed.chars().next_back().unwrap_or(' ');

    // After the member name, check for method signature `(` or field assignment/terminator.
    let after_pos = pos + member_name.len();
    let after = if after_pos < source.len() {
        source[after_pos..].trim_start()
    } else {
        ""
    };

    let after_char = after.chars().next().unwrap_or(' ');

    // Declaration context: preceded by a type (letter, >, ]) and followed by ( ; = { or end.
    let valid_before = last_char.is_alphanumeric() || last_char == '>' || last_char == ']';
    let valid_after = matches!(after_char, '(' | ';' | '=' | '{' | ':' | '\n');

    valid_before && valid_after
}

/// Find the position of a declaration keyword pattern in source text.
///
/// Ensures the match is a whole word (not part of a larger identifier).
fn find_declaration_position(source: &str, pattern: &str) -> Option<usize> {
    let mut search_start = 0;
    while search_start < source.len() {
        let remaining = &source[search_start..];
        let offset = remaining.find(pattern)?;
        let abs_pos = search_start + offset;

        // Verify whole-word boundary before the pattern.
        if abs_pos > 0 {
            let prev_char = source.as_bytes()[abs_pos - 1];
            if prev_char.is_ascii_alphanumeric() || prev_char == b'_' {
                search_start = abs_pos + pattern.len();
                continue;
            }
        }

        // Verify boundary after the pattern (should be whitespace, <, (, {, etc.).
        let end_pos = abs_pos + pattern.len();
        if end_pos < source.len() {
            let next_char = source.as_bytes()[end_pos];
            if next_char.is_ascii_alphanumeric() || next_char == b'_' {
                search_start = end_pos;
                continue;
            }
        }

        return Some(abs_pos);
    }

    None
}

/// Extract the `/** ... */` doc comment that immediately precedes the given position.
///
/// Searches backwards from `pos` for the closest `*/` and then its matching `/**`.
/// Ensures there is no non-whitespace/non-annotation content between the comment
/// end and the declaration.
fn extract_preceding_doc_comment(source: &str, pos: usize) -> Option<String> {
    let before = &source[..pos];

    // Find the last `*/` before this position.
    let comment_end = before.rfind("*/")?;
    let comment_end_full = comment_end + 2;

    // Check that between the comment end and the declaration, there's only
    // whitespace and annotations.
    let between = before[comment_end_full..].trim();
    if !between.is_empty() && !is_only_annotations_modifiers_and_types(between) {
        return None;
    }

    // Find the matching `/**`.
    let before_end = &source[..=comment_end];
    let comment_start = before_end.rfind("/**")?;

    // Extract and clean the doc comment.
    let raw_comment = &source[comment_start..comment_end_full];
    Some(clean_doc_comment(raw_comment))
}

/// Check if text between a doc comment and a declaration contains only
/// annotations, access modifiers, and type signatures.
///
/// This is intentionally permissive: it allows any identifier-like tokens
/// (type names, generics, array brackets) that can appear between a doc
/// comment and the declared member name. The key exclusions are statement
/// terminators and assignment operators that would indicate intervening code.
fn is_only_annotations_modifiers_and_types(text: &str) -> bool {
    // If the text contains statement-like constructs, it's not just declarations.
    if text.contains(';') || text.contains("return ") || text.contains("throw ") {
        return false;
    }

    for token in text.split_whitespace() {
        // Allow annotations like @Override, @Deprecated, @SuppressWarnings("...")
        if token.starts_with('@') {
            continue;
        }
        // Allow annotation arguments in parentheses.
        if token.starts_with('(') || token.ends_with(')') || token.ends_with(',') {
            continue;
        }
        // Allow string literals within annotations.
        if token.starts_with('"') || token.ends_with('"') {
            continue;
        }
        // Allow identifier-like tokens (modifiers, types, generics).
        // These match: public, static, String, List<String>, int, void,
        // Map<K,V>, Optional<T>, int[], byte[], etc.
        if is_declaration_token(token) {
            continue;
        }
        return false;
    }
    true
}

/// Check if a token looks like it belongs in a declaration signature.
///
/// Allows: identifiers, generic types (`List<String>`), array types (`int[]`),
/// keywords, wildcards (`?`), bounds (`extends`, `super`).
fn is_declaration_token(token: &str) -> bool {
    token.chars().all(|c| {
        c.is_alphanumeric()
            || c == '_'
            || c == '<'
            || c == '>'
            || c == '['
            || c == ']'
            || c == '.'
            || c == ','
            || c == '?'
    })
}

/// Clean a raw `/** ... */` doc comment into plain text.
///
/// Strips the comment delimiters, leading `*` characters, and converts
/// HTML/Javadoc markup to plain text.
fn clean_doc_comment(raw: &str) -> String {
    // Remove the opening `/**` and closing `*/`.
    let content = raw
        .strip_prefix("/**")
        .unwrap_or(raw)
        .strip_suffix("*/")
        .unwrap_or(raw);

    let mut lines: Vec<String> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        // Strip leading `*` (with optional space after).
        let cleaned = if let Some(rest) = trimmed.strip_prefix("* ") {
            rest
        } else if let Some(rest) = trimmed.strip_prefix('*') {
            rest
        } else {
            trimmed
        };
        lines.push(cleaned.to_string());
    }

    let joined = lines.join("\n");
    let result = convert_html_to_plain_text(&joined);

    // Trim leading/trailing blank lines and normalize whitespace.
    let trimmed = result.trim();
    normalize_blank_lines(trimmed)
}

/// Convert HTML and Javadoc inline tags to plain text.
fn convert_html_to_plain_text(text: &str) -> String {
    let mut result = text.to_string();

    // Convert {@code text} -> `text`
    result = replace_inline_tag(&result, "code");

    // Convert {@link Type#method} -> Type.method
    result = replace_link_tags(&result);

    // Convert {@literal text} -> text
    result = replace_literal_tags(&result);

    // Convert {@value ...} -> the value reference.
    result = replace_value_tags(&result);

    // Convert HTML tags.
    result = convert_html_tags(&result);

    // Convert block tags (@param, @return, @throws, etc.).
    result = convert_block_tags(&result);

    result
}

/// Replace `{@code text}` with `` `text` ``.
fn replace_inline_tag(text: &str, tag_name: &str) -> String {
    let open_pattern = format!("{{@{tag_name} ");
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(start) = remaining.find(&open_pattern) {
        result.push_str(&remaining[..start]);
        let after_tag = &remaining[start + open_pattern.len()..];

        if let Some(close) = find_matching_brace(after_tag) {
            let content = &after_tag[..close];
            result.push('`');
            result.push_str(content.trim());
            result.push('`');
            remaining = &after_tag[close + 1..];
        } else {
            // No matching brace; keep as-is.
            result.push_str(&remaining[start..start + open_pattern.len()]);
            remaining = after_tag;
        }
    }
    result.push_str(remaining);
    result
}

/// Find the position of the matching `}` for a `{@tag ...}` construct,
/// handling nested braces.
fn find_matching_brace(text: &str) -> Option<usize> {
    let mut depth = 0u32;
    for (i, ch) in text.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// Replace `{@link Type#method}` with `Type.method`.
fn replace_link_tags(text: &str) -> String {
    let open_patterns = ["{@link ", "{@linkplain "];
    let mut result = text.to_string();

    for open_pattern in &open_patterns {
        let mut new_result = String::with_capacity(result.len());
        let mut remaining = result.as_str();

        while let Some(start) = remaining.find(open_pattern) {
            new_result.push_str(&remaining[..start]);
            let after_tag = &remaining[start + open_pattern.len()..];

            if let Some(close) = find_matching_brace(after_tag) {
                let content = after_tag[..close].trim();
                // If there's a display label (space-separated), use that.
                // Otherwise, convert # to .
                let display = if let Some(label_start) = content.find(' ') {
                    content[label_start + 1..].trim()
                } else {
                    content
                };
                new_result.push_str(&display.replace('#', "."));
                remaining = &after_tag[close + 1..];
            } else {
                new_result.push_str(&remaining[start..start + open_pattern.len()]);
                remaining = after_tag;
            }
        }
        new_result.push_str(remaining);
        result = new_result;
    }

    result
}

/// Replace `{@literal text}` with backtick-wrapped text.
///
/// `{@literal}` in Javadoc means "display as-is without HTML interpretation".
/// We wrap the content in backticks to protect it from HTML tag stripping
/// and to signal that it is a literal/code-like reference.
fn replace_literal_tags(text: &str) -> String {
    let open_pattern = "{@literal ";
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(start) = remaining.find(open_pattern) {
        result.push_str(&remaining[..start]);
        let after_tag = &remaining[start + open_pattern.len()..];

        if let Some(close) = find_matching_brace(after_tag) {
            result.push('`');
            result.push_str(after_tag[..close].trim());
            result.push('`');
            remaining = &after_tag[close + 1..];
        } else {
            result.push_str(&remaining[start..start + open_pattern.len()]);
            remaining = after_tag;
        }
    }
    result.push_str(remaining);
    result
}

/// Replace `{@value ...}` with the reference.
fn replace_value_tags(text: &str) -> String {
    let open_pattern = "{@value ";
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(start) = remaining.find(open_pattern) {
        result.push_str(&remaining[..start]);
        let after_tag = &remaining[start + open_pattern.len()..];

        if let Some(close) = find_matching_brace(after_tag) {
            result.push_str(after_tag[..close].trim());
            remaining = &after_tag[close + 1..];
        } else {
            result.push_str(&remaining[start..start + open_pattern.len()]);
            remaining = after_tag;
        }
    }
    result.push_str(remaining);
    result
}

/// Convert HTML tags to plain text equivalents.
fn convert_html_tags(text: &str) -> String {
    let mut result = text.to_string();

    // <p> and </p> -> newline
    result = result.replace("<p>", "\n");
    result = result.replace("</p>", "");
    result = result.replace("<P>", "\n");

    // <br> and <br/> -> newline
    result = result.replace("<br>", "\n");
    result = result.replace("<br/>", "\n");
    result = result.replace("<br />", "\n");
    result = result.replace("<BR>", "\n");

    // <code>...</code> -> `...`
    result = result.replace("<code>", "`");
    result = result.replace("</code>", "`");
    result = result.replace("<CODE>", "`");
    result = result.replace("</CODE>", "`");

    // <pre>...</pre> -> preserve content (already plain text in code blocks).
    result = result.replace("<pre>", "");
    result = result.replace("</pre>", "");
    result = result.replace("<PRE>", "");
    result = result.replace("</PRE>", "");

    // <b>/<strong> -> keep content.
    result = result.replace("<b>", "");
    result = result.replace("</b>", "");
    result = result.replace("<strong>", "");
    result = result.replace("</strong>", "");
    result = result.replace("<B>", "");
    result = result.replace("</B>", "");

    // <i>/<em> -> keep content.
    result = result.replace("<i>", "");
    result = result.replace("</i>", "");
    result = result.replace("<em>", "");
    result = result.replace("</em>", "");
    result = result.replace("<I>", "");
    result = result.replace("</I>", "");

    // <ul>/<ol>/<li> -> basic list formatting.
    result = result.replace("<ul>", "");
    result = result.replace("</ul>", "");
    result = result.replace("<ol>", "");
    result = result.replace("</ol>", "");
    result = result.replace("<li>", "\n- ");
    result = result.replace("</li>", "");

    // <tt> (teletype) -> backticks.
    result = result.replace("<tt>", "`");
    result = result.replace("</tt>", "`");

    // Strip remaining HTML tags.
    strip_remaining_html_tags(&result)
}

/// Strip any remaining HTML tags from the text.
///
/// Content inside backticks is preserved verbatim (e.g., `` `Map<K, V>` ``
/// should not have `<K, V>` stripped as an HTML tag).
fn strip_remaining_html_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_tag = false;
    let mut in_backtick = false;

    for ch in text.chars() {
        if ch == '`' {
            in_backtick = !in_backtick;
            result.push(ch);
        } else if in_backtick {
            // Inside backticks, preserve everything verbatim.
            result.push(ch);
        } else {
            match ch {
                '<' => in_tag = true,
                '>' if in_tag => in_tag = false,
                _ if !in_tag => result.push(ch),
                _ => {}
            }
        }
    }

    result
}

/// Convert Javadoc block tags (@param, @return, @throws, etc.) to plain text.
fn convert_block_tags(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("@param ") {
            let parts: Vec<&str> = rest.splitn(2, ' ').collect();
            if parts.len() == 2 {
                lines.push(format!("param {}: {}", parts[0], parts[1]));
            } else {
                lines.push(format!("param {rest}"));
            }
        } else if let Some(rest) = trimmed.strip_prefix("@return ") {
            lines.push(format!("returns: {rest}"));
        } else if let Some(rest) = trimmed.strip_prefix("@returns ") {
            lines.push(format!("returns: {rest}"));
        } else if let Some(rest) = trimmed.strip_prefix("@throws ") {
            let parts: Vec<&str> = rest.splitn(2, ' ').collect();
            if parts.len() == 2 {
                lines.push(format!("throws {}: {}", parts[0], parts[1]));
            } else {
                lines.push(format!("throws {rest}"));
            }
        } else if let Some(rest) = trimmed.strip_prefix("@exception ") {
            let parts: Vec<&str> = rest.splitn(2, ' ').collect();
            if parts.len() == 2 {
                lines.push(format!("throws {}: {}", parts[0], parts[1]));
            } else {
                lines.push(format!("throws {rest}"));
            }
        } else if let Some(rest) = trimmed.strip_prefix("@see ") {
            lines.push(format!("see: {rest}"));
        } else if let Some(rest) = trimmed.strip_prefix("@since ") {
            lines.push(format!("since: {rest}"));
        } else if let Some(rest) = trimmed.strip_prefix("@version ") {
            lines.push(format!("version: {rest}"));
        } else if let Some(rest) = trimmed.strip_prefix("@author ") {
            lines.push(format!("author: {rest}"));
        } else if trimmed.starts_with("@deprecated") {
            let rest = trimmed
                .strip_prefix("@deprecated ")
                .unwrap_or("(deprecated)");
            lines.push(format!("DEPRECATED: {rest}"));
        } else {
            lines.push(line.to_string());
        }
    }

    lines.join("\n")
}

/// Normalize consecutive blank lines to at most one.
fn normalize_blank_lines(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut prev_blank = false;

    for line in text.lines() {
        let is_blank = line.trim().is_empty();
        if is_blank {
            if !prev_blank {
                result.push('\n');
            }
            prev_blank = true;
        } else {
            if prev_blank && !result.is_empty() {
                result.push('\n');
            }
            if !result.is_empty() && !prev_blank {
                result.push('\n');
            }
            result.push_str(line);
            prev_blank = false;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// Helper: create a ZIP file in memory containing the given entries.
    fn create_source_jar(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (path, content) in entries {
                writer.start_file((*path).to_string(), options).unwrap();
                writer.write_all(content.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        buf
    }

    /// Helper: write a source JAR to a temp directory and create classpath entries.
    fn setup_provider(
        dir: &tempfile::TempDir,
        entries: &[(&str, &str)],
    ) -> (SourceJarProvider, PathBuf) {
        let source_jar_path = dir.path().join("lib-sources.jar");
        let jar_data = create_source_jar(entries);
        std::fs::write(&source_jar_path, jar_data).unwrap();

        let binary_jar_path = dir.path().join("lib.jar");
        std::fs::write(&binary_jar_path, b"fake binary jar").unwrap();

        let classpath_entries = vec![ClasspathEntry {
            jar_path: binary_jar_path.clone(),
            coordinates: Some("com.example:lib:1.0".to_string()),
            is_direct: true,
            source_jar: Some(source_jar_path),
        }];

        let provider = SourceJarProvider::new(&classpath_entries, 100);
        (provider, binary_jar_path)
    }

    #[test]
    fn test_extract_javadoc_from_simple_class() {
        let dir = tempfile::tempdir().unwrap();
        let source = r"package com.example;

/**
 * A simple utility class for string operations.
 *
 * This class provides common string manipulation methods.
 */
public class StringUtils {
    public static String trim(String s) {
        return s.trim();
    }
}
";
        let (provider, jar_path) =
            setup_provider(&dir, &[("com/example/StringUtils.java", source)]);

        let docs = provider
            .get_docs("com.example.StringUtils", &jar_path)
            .unwrap();
        assert!(docs.contains("A simple utility class for string operations"));
        assert!(docs.contains("common string manipulation methods"));
    }

    #[test]
    fn test_extract_method_level_javadoc() {
        let dir = tempfile::tempdir().unwrap();
        let source = r"package com.example;

/**
 * String utilities.
 */
public class StringUtils {
    /**
     * Trims whitespace from both ends of a string.
     *
     * @param s the input string
     * @return the trimmed string
     */
    public static String trim(String s) {
        return s.trim();
    }
}
";
        let (provider, jar_path) =
            setup_provider(&dir, &[("com/example/StringUtils.java", source)]);

        let docs = provider
            .get_member_docs("com.example.StringUtils", "trim", &jar_path)
            .unwrap();
        assert!(docs.contains("Trims whitespace from both ends"));
        assert!(docs.contains("param s:"));
        assert!(docs.contains("returns:"));
    }

    #[test]
    fn test_missing_source_jar_returns_none() {
        let entries = vec![ClasspathEntry {
            jar_path: PathBuf::from("/nonexistent/lib.jar"),
            coordinates: None,
            is_direct: true,
            source_jar: None,
        }];
        let provider = SourceJarProvider::new(&entries, 100);
        let result = provider.get_docs("com.example.Foo", Path::new("/nonexistent/lib.jar"));
        assert!(result.is_none());
    }

    #[test]
    fn test_class_not_found_in_source_jar_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let source = "package com.example;\npublic class Other {}\n";
        let (provider, jar_path) = setup_provider(&dir, &[("com/example/Other.java", source)]);

        let result = provider.get_docs("com.example.Missing", &jar_path);
        assert!(result.is_none());
    }

    #[test]
    fn test_html_to_plain_text_conversion() {
        let input = "First paragraph.<p>Second paragraph.\n<code>some code</code> and text.";
        let result = convert_html_to_plain_text(input);
        assert!(result.contains("First paragraph."));
        assert!(result.contains("Second paragraph."));
        assert!(result.contains("`some code`"));
    }

    #[test]
    fn test_code_and_link_tag_conversion() {
        let input = "Use {@code Map<K, V>} for mappings. See {@link HashMap#get the get method}.";
        let result = convert_html_to_plain_text(input);
        assert!(result.contains("`Map<K, V>`"));
        assert!(result.contains("the get method"));
    }

    #[test]
    fn test_param_return_throws_conversion() {
        let input = "@param name the user name\n@return the greeting\n@throws IllegalArgumentException if name is null";
        let result = convert_block_tags(input);
        assert!(result.contains("param name: the user name"));
        assert!(result.contains("returns: the greeting"));
        assert!(result.contains("throws IllegalArgumentException: if name is null"));
    }

    #[test]
    fn test_cache_hit_returns_same_result() {
        let dir = tempfile::tempdir().unwrap();
        let source = r"package com.example;
/**
 * Cached class docs.
 */
public class Cached {
}
";
        let (provider, jar_path) = setup_provider(&dir, &[("com/example/Cached.java", source)]);

        let first = provider.get_docs("com.example.Cached", &jar_path);
        let second = provider.get_docs("com.example.Cached", &jar_path);
        assert_eq!(first, second);
        assert!(first.is_some());

        // Verify it's actually in the cache.
        let cache = provider.cache.lock().unwrap();
        assert!(!cache.is_empty());
    }

    #[test]
    fn test_lru_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let mut jar_entries = Vec::new();

        // Create 5 classes.
        for i in 0..5 {
            let name = format!("Class{i}");
            let path = format!("com/example/{name}.java");
            let content =
                format!("package com.example;\n/** Doc for {name}. */\npublic class {name} {{}}\n");
            jar_entries.push((path, content));
        }

        let jar_entry_refs: Vec<(&str, &str)> = jar_entries
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_str()))
            .collect();

        let source_jar_path = dir.path().join("lib-sources.jar");
        let jar_data = create_source_jar(&jar_entry_refs);
        std::fs::write(&source_jar_path, jar_data).unwrap();

        let binary_jar_path = dir.path().join("lib.jar");
        std::fs::write(&binary_jar_path, b"fake").unwrap();

        let entries = vec![ClasspathEntry {
            jar_path: binary_jar_path.clone(),
            coordinates: None,
            is_direct: true,
            source_jar: Some(source_jar_path),
        }];

        // Cache size of 3 means older entries get evicted.
        let provider = SourceJarProvider::new(&entries, 3);

        // Access 5 classes with cache size 3.
        for i in 0..5 {
            let fqn = format!("com.example.Class{i}");
            let result = provider.get_docs(&fqn, &binary_jar_path);
            assert!(result.is_some(), "Class{i} should have docs");
        }

        // Cache should have at most 3 entries.
        let cache = provider.cache.lock().unwrap();
        assert!(cache.len() <= 3, "LRU cache should evict to capacity");
    }

    #[test]
    fn test_kotlin_extension_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let source = r"package com.example

/**
 * A Kotlin data class.
 */
data class UserProfile(val name: String, val age: Int)
";
        let (provider, jar_path) = setup_provider(&dir, &[("com/example/UserProfile.kt", source)]);

        let docs = provider
            .get_docs("com.example.UserProfile", &jar_path)
            .unwrap();
        assert!(docs.contains("Kotlin data class"));
    }

    #[test]
    fn test_scala_extension_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let source = r"package com.example

/**
 * A Scala case class for configuration.
 */
case class AppConfig(host: String, port: Int)
";
        // Scala uses the same /** */ doc comments.
        let (provider, jar_path) = setup_provider(&dir, &[("com/example/AppConfig.scala", source)]);

        let docs = provider
            .get_docs("com.example.AppConfig", &jar_path)
            .unwrap();
        assert!(docs.contains("Scala case class for configuration"));
    }

    #[test]
    fn test_multiline_doc_comment() {
        let dir = tempfile::tempdir().unwrap();
        let source = r#"package com.example;

/**
 * An immutable, ordered collection of elements.
 *
 * <p>This is the second paragraph with more details.
 *
 * <p>Usage example:
 * <pre>
 *   ImmutableList<String> list = ImmutableList.of("a", "b", "c");
 * </pre>
 *
 * @param <E> the element type
 * @since 2.0
 * @see java.util.List
 * @author Google
 */
public class ImmutableList<E> {
}
"#;
        let (provider, jar_path) =
            setup_provider(&dir, &[("com/example/ImmutableList.java", source)]);

        let docs = provider
            .get_docs("com.example.ImmutableList", &jar_path)
            .unwrap();
        assert!(docs.contains("immutable, ordered collection"));
        assert!(docs.contains("second paragraph"));
        assert!(docs.contains("since: 2.0"));
        assert!(docs.contains("see: java.util.List"));
        assert!(docs.contains("author: Google"));
    }

    // --- Unit tests for internal conversion functions ---

    #[test]
    fn test_clean_doc_comment_strips_delimiters() {
        let raw = "/** Simple doc. */";
        let result = clean_doc_comment(raw);
        assert_eq!(result.trim(), "Simple doc.");
    }

    #[test]
    fn test_clean_doc_comment_strips_leading_stars() {
        let raw = "/**\n * Line one.\n * Line two.\n */";
        let result = clean_doc_comment(raw);
        assert!(result.contains("Line one."));
        assert!(result.contains("Line two."));
        assert!(!result.contains("* "));
    }

    #[test]
    fn test_replace_inline_code_tag() {
        assert_eq!(replace_inline_tag("Use {@code foo}.", "code"), "Use `foo`.");
    }

    #[test]
    fn test_replace_inline_code_tag_with_generics() {
        let result = replace_inline_tag("A {@code Map<K, V>} instance.", "code");
        assert_eq!(result, "A `Map<K, V>` instance.");
    }

    #[test]
    fn test_replace_link_tag_simple() {
        let result = replace_link_tags("See {@link String}.");
        assert_eq!(result, "See String.");
    }

    #[test]
    fn test_replace_link_tag_with_member() {
        let result = replace_link_tags("See {@link String#length}.");
        assert_eq!(result, "See String.length.");
    }

    #[test]
    fn test_replace_link_tag_with_label() {
        let result = replace_link_tags("See {@link String#length the length method}.");
        assert_eq!(result, "See the length method.");
    }

    #[test]
    fn test_strip_remaining_html_tags() {
        let input = "Hello <b>world</b> and <unknown-tag>foo</unknown-tag>.";
        let result = strip_remaining_html_tags(input);
        assert_eq!(result, "Hello world and foo.");
    }

    #[test]
    fn test_normalize_blank_lines() {
        let input = "Line 1\n\n\n\nLine 2\n\nLine 3";
        let result = normalize_blank_lines(input);
        // Should have at most one blank line between content lines.
        assert!(!result.contains("\n\n\n"));
    }

    #[test]
    fn test_find_matching_brace_simple() {
        assert_eq!(find_matching_brace("text}rest"), Some(4));
    }

    #[test]
    fn test_find_matching_brace_nested() {
        assert_eq!(find_matching_brace("a{b}c}rest"), Some(5));
    }

    #[test]
    fn test_is_only_annotations_modifiers_and_types() {
        assert!(is_only_annotations_modifiers_and_types("@Override public"));
        assert!(is_only_annotations_modifiers_and_types("@Deprecated"));
        assert!(is_only_annotations_modifiers_and_types(
            "public static final"
        ));
        assert!(!is_only_annotations_modifiers_and_types("int x = 5;"));
    }

    #[test]
    fn test_deprecated_tag_conversion() {
        let input = "@deprecated Use newMethod() instead.";
        let result = convert_block_tags(input);
        assert!(result.contains("DEPRECATED: Use newMethod() instead."));
    }

    #[test]
    fn test_exception_tag_conversion() {
        let input = "@exception IOException if I/O fails";
        let result = convert_block_tags(input);
        assert!(result.contains("throws IOException: if I/O fails"));
    }

    #[test]
    fn test_literal_tag_conversion() {
        let input = "Use {@literal <T>} for generics.";
        let result = convert_html_to_plain_text(input);
        assert!(result.contains("<T>"));
    }

    #[test]
    fn test_class_with_annotations() {
        let dir = tempfile::tempdir().unwrap();
        let source = r#"package com.example;

/**
 * Deprecated utility class.
 */
@Deprecated
@SuppressWarnings("unused")
public final class OldUtils {
}
"#;
        let (provider, jar_path) = setup_provider(&dir, &[("com/example/OldUtils.java", source)]);

        let docs = provider
            .get_docs("com.example.OldUtils", &jar_path)
            .unwrap();
        assert!(docs.contains("Deprecated utility class"));
    }

    #[test]
    fn test_interface_doc_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let source = r"package com.example;

/**
 * A service interface for user management.
 */
public interface UserService {
    void createUser(String name);
}
";
        let (provider, jar_path) =
            setup_provider(&dir, &[("com/example/UserService.java", source)]);

        let docs = provider
            .get_docs("com.example.UserService", &jar_path)
            .unwrap();
        assert!(docs.contains("service interface for user management"));
    }

    #[test]
    fn test_enum_doc_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let source = r"package com.example;

/**
 * Represents the status of an order.
 */
public enum OrderStatus {
    PENDING, SHIPPED, DELIVERED
}
";
        let (provider, jar_path) =
            setup_provider(&dir, &[("com/example/OrderStatus.java", source)]);

        let docs = provider
            .get_docs("com.example.OrderStatus", &jar_path)
            .unwrap();
        assert!(docs.contains("status of an order"));
    }

    #[test]
    fn test_no_doc_comment_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let source = r"package com.example;

public class NoDoc {
}
";
        let (provider, jar_path) = setup_provider(&dir, &[("com/example/NoDoc.java", source)]);

        let result = provider.get_docs("com.example.NoDoc", &jar_path);
        assert!(result.is_none());
    }

    #[test]
    fn test_member_not_found_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let source = r"package com.example;

/**
 * A class.
 */
public class MyClass {
    public void existingMethod() {}
}
";
        let (provider, jar_path) = setup_provider(&dir, &[("com/example/MyClass.java", source)]);

        let result = provider.get_member_docs("com.example.MyClass", "nonExistent", &jar_path);
        assert!(result.is_none());
    }

    #[test]
    fn test_cache_none_for_missing_docs() {
        let dir = tempfile::tempdir().unwrap();
        let source = "package com.example;\npublic class NoDoc {}\n";
        let (provider, jar_path) = setup_provider(&dir, &[("com/example/NoDoc.java", source)]);

        // First call caches None.
        let first = provider.get_docs("com.example.NoDoc", &jar_path);
        assert!(first.is_none());

        // Second call should hit cache (also None).
        let second = provider.get_docs("com.example.NoDoc", &jar_path);
        assert!(second.is_none());

        // Verify it's cached.
        let cache = provider.cache.lock().unwrap();
        let key = format!("{}::com.example.NoDoc", jar_path.display());
        assert!(cache.peek(&key).is_some());
    }

    #[test]
    fn test_with_defaults_constructor() {
        let entries = vec![];
        let provider = SourceJarProvider::with_defaults(&entries);
        // Should not panic and should have empty map.
        assert!(provider.source_jar_map.is_empty());
    }

    #[test]
    #[allow(clippy::similar_names)] // Domain naming is intentional
    fn test_multiple_jars_mapping() {
        let dir = tempfile::tempdir().unwrap();

        // Create two source JARs with different classes.
        let source_a = "package com.a;\n/** Class A. */\npublic class A {}\n";
        let source_b = "package com.b;\n/** Class B. */\npublic class B {}\n";

        let jar_a_src = dir.path().join("a-sources.jar");
        let jar_b_src = dir.path().join("b-sources.jar");
        let jar_a_bin = dir.path().join("a.jar");
        let jar_b_bin = dir.path().join("b.jar");

        std::fs::write(&jar_a_src, create_source_jar(&[("com/a/A.java", source_a)])).unwrap();
        std::fs::write(&jar_b_src, create_source_jar(&[("com/b/B.java", source_b)])).unwrap();
        std::fs::write(&jar_a_bin, b"fake").unwrap();
        std::fs::write(&jar_b_bin, b"fake").unwrap();

        let entries = vec![
            ClasspathEntry {
                jar_path: jar_a_bin.clone(),
                coordinates: None,
                is_direct: true,
                source_jar: Some(jar_a_src),
            },
            ClasspathEntry {
                jar_path: jar_b_bin.clone(),
                coordinates: None,
                is_direct: true,
                source_jar: Some(jar_b_src),
            },
        ];

        let provider = SourceJarProvider::new(&entries, 100);

        let docs_a = provider.get_docs("com.a.A", &jar_a_bin).unwrap();
        assert!(docs_a.contains("Class A"));

        let docs_b = provider.get_docs("com.b.B", &jar_b_bin).unwrap();
        assert!(docs_b.contains("Class B"));

        // Cross-lookup should fail.
        assert!(provider.get_docs("com.a.A", &jar_b_bin).is_none());
    }

    #[test]
    fn test_field_member_docs() {
        let dir = tempfile::tempdir().unwrap();
        let source = r"package com.example;

public class Config {
    /**
     * The maximum number of retries.
     */
    public static final int MAX_RETRIES = 3;

    /**
     * The default timeout in milliseconds.
     */
    private long timeout = 5000;
}
";
        let (provider, jar_path) = setup_provider(&dir, &[("com/example/Config.java", source)]);

        let docs = provider
            .get_member_docs("com.example.Config", "MAX_RETRIES", &jar_path)
            .unwrap();
        assert!(docs.contains("maximum number of retries"));

        let docs = provider
            .get_member_docs("com.example.Config", "timeout", &jar_path)
            .unwrap();
        assert!(docs.contains("default timeout in milliseconds"));
    }
}
