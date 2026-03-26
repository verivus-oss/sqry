//! Binary file detection utilities
//!
//! Provides heuristics to determine whether a file (or byte slice) should be
//! considered binary. The implementation favors determinism and avoids false
//! positives by combining null-byte checks with a ratio of printable characters.

use std::fs::File;
use std::io::{Read, Result as IoResult};
use std::path::Path;

use crate::config::buffers::DEFAULT_READ_BUFFER;

/// Number of bytes sampled when inspecting a file.
/// Uses the default read buffer size (8 KiB) for consistency.
const SAMPLE_SIZE: usize = DEFAULT_READ_BUFFER;

/// Threshold percentage of non-printable bytes (excluding common whitespace) above which a
/// file is considered binary.
const NON_PRINTABLE_THRESHOLD_PERCENT: usize = 30;

/// Determine if the provided byte slice should be treated as binary.
///
/// The heuristic mirrors common implementations used by tools such as ripgrep
/// and git. A slice is considered binary if:
/// - It contains a NUL byte (`0x00`), or
/// - More than 30% of bytes are outside the printable ASCII range.
#[must_use]
pub fn is_binary_bytes(data: &[u8]) -> bool {
    if data.is_empty() {
        return false;
    }

    if data.contains(&0) {
        return true;
    }

    let non_printable = data
        .iter()
        .filter(|&&byte| !is_printable_ascii(byte))
        .count();

    non_printable.saturating_mul(100) > data.len().saturating_mul(NON_PRINTABLE_THRESHOLD_PERCENT)
}

/// Detect whether a file at `path` is likely binary by sampling the first 8 KiB.
///
/// # Errors
///
/// Returns [`std::io::Error`] when the file cannot be opened or read.
pub fn is_binary_file(path: &Path) -> IoResult<bool> {
    let mut file = File::open(path)?;
    let mut buffer = vec![0u8; SAMPLE_SIZE];
    let read = file.read(&mut buffer)?;
    Ok(is_binary_bytes(&buffer[..read]))
}

fn is_printable_ascii(byte: u8) -> bool {
    matches!(byte, 0x09 | 0x0A | 0x0D | 0x0C | 0x0B | 0x20..=0x7E)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn detects_ascii_text_as_non_binary() {
        let data = b"hello world\nthis is text";
        assert!(!is_binary_bytes(data));
    }

    #[test]
    fn detects_null_byte_as_binary() {
        let data = b"hello\0world";
        assert!(is_binary_bytes(data));
    }

    #[test]
    fn detects_high_ratio_non_printable_as_binary() {
        let data = [0x01u8; 100];
        assert!(is_binary_bytes(&data));
    }

    #[test]
    fn file_detection_respects_null_bytes() {
        let mut temp = NamedTempFile::new().unwrap();
        temp.write_all(b"text before\0text after").unwrap();
        temp.flush().unwrap();
        assert!(is_binary_file(temp.path()).unwrap());
    }

    #[test]
    fn file_detection_handles_text() {
        let mut temp = NamedTempFile::new().unwrap();
        temp.write_all(b"plain ascii text\n").unwrap();
        temp.flush().unwrap();
        assert!(!is_binary_file(temp.path()).unwrap());
    }
}
