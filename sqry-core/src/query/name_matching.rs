//! Shared name matching helpers for relation and validation checks.

/// Returns `true` if `byte` is one of the qualified-name separators recognized
/// across sqry's supported languages (`::`, `.`, `#`, `$`, `/`, `\`).
#[must_use]
pub fn is_separator(byte: u8) -> bool {
    matches!(byte, b'.' | b':' | b'#' | b'$' | b'/' | b'\\')
}

/// Returns `true` if `value` contains any character recognized by
/// [`is_separator`]. Used to detect qualified names without splitting them.
#[must_use]
pub fn contains_separator(value: &str) -> bool {
    value.as_bytes().iter().any(|c| is_separator(*c))
}

/// Returns `true` if `long` ends with `short` on a separator boundary.
///
/// The boundary check prevents false positives such as `"foobar".ends_with("bar")`
/// matching `"bar"` when the caller intended qualified-name segment matching.
#[must_use]
pub fn suffix_match(long: &str, short: &str) -> bool {
    if long.len() <= short.len() {
        return false;
    }
    if !long.ends_with(short) {
        return false;
    }
    let prefix = &long[..long.len() - short.len()];
    prefix.as_bytes().last().is_none_or(|c| is_separator(*c))
}

/// Segment-aware equality. Two qualified names match when one is a suffix of
/// the other at a separator boundary (e.g. `foo::bar::baz` matches `bar::baz`)
/// or when the strings are exactly equal.
#[must_use]
pub fn segments_match(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }

    let a_has_sep = contains_separator(a);
    let b_has_sep = contains_separator(b);

    match (a_has_sep, b_has_sep) {
        (false, true) => suffix_match(b, a),
        (true, false) => suffix_match(a, b),
        (true, true) => suffix_match(a, b) || suffix_match(b, a),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_segments_match_separators() {
        assert!(segments_match("foo::bar", "bar"));
        assert!(segments_match("foo.bar", "bar"));
        assert!(segments_match("foo#bar", "bar"));
        assert!(segments_match("foo$bar", "bar"));
        assert!(segments_match("foo/bar", "bar"));
        assert!(segments_match("foo\\bar", "bar"));
        assert!(!segments_match("foo", "bar"));
    }
}
