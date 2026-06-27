//! Raw Git object source reader for immutable revision indexing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::error::DaemonError;

use super::artifact_key::PathScope;
use super::virtual_source::{
    DEFAULT_RAW_SOURCE_MAX_FILE_BYTES, VirtualPath, VirtualSourceEntry, VirtualSourceKind,
    VirtualSourceReader,
};

/// Options for opening a raw Git tree source.
#[derive(Debug, Clone)]
pub struct RawGitSourceOptions {
    /// Repository root passed to `git -C`.
    pub repo_root: PathBuf,
    /// Tree object id to traverse.
    pub tree_oid: String,
    /// Path scope to include.
    pub path_scope: PathScope,
    /// Maximum regular blob size allowed into the parser input set.
    pub max_file_size_bytes: u64,
}

impl RawGitSourceOptions {
    /// Construct options for a whole-tree raw source.
    #[must_use]
    pub fn new(repo_root: impl Into<PathBuf>, tree_oid: impl Into<String>) -> Self {
        Self {
            repo_root: repo_root.into(),
            tree_oid: tree_oid.into(),
            path_scope: PathScope::Repository,
            max_file_size_bytes: DEFAULT_RAW_SOURCE_MAX_FILE_BYTES,
        }
    }
}

/// Raw immutable source over a Git tree object.
#[derive(Debug, Clone)]
pub struct RawGitSource {
    options: RawGitSourceOptions,
    entries: Vec<VirtualSourceEntry>,
}

impl RawGitSource {
    /// Open and list a raw Git tree source.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::RevisionObjectMissing`] when Git cannot resolve
    /// the tree object locally. No implicit fetch is attempted.
    pub fn open(options: RawGitSourceOptions) -> Result<Self, DaemonError> {
        let output = git_command(&options.repo_root)
            .args(["ls-tree", "-r", "-z", "-l", &options.tree_oid])
            .output()
            .map_err(|err| DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to execute git ls-tree: {err}"),
                path: Some(options.repo_root.clone()),
            })?;
        if !output.status.success() {
            return Err(missing_object_error(
                &options.tree_oid,
                None,
                "git ls-tree failed",
                &output,
            ));
        }

        let mut entries = parse_ls_tree_output(
            &output.stdout,
            &options.path_scope,
            options.max_file_size_bytes,
        )?;
        entries.sort_by(|left, right| left.path.cmp(&right.path));

        Ok(Self { options, entries })
    }

    /// Repository root backing this raw source.
    #[must_use]
    pub fn repo_root(&self) -> &Path {
        &self.options.repo_root
    }

    /// Tree object id backing this raw source.
    #[must_use]
    pub fn tree_oid(&self) -> &str {
        &self.options.tree_oid
    }

    /// Configured path scope.
    #[must_use]
    pub fn path_scope(&self) -> &PathScope {
        &self.options.path_scope
    }

    /// Configured parser file-size guard.
    #[must_use]
    pub fn max_file_size_bytes(&self) -> u64 {
        self.options.max_file_size_bytes
    }
}

impl VirtualSourceReader for RawGitSource {
    fn entries(&self) -> &[VirtualSourceEntry] {
        &self.entries
    }

    fn read_entry_bytes(&self, entry: &VirtualSourceEntry) -> Result<Vec<u8>, DaemonError> {
        match &entry.kind {
            VirtualSourceKind::RegularFile
            | VirtualSourceKind::ExecutableFile
            | VirtualSourceKind::Symlink => {}
            VirtualSourceKind::Gitlink { oid } => {
                return Err(DaemonError::SubmoduleUnavailable {
                    path: entry.path.to_path_buf_under(Path::new(""))?,
                    gitlink_oid: Some(oid.clone()),
                });
            }
            VirtualSourceKind::Deletion => {
                return Err(DaemonError::RevisionSourceUnavailable {
                    reason: "deletion entries do not have source bytes".to_owned(),
                    path: Some(entry.path.to_path_buf_under(Path::new(""))?),
                });
            }
            VirtualSourceKind::TooLarge {
                size_bytes,
                max_bytes,
            } => {
                return Err(DaemonError::RevisionSourceUnavailable {
                    reason: format!("blob is {size_bytes} bytes, exceeds limit {max_bytes}"),
                    path: Some(entry.path.to_path_buf_under(Path::new(""))?),
                });
            }
            VirtualSourceKind::Unsupported { reason } => {
                return Err(DaemonError::RevisionSourceUnavailable {
                    reason: reason.clone(),
                    path: Some(entry.path.to_path_buf_under(Path::new(""))?),
                });
            }
        }

        let Some(object_id) = &entry.object_id else {
            return Err(DaemonError::RevisionSourceUnavailable {
                reason: "blob entry did not include an object id".to_owned(),
                path: Some(entry.path.to_path_buf_under(Path::new(""))?),
            });
        };
        let output = git_command(&self.options.repo_root)
            .args(["cat-file", "blob", object_id])
            .output()
            .map_err(|err| DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to execute git cat-file: {err}"),
                path: Some(entry_path_for_error(&entry.path)),
            })?;
        if !output.status.success() {
            return Err(missing_object_error(
                object_id,
                Some(&entry.path),
                "git cat-file failed",
                &output,
            ));
        }
        Ok(output.stdout)
    }
}

fn parse_ls_tree_output(
    stdout: &[u8],
    path_scope: &PathScope,
    max_file_size_bytes: u64,
) -> Result<Vec<VirtualSourceEntry>, DaemonError> {
    let mut entries = Vec::new();
    for raw_record in stdout.split(|byte| *byte == 0) {
        if raw_record.is_empty() {
            continue;
        }
        let Some(tab_index) = raw_record.iter().position(|byte| *byte == b'\t') else {
            return Err(DaemonError::RevisionSourceUnavailable {
                reason: "git ls-tree record missing path separator".to_owned(),
                path: None,
            });
        };
        let header = &raw_record[..tab_index];
        let path_bytes = raw_record[tab_index + 1..].to_vec();
        let path = VirtualPath::from_git_path_bytes(path_bytes)?;
        if !path.is_in_scope(path_scope) {
            continue;
        }

        let fields = header
            .split(|byte| *byte == b' ')
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();
        if fields.len() != 4 {
            return Err(DaemonError::RevisionSourceUnavailable {
                reason: "git ls-tree record had unexpected header shape".to_owned(),
                path: Some(path.to_path_buf_under(Path::new(""))?),
            });
        }
        let mode = bytes_to_ascii(fields[0], "mode", &path)?;
        let object_type = bytes_to_ascii(fields[1], "object type", &path)?;
        let object_id = bytes_to_ascii(fields[2], "object id", &path)?;
        let size = parse_ls_tree_size(fields[3], &path)?;
        let kind = classify_entry(mode, object_type, object_id, size, max_file_size_bytes);

        entries.push(VirtualSourceEntry::new(
            path,
            kind,
            Some(object_id.to_owned()),
            size,
        ));
    }
    Ok(entries)
}

fn classify_entry(
    mode: &str,
    object_type: &str,
    object_id: &str,
    size: Option<u64>,
    max_file_size_bytes: u64,
) -> VirtualSourceKind {
    match (mode, object_type) {
        ("100644", "blob") => file_kind_with_size_guard(size, max_file_size_bytes, false),
        ("100755", "blob") => file_kind_with_size_guard(size, max_file_size_bytes, true),
        ("120000", "blob") => VirtualSourceKind::Symlink,
        ("160000", "commit") => VirtualSourceKind::Gitlink {
            oid: object_id.to_owned(),
        },
        _ => VirtualSourceKind::Unsupported {
            reason: format!("unsupported Git tree entry mode {mode} type {object_type}"),
        },
    }
}

fn file_kind_with_size_guard(
    size: Option<u64>,
    max_file_size_bytes: u64,
    executable: bool,
) -> VirtualSourceKind {
    if let Some(size_bytes) = size
        && size_bytes > max_file_size_bytes
    {
        return VirtualSourceKind::TooLarge {
            size_bytes,
            max_bytes: max_file_size_bytes,
        };
    }
    if executable {
        VirtualSourceKind::ExecutableFile
    } else {
        VirtualSourceKind::RegularFile
    }
}

fn parse_ls_tree_size(size: &[u8], path: &VirtualPath) -> Result<Option<u64>, DaemonError> {
    if size == b"-" {
        return Ok(None);
    }
    let text = bytes_to_ascii(size, "size", path)?;
    text.parse::<u64>()
        .map(Some)
        .map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("invalid git ls-tree size: {err}"),
            path: Some(
                path.to_path_buf_under(Path::new(""))
                    .unwrap_or_else(|_| PathBuf::from(path.display_lossy())),
            ),
        })
}

fn bytes_to_ascii<'a>(
    bytes: &'a [u8],
    field: &str,
    path: &VirtualPath,
) -> Result<&'a str, DaemonError> {
    std::str::from_utf8(bytes).map_err(|err| DaemonError::RevisionSourceUnavailable {
        reason: format!("git ls-tree {field} was not UTF-8/ASCII: {err}"),
        path: Some(
            path.to_path_buf_under(Path::new(""))
                .unwrap_or_else(|_| PathBuf::from(path.display_lossy())),
        ),
    })
}

fn git_command(repo_root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo_root)
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    command
}

fn missing_object_error(
    object: &str,
    path: Option<&VirtualPath>,
    context: &str,
    output: &Output,
) -> DaemonError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = if stderr.trim().is_empty() {
        context.to_owned()
    } else {
        format!("{context}: {}", stderr.trim())
    };
    tracing::debug!("{reason}");
    DaemonError::RevisionObjectMissing {
        object: object.to_owned(),
        path: path.and_then(|value| value.to_path_buf_under(Path::new("")).ok()),
    }
}

fn entry_path_for_error(path: &VirtualPath) -> PathBuf {
    path.to_path_buf_under(Path::new(""))
        .unwrap_or_else(|_| PathBuf::from(path.display_lossy()))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt as _;

    use tempfile::TempDir;

    use super::*;

    fn init_repo() -> TempDir {
        let temp_dir = TempDir::new().expect("tempdir");
        git(temp_dir.path(), ["init"]).expect("git init");
        temp_dir
    }

    fn git<const N: usize>(repo: &Path, args: [&str; N]) -> Result<String, String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .map_err(|err| err.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).into_owned())
        }
    }

    fn write_blob(repo: &Path, bytes: &[u8]) -> String {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["hash-object", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .spawn()
            .expect("spawn hash-object");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(bytes)
            .expect("write blob stdin");
        let output = child.wait_with_output().expect("hash-object output");
        assert!(
            output.status.success(),
            "hash-object failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn cacheinfo(repo: &Path, mode: &str, oid: &str, path: &str) {
        git(
            repo,
            ["update-index", "--add", "--cacheinfo", mode, oid, path],
        )
        .expect("update-index cacheinfo");
    }

    fn write_tree(repo: &Path) -> String {
        git(repo, ["write-tree"]).expect("write-tree")
    }

    #[test]
    fn raw_source_lists_regular_symlink_executable_and_gitlink_entries() {
        let repo = init_repo();
        let regular = write_blob(repo.path(), b"fn main() {}\n");
        let executable = write_blob(repo.path(), b"#!/bin/sh\n");
        let link_target = write_blob(repo.path(), b"src/main.rs");

        cacheinfo(repo.path(), "100644", &regular, "src/main.rs");
        cacheinfo(repo.path(), "100755", &executable, "script.sh");
        cacheinfo(repo.path(), "120000", &link_target, "main-link");
        cacheinfo(
            repo.path(),
            "160000",
            "1234567890123456789012345678901234567890",
            "deps/submodule",
        );
        let tree = write_tree(repo.path());

        let source = RawGitSource::open(RawGitSourceOptions::new(repo.path(), tree)).unwrap();
        let entries = source.entries();

        assert!(
            entries
                .iter()
                .any(|entry| matches!(entry.kind, VirtualSourceKind::RegularFile)
                    && entry.path.display_lossy() == "src/main.rs")
        );
        assert!(entries.iter().any(|entry| matches!(
            entry.kind,
            VirtualSourceKind::ExecutableFile
        ) && entry.path.display_lossy() == "script.sh"));
        assert!(
            entries
                .iter()
                .any(|entry| matches!(entry.kind, VirtualSourceKind::Symlink)
                    && entry.path.display_lossy() == "main-link")
        );
        assert!(entries.iter().any(|entry| matches!(
            entry.kind,
            VirtualSourceKind::Gitlink { .. }
        ) && entry.path.display_lossy() == "deps/submodule"));
    }

    #[test]
    fn raw_source_reads_blob_bytes_without_checkout_filters() {
        let repo = init_repo();
        let blob = write_blob(repo.path(), b"first\r\nsecond\r\n");
        cacheinfo(repo.path(), "100644", &blob, "src/lib.rs");
        let tree = write_tree(repo.path());

        let source = RawGitSource::open(RawGitSourceOptions::new(repo.path(), tree)).unwrap();
        let entry = source
            .entries()
            .iter()
            .find(|entry| entry.path.display_lossy() == "src/lib.rs")
            .unwrap();

        assert_eq!(
            source.read_entry_bytes(entry).unwrap(),
            b"first\r\nsecond\r\n"
        );
    }

    #[test]
    fn raw_source_enforces_partial_scope_and_size_guard_before_parsing() {
        let repo = init_repo();
        let small = write_blob(repo.path(), b"ok");
        let large = write_blob(repo.path(), b"too-large");
        let readme = write_blob(repo.path(), b"readme");
        cacheinfo(repo.path(), "100644", &small, "src/lib.rs");
        cacheinfo(repo.path(), "100644", &large, "src/large.rs");
        cacheinfo(repo.path(), "100644", &readme, "README.md");
        let tree = write_tree(repo.path());

        let mut options = RawGitSourceOptions::new(repo.path(), tree);
        options.path_scope = PathScope::Paths {
            paths: vec!["src".to_owned()],
        };
        options.max_file_size_bytes = 3;
        let source = RawGitSource::open(options).unwrap();

        assert_eq!(source.entries().len(), 2);
        assert!(source.entries().iter().any(|entry| {
            entry.path.display_lossy() == "src/large.rs"
                && matches!(entry.kind, VirtualSourceKind::TooLarge { .. })
        }));
        assert!(
            !source
                .entries()
                .iter()
                .any(|entry| entry.path.display_lossy() == "README.md")
        );
    }

    #[test]
    fn raw_source_missing_tree_returns_revision_object_missing() {
        let repo = init_repo();
        let err = RawGitSource::open(RawGitSourceOptions::new(repo.path(), "f".repeat(40)))
            .expect_err("missing tree should fail");

        assert!(matches!(
            err,
            DaemonError::RevisionObjectMissing { object, .. } if object == "f".repeat(40)
        ));
    }

    #[test]
    fn gitlink_read_returns_submodule_unavailable() {
        let repo = init_repo();
        cacheinfo(
            repo.path(),
            "160000",
            "1234567890123456789012345678901234567890",
            "deps/submodule",
        );
        let tree = write_tree(repo.path());
        let source = RawGitSource::open(RawGitSourceOptions::new(repo.path(), tree)).unwrap();
        let entry = source.entries().first().unwrap();

        assert!(matches!(
            source.read_entry_bytes(entry),
            Err(DaemonError::SubmoduleUnavailable { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn raw_source_preserves_unusual_path_bytes_from_nul_safe_tree_output() {
        let repo = init_repo();
        let path = PathBuf::from(OsString::from_vec(b"strange-\xff.rs".to_vec()));
        std::fs::write(repo.path().join(&path), b"fn main() {}\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .arg("add")
            .arg(&path)
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .expect("git add")
            .success()
            .then_some(())
            .expect("git add succeeded");
        let tree = write_tree(repo.path());

        let source = RawGitSource::open(RawGitSourceOptions::new(repo.path(), tree)).unwrap();

        assert!(
            source
                .entries()
                .iter()
                .any(|entry| entry.path.as_bytes() == b"strange-\xff.rs")
        );
    }
}
