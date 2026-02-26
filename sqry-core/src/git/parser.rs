//! Parsers for git command output
//!
//! This module provides parsers for git porcelain and diff output formats.
//! All output is expected to be null-terminated (-z flag) to handle filenames
//! with spaces, newlines, and other special characters.

use super::{ChangeSet, GitError, Result};
use std::iter::Peekable;
use std::path::PathBuf;
use std::str::Split;

type NullSplit<'a> = Peekable<Split<'a, char>>;

fn parse_porcelain_entry(
    entry: &str,
    entries: &mut NullSplit<'_>,
    changeset: &mut ChangeSet,
) -> Result<()> {
    if entry.len() < 3 {
        return Err(GitError::InvalidOutput(format!(
            "Status entry too short: '{entry}'"
        )));
    }

    let status = &entry[..2];
    if entry.as_bytes().get(2).copied() != Some(b' ') {
        return Err(GitError::InvalidOutput(format!(
            "Missing space separator after status in entry: '{entry}'"
        )));
    }
    let filename = &entry[3..];
    if filename.is_empty() {
        return Err(GitError::InvalidOutput(
            "Missing filename in status entry".to_string(),
        ));
    }

    match status {
        "A " | " A" | "AM" | "??" => {
            changeset.added.push(PathBuf::from(filename));
        }
        "M " | " M" | "MM" | "UU" | "AA" | "AU" | "UA" => {
            changeset.modified.push(PathBuf::from(filename));
        }
        "D " | " D" | "DD" | "DU" | "UD" => {
            changeset.deleted.push(PathBuf::from(filename));
        }
        "R " | "RM" => {
            let old_path = PathBuf::from(filename);
            let Some(new_filename) = entries.next() else {
                return Err(GitError::InvalidOutput(format!(
                    "Rename entry missing new filename: '{entry}'"
                )));
            };
            if new_filename.is_empty() {
                return Err(GitError::InvalidOutput(
                    "Rename entry missing new filename".to_string(),
                ));
            }
            changeset
                .renamed
                .push((old_path, PathBuf::from(new_filename)));
        }
        "C " | "CM" => {
            if entries.next().is_none() {
                return Err(GitError::InvalidOutput(
                    "Copy entry missing old filename".to_string(),
                ));
            }
            changeset.added.push(PathBuf::from(filename));
        }
        "!!" => {}
        _ => {
            log::warn!("Unknown git status code: '{status}' for file '{filename}'");
        }
    }

    Ok(())
}

fn parse_diff_entry(
    status_code: &str,
    entries: &mut NullSplit<'_>,
    changeset: &mut ChangeSet,
) -> Result<()> {
    let status_char = status_code.chars().next().ok_or_else(|| {
        GitError::InvalidOutput(format!(
            "Missing status character in diff entry: '{status_code}'"
        ))
    })?;

    match status_char {
        'A' => {
            let Some(filename) = entries.next() else {
                return Err(GitError::InvalidOutput(format!(
                    "Added entry missing filename after status '{status_code}'"
                )));
            };
            if filename.is_empty() {
                return Err(GitError::InvalidOutput(
                    "Added entry missing filename".to_string(),
                ));
            }
            changeset.added.push(PathBuf::from(filename));
        }
        'M' | 'T' | 'U' | 'X' => {
            let Some(filename) = entries.next() else {
                return Err(GitError::InvalidOutput(format!(
                    "Modified entry missing filename after status '{status_code}'"
                )));
            };
            if filename.is_empty() {
                return Err(GitError::InvalidOutput(
                    "Modified entry missing filename".to_string(),
                ));
            }
            changeset.modified.push(PathBuf::from(filename));
        }
        'D' => {
            let Some(filename) = entries.next() else {
                return Err(GitError::InvalidOutput(format!(
                    "Deleted entry missing filename after status '{status_code}'"
                )));
            };
            if filename.is_empty() {
                return Err(GitError::InvalidOutput(
                    "Deleted entry missing filename".to_string(),
                ));
            }
            changeset.deleted.push(PathBuf::from(filename));
        }
        'R' => {
            let Some(old_filename) = entries.next() else {
                return Err(GitError::InvalidOutput(format!(
                    "Rename entry missing old filename after status '{status_code}'"
                )));
            };
            if old_filename.is_empty() {
                return Err(GitError::InvalidOutput(
                    "Rename entry missing old filename".to_string(),
                ));
            }
            let Some(new_filename) = entries.next() else {
                return Err(GitError::InvalidOutput(format!(
                    "Rename entry missing new filename after old '{old_filename}'"
                )));
            };
            if new_filename.is_empty() {
                return Err(GitError::InvalidOutput(
                    "Rename entry missing new filename".to_string(),
                ));
            }
            changeset
                .renamed
                .push((PathBuf::from(old_filename), PathBuf::from(new_filename)));
        }
        'C' => {
            let Some(old_filename) = entries.next() else {
                return Err(GitError::InvalidOutput(format!(
                    "Copy entry missing old filename after status '{status_code}'"
                )));
            };
            if old_filename.is_empty() {
                return Err(GitError::InvalidOutput(
                    "Copy entry missing old filename".to_string(),
                ));
            }
            let Some(new_filename) = entries.next() else {
                return Err(GitError::InvalidOutput(
                    "Copy entry missing new filename".to_string(),
                ));
            };
            if new_filename.is_empty() {
                return Err(GitError::InvalidOutput(
                    "Copy entry missing new filename".to_string(),
                ));
            }
            changeset.added.push(PathBuf::from(new_filename));
        }
        _ => {
            return Err(GitError::InvalidOutput(format!(
                "Unknown diff status code: '{status_code}'"
            )));
        }
    }

    Ok(())
}

/// Parse `git status --porcelain=v1 -z` output
///
/// Format: `XY filename\0` where:
/// - X = index status
/// - Y = working tree status
/// - filename = path relative to repo root
///
/// Special cases:
/// - Renames: `R  old\0new\0`
/// - Copies: `C  old\0new\0`
/// - Untracked: `?? filename\0`
/// - Ignored: `!! filename\0`
/// - Merge conflicts: `UU filename\0`, `AA filename\0`, etc.
///
/// See: <https://git-scm.com/docs/git-status#_short_format>
///
/// # Errors
///
/// Returns [`GitError::InvalidOutput`] when the porcelain stream is malformed
/// (missing status codes, filenames, or rename/copy metadata).
pub fn parse_porcelain(output: &str) -> Result<ChangeSet> {
    let mut changeset = ChangeSet::new();

    if output.is_empty() {
        return Ok(changeset);
    }

    // Split by null terminator
    let mut entries: NullSplit<'_> = output.split('\0').peekable();

    while let Some(entry) = entries.next() {
        if entry.is_empty() {
            // Trailing NUL can produce an empty entry; skip it.
            continue;
        }

        parse_porcelain_entry(entry, &mut entries, &mut changeset)?;
    }

    Ok(changeset)
}

/// Parse `git diff --name-status -z` output
///
/// Format: `status\0filename\0` or `status\0old\0new\0` for renames
///
/// Status codes:
/// - A = added
/// - M = modified
/// - D = deleted
/// - `R<similarity>` = renamed (e.g., R075)
/// - `C<similarity>` = copied
/// - T = type change (treat as modified)
/// - U = unmerged (treat as modified)
/// - X = unknown (treat as modified)
///
/// See: <https://git-scm.com/docs/git-diff#_diff_format>
///
/// # Errors
///
/// Returns [`GitError::InvalidOutput`] when the diff stream is truncated or
/// otherwise malformed (missing filenames, rename metadata, or status codes).
pub fn parse_diff_name_status(output: &str) -> Result<ChangeSet> {
    let mut changeset = ChangeSet::new();

    if output.is_empty() {
        return Ok(changeset);
    }

    // Split by null terminator
    let mut entries: NullSplit<'_> = output.split('\0').peekable();

    while let Some(status_code) = entries.next() {
        if status_code.is_empty() {
            // Trailing NUL yields empty entries; skip
            continue;
        }

        parse_diff_entry(status_code, &mut entries, &mut changeset)?;
    }

    Ok(changeset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_porcelain_empty() {
        let changes = parse_porcelain("").unwrap();
        assert!(changes.is_empty());
    }

    #[test]
    fn test_parse_porcelain_modified() {
        let output = "M  file1.rs\0 M file2.rs\0";
        let changes = parse_porcelain(output).unwrap();
        assert_eq!(changes.modified.len(), 2);
        assert_eq!(changes.modified[0], PathBuf::from("file1.rs"));
        assert_eq!(changes.modified[1], PathBuf::from("file2.rs"));
    }

    #[test]
    fn test_parse_porcelain_added() {
        let output = "A  new.rs\0?? untracked.rs\0";
        let changes = parse_porcelain(output).unwrap();
        assert_eq!(changes.added.len(), 2);
        assert_eq!(changes.added[0], PathBuf::from("new.rs"));
        assert_eq!(changes.added[1], PathBuf::from("untracked.rs"));
    }

    #[test]
    fn test_parse_porcelain_deleted() {
        let output = "D  deleted.rs\0 D removed.rs\0";
        let changes = parse_porcelain(output).unwrap();
        assert_eq!(changes.deleted.len(), 2);
        assert_eq!(changes.deleted[0], PathBuf::from("deleted.rs"));
        assert_eq!(changes.deleted[1], PathBuf::from("removed.rs"));
    }

    #[test]
    fn test_parse_porcelain_renamed() {
        let output = "R  old.rs\0new.rs\0";
        let changes = parse_porcelain(output).unwrap();
        assert_eq!(changes.renamed.len(), 1);
        assert_eq!(changes.renamed[0].0, PathBuf::from("old.rs"));
        assert_eq!(changes.renamed[0].1, PathBuf::from("new.rs"));
    }

    #[test]
    fn test_parse_porcelain_merge_conflicts_uu() {
        let output = "UU conflict.rs\0";
        let changes = parse_porcelain(output).unwrap();
        assert_eq!(changes.modified.len(), 1);
        assert_eq!(changes.modified[0], PathBuf::from("conflict.rs"));
    }

    #[test]
    fn test_parse_porcelain_merge_conflicts_aa() {
        let output = "AA both_added.rs\0";
        let changes = parse_porcelain(output).unwrap();
        assert_eq!(changes.modified.len(), 1);
        assert_eq!(changes.modified[0], PathBuf::from("both_added.rs"));
    }

    #[test]
    fn test_parse_porcelain_spaces_in_filename() {
        let output = "M  file with spaces.rs\0";
        let changes = parse_porcelain(output).unwrap();
        assert_eq!(changes.modified.len(), 1);
        assert_eq!(changes.modified[0], PathBuf::from("file with spaces.rs"));
    }

    #[test]
    fn test_parse_porcelain_newlines_in_filename() {
        // Null-terminated output handles newlines in filenames
        let output = "M  file\nwith\nnewlines.rs\0";
        let changes = parse_porcelain(output).unwrap();
        assert_eq!(changes.modified.len(), 1);
        assert_eq!(
            changes.modified[0],
            PathBuf::from("file\nwith\nnewlines.rs")
        );
    }

    #[test]
    fn test_parse_porcelain_malformed_too_short() {
        let output = "M\0"; // Missing space and filename
        let result = parse_porcelain(output);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), GitError::InvalidOutput(_)));
    }

    #[test]
    fn test_parse_porcelain_malformed_rename_missing_new() {
        let output = "R  old.rs\0"; // Missing new filename
        let result = parse_porcelain(output);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), GitError::InvalidOutput(_)));
    }

    #[test]
    fn test_parse_porcelain_malformed_empty_filename() {
        // Missing filename after status; should be invalid
        let output = "M  \0";
        let result = parse_porcelain(output);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), GitError::InvalidOutput(_)));
    }

    #[test]
    fn test_parse_diff_empty() {
        let changes = parse_diff_name_status("").unwrap();
        assert!(changes.is_empty());
    }

    #[test]
    fn test_parse_diff_added() {
        let output = "A\0new.rs\0";
        let changes = parse_diff_name_status(output).unwrap();
        assert_eq!(changes.added.len(), 1);
        assert_eq!(changes.added[0], PathBuf::from("new.rs"));
    }

    #[test]
    fn test_parse_diff_modified() {
        let output = "M\0file.rs\0";
        let changes = parse_diff_name_status(output).unwrap();
        assert_eq!(changes.modified.len(), 1);
        assert_eq!(changes.modified[0], PathBuf::from("file.rs"));
    }

    #[test]
    fn test_parse_diff_deleted() {
        let output = "D\0old.rs\0";
        let changes = parse_diff_name_status(output).unwrap();
        assert_eq!(changes.deleted.len(), 1);
        assert_eq!(changes.deleted[0], PathBuf::from("old.rs"));
    }

    #[test]
    fn test_parse_diff_renamed() {
        let output = "R075\0old.rs\0new.rs\0";
        let changes = parse_diff_name_status(output).unwrap();
        assert_eq!(changes.renamed.len(), 1);
        assert_eq!(changes.renamed[0].0, PathBuf::from("old.rs"));
        assert_eq!(changes.renamed[0].1, PathBuf::from("new.rs"));
    }

    #[test]
    fn test_parse_diff_complex() {
        let output = "A\0added.rs\0M\0modified.rs\0D\0deleted.rs\0R050\0old.rs\0new.rs\0";
        let changes = parse_diff_name_status(output).unwrap();
        assert_eq!(changes.added.len(), 1);
        assert_eq!(changes.modified.len(), 1);
        assert_eq!(changes.deleted.len(), 1);
        assert_eq!(changes.renamed.len(), 1);
    }

    #[test]
    fn test_parse_diff_invalid_status() {
        let output = "Z\0file.rs\0"; // Invalid status code
        let result = parse_diff_name_status(output);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), GitError::InvalidOutput(_)));
    }

    #[test]
    fn test_parse_diff_malformed_missing_filename() {
        let output = "A\0"; // Missing filename
        let result = parse_diff_name_status(output);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), GitError::InvalidOutput(_)));
    }

    #[test]
    fn test_parse_diff_malformed_rename_missing_new() {
        let output = "R075\0old.rs\0"; // Missing new filename
        let result = parse_diff_name_status(output);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), GitError::InvalidOutput(_)));
    }

    #[test]
    fn test_parse_diff_malformed_empty_filename() {
        let output = "A\0\0"; // Added but empty filename
        let result = parse_diff_name_status(output);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), GitError::InvalidOutput(_)));
    }
}
