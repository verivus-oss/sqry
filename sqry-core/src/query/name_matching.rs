//! Shared name matching helpers for relation and validation checks.

pub(crate) fn is_separator(byte: u8) -> bool {
    matches!(byte, b'.' | b':' | b'#' | b'$' | b'/' | b'\\')
}

pub(crate) fn contains_separator(value: &str) -> bool {
    value.as_bytes().iter().any(|c| is_separator(*c))
}

pub(crate) fn suffix_match(long: &str, short: &str) -> bool {
    if long.len() <= short.len() {
        return false;
    }
    if !long.ends_with(short) {
        return false;
    }
    let prefix = &long[..long.len() - short.len()];
    prefix.as_bytes().last().is_none_or(|c| is_separator(*c))
}

pub(crate) fn segments_match(a: &str, b: &str) -> bool {
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
