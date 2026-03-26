//! File URI parsing and handling.
//!
//! Handles `file://` URIs according to RFC 8089, including:
//! - Unix paths: `file:///path/to/file`
//! - Windows paths: `file:///C:/path/to/file`
//! - UNC paths: `file://server/share/path`
//! - Percent-encoded characters

use crate::PathError;

/// Parse a `file://` URI and extract the filesystem path.
///
/// # URI Formats
///
/// | URI | Parsed Path |
/// |-----|-------------|
/// | `file:///home/user/file.rs` | `/home/user/file.rs` |
/// | `file:///C:/Users/file.rs` | `C:/Users/file.rs` |
/// | `file://server/share/path` | `\\server\share\path` |
/// | `file:///foo%20bar.rs` | `/foo bar.rs` |
///
/// # Errors
///
/// Returns `PathError::NotFileUri` if input doesn't start with `file://`.
/// Returns `PathError::MalformedFileUri` for invalid syntax.
pub fn parse_file_uri(uri: &str) -> Result<String, PathError> {
    if !uri.starts_with("file://") {
        return Err(PathError::NotFileUri);
    }

    let after_scheme = &uri[7..]; // Skip "file://"

    if after_scheme.is_empty() {
        return Err(PathError::MalformedFileUri);
    }

    // file:///path (Unix) or file:///C:/path (Windows)
    if after_scheme.starts_with('/') {
        // Check for Windows drive letter: file:///C:/path
        let chars: Vec<char> = after_scheme.chars().collect();
        if chars.len() >= 4 && chars[1].is_ascii_alphabetic() && chars[2] == ':' {
            // Windows: file:///C:/path → C:/path
            let path = percent_decode(&after_scheme[1..])?;
            return Ok(path);
        }

        // Unix: file:///path → /path
        percent_decode(after_scheme)
    } else if after_scheme.contains('/') {
        // file://host/path → treat as UNC \\host\path
        let unc = format!("\\\\{}", after_scheme.replace('/', "\\"));
        Ok(unc)
    } else {
        Err(PathError::MalformedFileUri)
    }
}

/// Decode percent-encoded characters in URI paths.
///
/// Handles UTF-8 multi-byte sequences correctly.
fn percent_decode(s: &str) -> Result<String, PathError> {
    let mut result = Vec::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            // Read next two hex digits
            let high = chars.next().ok_or(PathError::MalformedFileUri)?;
            let low = chars.next().ok_or(PathError::MalformedFileUri)?;

            let hex = format!("{high}{low}");
            let byte = u8::from_str_radix(&hex, 16).map_err(|_| PathError::MalformedFileUri)?;
            result.push(byte);
        } else {
            // Direct character (ASCII)
            for byte in c.to_string().bytes() {
                result.push(byte);
            }
        }
    }

    String::from_utf8(result).map_err(|_| PathError::InvalidUtf8)
}

/// Convert a filesystem path to a `file://` URI.
///
/// This is the inverse of `parse_file_uri`.
pub fn path_to_file_uri(path: &str) -> String {
    let normalized = path.replace('\\', "/");

    // Check for Windows drive letter
    let chars: Vec<char> = normalized.chars().collect();
    if chars.len() >= 2 && chars[0].is_ascii_alphabetic() && chars[1] == ':' {
        return format!("file:///{}", percent_encode(&normalized));
    }

    // Check for UNC path (converted to forward slashes)
    if let Some(after_slashes) = normalized.strip_prefix("//") {
        // UNC: //server/share → file://server/share
        return format!("file://{}", percent_encode(after_slashes));
    }

    // Unix absolute path
    if normalized.starts_with('/') {
        return format!("file://{}", percent_encode(&normalized));
    }

    // Relative path - prefix with current directory marker
    format!("file://./{}", percent_encode(&normalized))
}

/// Percent-encode characters that are not allowed in URIs.
fn percent_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len() * 3);

    for c in s.chars() {
        match c {
            // Allowed characters in path segments
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' | '/' | ':' => {
                result.push(c);
            }
            // Everything else gets percent-encoded
            _ => {
                use std::fmt::Write;
                for byte in c.to_string().bytes() {
                    let _ = write!(result, "%{byte:02X}");
                }
            }
        }
    }

    result
}

/// Check if a string looks like a file URI.
#[inline]
#[must_use]
pub fn is_file_uri(s: &str) -> bool {
    s.starts_with("file://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_unix_file_uri() {
        let path = parse_file_uri("file:///home/user/file.rs").unwrap();
        assert_eq!(path, "/home/user/file.rs");
    }

    #[test]
    fn test_parse_windows_file_uri() {
        let path = parse_file_uri("file:///C:/Users/file.rs").unwrap();
        assert_eq!(path, "C:/Users/file.rs");
    }

    #[test]
    fn test_parse_unc_file_uri() {
        let path = parse_file_uri("file://server/share/path").unwrap();
        assert_eq!(path, "\\\\server\\share\\path");
    }

    #[test]
    fn test_parse_percent_encoded() {
        let path = parse_file_uri("file:///foo%20bar.rs").unwrap();
        assert_eq!(path, "/foo bar.rs");
    }

    #[test]
    fn test_parse_root_uri() {
        let path = parse_file_uri("file:///").unwrap();
        assert_eq!(path, "/");
    }

    #[test]
    fn test_not_file_uri() {
        let result = parse_file_uri("https://example.com");
        assert!(matches!(result, Err(PathError::NotFileUri)));
    }

    #[test]
    fn test_malformed_uri() {
        let result = parse_file_uri("file://");
        assert!(matches!(result, Err(PathError::MalformedFileUri)));
    }

    #[test]
    fn test_path_to_file_uri_unix() {
        let uri = path_to_file_uri("/home/user/file.rs");
        assert_eq!(uri, "file:///home/user/file.rs");
    }

    #[test]
    fn test_path_to_file_uri_windows() {
        let uri = path_to_file_uri("C:\\Users\\file.rs");
        assert_eq!(uri, "file:///C:/Users/file.rs");
    }

    #[test]
    fn test_path_to_file_uri_with_spaces() {
        let uri = path_to_file_uri("/foo bar/file.rs");
        assert_eq!(uri, "file:///foo%20bar/file.rs");
    }

    #[test]
    fn test_roundtrip() {
        let original = "/home/user/my project/file.rs";
        let uri = path_to_file_uri(original);
        let parsed = parse_file_uri(&uri).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn test_is_file_uri() {
        assert!(is_file_uri("file:///path"));
        assert!(is_file_uri("file://server/share"));
        assert!(!is_file_uri("/path/to/file"));
        assert!(!is_file_uri("https://example.com"));
    }

    #[test]
    fn test_percent_decode_utf8() {
        // "日本語" in percent-encoded UTF-8
        let path = parse_file_uri("file:///%E6%97%A5%E6%9C%AC%E8%AA%9E.txt").unwrap();
        assert_eq!(path, "/日本語.txt");
    }
}
