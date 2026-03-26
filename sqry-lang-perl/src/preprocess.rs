use std::borrow::Cow;

/// Remove POD sections while preserving line numbers.
pub fn preprocess_content(content: &[u8]) -> Cow<'_, [u8]> {
    if content.is_empty() {
        return Cow::Borrowed(content);
    }

    let text = String::from_utf8_lossy(content);
    let mut result = String::with_capacity(text.len());
    let mut pod_depth: i32 = 0;

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        let trimmed = line.trim_start();

        if trimmed.starts_with('=') {
            let directive = trimmed
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();

            match directive.as_str() {
                "=cut" => {
                    if pod_depth > 0 {
                        pod_depth -= 1;
                    }
                    result.push('\n');
                    continue;
                }
                "=begin" | "=for" | "=pod" | "=head1" | "=head2" | "=head3" | "=head4"
                | "=over" | "=back" | "=item" => {
                    pod_depth += 1;
                    result.push('\n');
                    continue;
                }
                _ => {}
            }
        }

        if pod_depth > 0 {
            result.push('\n');
            continue;
        }

        result.push_str(line);
        result.push('\n');
    }

    if !text.ends_with('\n') {
        result.push('\n');
    }

    Cow::Owned(result.into_bytes())
}
