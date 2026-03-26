use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose};

const CURSOR_PREFIX: &str = "offset:";

#[must_use]
pub fn encode_cursor(offset: usize) -> String {
    let payload = format!("{CURSOR_PREFIX}{offset}");
    general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes())
}

/// Decode a cursor string into an offset.
///
/// # Errors
///
/// Returns an error when the cursor is malformed, not UTF-8, or cannot be parsed.
pub fn decode_cursor(cursor: &str) -> Result<usize> {
    if cursor.trim().is_empty() {
        return Ok(0);
    }

    if let Ok(value) = cursor.parse::<usize>() {
        return Ok(value);
    }

    if let Some(rest) = cursor.strip_prefix(CURSOR_PREFIX) {
        return rest
            .parse::<usize>()
            .map_err(|_| anyhow!("Invalid cursor payload"));
    }

    let decoded = general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .with_context(|| format!("Failed to decode cursor '{cursor}'"))?;
    let decoded_str = String::from_utf8(decoded)
        .with_context(|| format!("Cursor '{cursor}' is not valid UTF-8"))?;

    let Some(rest) = decoded_str.strip_prefix(CURSOR_PREFIX) else {
        bail!("Invalid cursor payload");
    };

    rest.parse::<usize>()
        .map_err(|_| anyhow!("Invalid cursor payload"))
}
