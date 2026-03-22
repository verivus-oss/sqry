use std::borrow::Cow;

/// Convert literate Haskell (`.lhs`) content into standard Haskell source.
pub fn preprocess_content(content: &[u8]) -> Cow<'_, [u8]> {
    if content.is_empty() {
        return Cow::Borrowed(content);
    }

    if !looks_literate(content) {
        return Cow::Borrowed(content);
    }

    Cow::Owned(convert_literate(content))
}

/// Heuristic to determine if the file is literate Haskell.
fn looks_literate(content: &[u8]) -> bool {
    let text = String::from_utf8_lossy(content);
    if text.contains("\\begin{code}") {
        return true;
    }

    let mut total = 0usize;
    let mut bird = 0usize;
    for line in text.lines() {
        total += 1;
        if line.trim_start().starts_with('>') {
            bird += 1;
        }
    }

    bird >= 4 || (bird > 0 && bird * 3 >= total) // >= one third of file uses bird tracks
}

/// Convert literate Haskell into token-preserving plain Haskell.
fn convert_literate(content: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(content);
    let mut result = String::with_capacity(text.len());
    let mut in_code_block = false;

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        let trimmed = line.trim_start();

        if trimmed.starts_with("\\begin{code}") {
            in_code_block = true;
            result.push('\n');
            continue;
        }

        if trimmed.starts_with("\\end{code}") {
            in_code_block = false;
            result.push('\n');
            continue;
        }

        if in_code_block {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix('>') {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            result.push_str(rest);
            result.push('\n');
        } else {
            // Preserve line numbers with blank lines.
            result.push('\n');
        }
    }

    if !text.ends_with('\n') {
        result.push('\n');
    }

    result.into_bytes()
}
