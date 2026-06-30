//! Virtual source entries for revision-aware graph builds.
//!
//! Raw Git revisions and dirty snapshots both need a source abstraction that is
//! not a live checkout. This module records repository-relative paths as raw
//! Git path bytes and provides a guarded materialization step for the existing
//! filesystem-based graph builder.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::error::DaemonError;

use super::PathScope;

/// Default upper bound for source files materialized into the parser pipeline.
pub const DEFAULT_RAW_SOURCE_MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// Repository-relative Git path bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VirtualPath {
    bytes: Vec<u8>,
}

impl VirtualPath {
    /// Construct a guarded repository-relative path from Git path bytes.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::RevisionSourceUnavailable`] for empty paths,
    /// absolute paths, parent-directory components, NUL bytes, or empty path
    /// components.
    pub fn from_git_path_bytes(bytes: Vec<u8>) -> Result<Self, DaemonError> {
        validate_git_path_bytes(&bytes)?;
        Ok(Self { bytes })
    }

    /// Raw slash-separated Git path bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Lossy path text for logs and JSON error details.
    #[must_use]
    pub fn display_lossy(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }

    /// Convert to a host path relative to `root`.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::RevisionSourceUnavailable`] if a non-Unix host
    /// receives a non-UTF-8 Git path.
    pub fn to_path_buf_under(&self, root: &Path) -> Result<PathBuf, DaemonError> {
        let mut out = root.to_path_buf();
        for component in self.bytes.split(|byte| *byte == b'/') {
            #[cfg(unix)]
            push_git_component(&mut out, component);

            #[cfg(not(unix))]
            push_git_component(&mut out, component, self)?;
        }
        Ok(out)
    }

    /// Whether this path is inside the artifact path scope.
    #[must_use]
    pub fn is_in_scope(&self, scope: &PathScope) -> bool {
        match scope {
            PathScope::Repository => true,
            PathScope::Paths { paths } => paths.iter().any(|path| {
                let normalized = path.trim_matches('/');
                if normalized.is_empty() {
                    return true;
                }
                let prefix = normalized.as_bytes();
                self.bytes == prefix
                    || self
                        .bytes
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.first() == Some(&b'/'))
            }),
        }
    }
}

/// State represented by a virtual source entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtualSourceKind {
    /// Normal blob file.
    RegularFile,
    /// Executable blob file.
    ExecutableFile,
    /// Symlink blob. The blob bytes are the link target, not followed content.
    Symlink,
    /// Git submodule entry.
    Gitlink {
        /// Commit object id recorded by the gitlink.
        oid: String,
    },
    /// Deletion marker for dirty snapshot sources.
    Deletion,
    /// Source file that was intentionally excluded before parsing due to size.
    TooLarge {
        /// Blob size reported by Git.
        size_bytes: u64,
        /// Configured maximum parser input size.
        max_bytes: u64,
    },
    /// Unsupported object or mode.
    Unsupported {
        /// Human-readable reason.
        reason: String,
    },
}

impl VirtualSourceKind {
    /// Whether the entry should be materialized into the parser input tree.
    #[must_use]
    pub fn is_regular_parse_candidate(&self) -> bool {
        matches!(self, Self::RegularFile | Self::ExecutableFile)
    }
}

/// One repository-relative virtual source entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualSourceEntry {
    /// Repository-relative path.
    pub path: VirtualPath,
    /// Entry kind.
    pub kind: VirtualSourceKind,
    /// Git object id for blob-backed entries.
    pub object_id: Option<String>,
    /// Blob size when known.
    pub size_bytes: Option<u64>,
}

impl VirtualSourceEntry {
    /// Construct a virtual source entry.
    #[must_use]
    pub fn new(
        path: VirtualPath,
        kind: VirtualSourceKind,
        object_id: Option<String>,
        size_bytes: Option<u64>,
    ) -> Self {
        Self {
            path,
            kind,
            object_id,
            size_bytes,
        }
    }
}

/// Source that can list virtual files and read raw bytes for parse candidates.
pub trait VirtualSourceReader {
    /// Stable list of virtual source entries.
    fn entries(&self) -> &[VirtualSourceEntry];

    /// Read bytes for a regular parse candidate.
    ///
    /// # Errors
    ///
    /// Returns a revision-specific [`DaemonError`] when the backing source is
    /// unavailable, too large, a gitlink, or missing locally.
    fn read_entry_bytes(&self, entry: &VirtualSourceEntry) -> Result<Vec<u8>, DaemonError>;
}

/// Temporary filesystem tree containing materialized regular virtual files.
#[derive(Debug)]
pub struct MaterializedVirtualSource {
    _temp_dir: TempDir,
    root: PathBuf,
    materialized_files: Vec<PathBuf>,
    skipped_entries: Vec<VirtualSourceEntry>,
}

impl MaterializedVirtualSource {
    /// Root directory passed to the existing graph builder.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Files written into the temp tree.
    #[must_use]
    pub fn materialized_files(&self) -> &[PathBuf] {
        &self.materialized_files
    }

    /// Entries intentionally skipped before parsing.
    #[must_use]
    pub fn skipped_entries(&self) -> &[VirtualSourceEntry] {
        &self.skipped_entries
    }
}

/// Materialize regular virtual files into a temp directory for parsing.
///
/// Symlinks, gitlinks, deletions, too-large files, and unsupported states stay
/// represented in `skipped_entries`; they are not followed or converted into
/// checkout bytes.
///
/// # Errors
///
/// Returns [`DaemonError`] if tempdir creation, path conversion, directory
/// creation, file write, or source byte reads fail.
pub fn materialize_virtual_source(
    source: &dyn VirtualSourceReader,
) -> Result<MaterializedVirtualSource, DaemonError> {
    let temp_dir = tempfile::Builder::new()
        .prefix("sqry-raw-git-source-")
        .tempdir()
        .map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to create raw source tempdir: {err}"),
            path: None,
        })?;
    let root = temp_dir.path().to_path_buf();
    let mut materialized_files = Vec::new();
    let mut skipped_entries = Vec::new();

    for entry in source.entries() {
        if !entry.kind.is_regular_parse_candidate() {
            skipped_entries.push(entry.clone());
            continue;
        }

        let bytes = source.read_entry_bytes(entry)?;
        let target = entry.path.to_path_buf_under(&root)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|err| DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to create raw source directory: {err}"),
                path: Some(parent.to_path_buf()),
            })?;
        }
        fs::write(&target, bytes).map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to materialize raw source file: {err}"),
            path: Some(target.clone()),
        })?;
        materialized_files.push(target);
    }

    Ok(MaterializedVirtualSource {
        _temp_dir: temp_dir,
        root,
        materialized_files,
        skipped_entries,
    })
}

fn validate_git_path_bytes(bytes: &[u8]) -> Result<(), DaemonError> {
    if bytes.is_empty()
        || bytes.first() == Some(&b'/')
        || bytes.contains(&0)
        || bytes
            .split(|byte| *byte == b'/')
            .any(|component| component.is_empty() || component == b"." || component == b"..")
    {
        return Err(DaemonError::RevisionSourceUnavailable {
            reason: "invalid repository-relative Git path".to_owned(),
            path: Some(PathBuf::from(String::from_utf8_lossy(bytes).into_owned())),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn push_git_component(out: &mut PathBuf, component: &[u8]) {
    use std::os::unix::ffi::OsStringExt as _;

    out.push(OsString::from_vec(component.to_vec()));
}

#[cfg(not(unix))]
fn push_git_component(
    out: &mut PathBuf,
    component: &[u8],
    virtual_path: &VirtualPath,
) -> Result<(), DaemonError> {
    let value =
        std::str::from_utf8(component).map_err(|_| DaemonError::RevisionSourceUnavailable {
            reason: "non-UTF-8 Git paths are unsupported on this platform".to_owned(),
            path: Some(PathBuf::from(virtual_path.display_lossy())),
        })?;
    out.push(OsString::from(value));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FakeSource {
        entries: Vec<VirtualSourceEntry>,
    }

    impl VirtualSourceReader for FakeSource {
        fn entries(&self) -> &[VirtualSourceEntry] {
            &self.entries
        }

        fn read_entry_bytes(&self, entry: &VirtualSourceEntry) -> Result<Vec<u8>, DaemonError> {
            Ok(format!("bytes for {}", entry.path.display_lossy()).into_bytes())
        }
    }

    fn entry(path: &str, kind: VirtualSourceKind) -> VirtualSourceEntry {
        VirtualSourceEntry::new(
            VirtualPath::from_git_path_bytes(path.as_bytes().to_vec()).unwrap(),
            kind,
            Some("a".repeat(40)),
            Some(1),
        )
    }

    #[test]
    fn virtual_path_rejects_escape_components() {
        assert!(VirtualPath::from_git_path_bytes(b"../x.rs".to_vec()).is_err());
        assert!(VirtualPath::from_git_path_bytes(b"/x.rs".to_vec()).is_err());
        assert!(VirtualPath::from_git_path_bytes(b"a//x.rs".to_vec()).is_err());
    }

    #[test]
    fn path_scope_matches_exact_paths_and_descendants() {
        let path = VirtualPath::from_git_path_bytes(b"src/lib.rs".to_vec()).unwrap();
        assert!(path.is_in_scope(&PathScope::Paths {
            paths: vec!["src".to_owned()]
        }));
        assert!(path.is_in_scope(&PathScope::Paths {
            paths: vec!["src/lib.rs".to_owned()]
        }));
        assert!(!path.is_in_scope(&PathScope::Paths {
            paths: vec!["tests".to_owned()]
        }));
    }

    #[test]
    fn materialization_writes_only_regular_parse_candidates() {
        let source = FakeSource {
            entries: vec![
                entry("src/lib.rs", VirtualSourceKind::RegularFile),
                entry("script.sh", VirtualSourceKind::ExecutableFile),
                entry("link", VirtualSourceKind::Symlink),
                entry(
                    "vendor/sub",
                    VirtualSourceKind::Gitlink {
                        oid: "b".repeat(40),
                    },
                ),
            ],
        };

        let materialized = materialize_virtual_source(&source).unwrap();
        assert_eq!(materialized.materialized_files().len(), 2);
        assert_eq!(materialized.skipped_entries().len(), 2);
        assert!(materialized.root().join("src/lib.rs").is_file());
        assert!(materialized.root().join("script.sh").is_file());
        assert!(!materialized.root().join("link").exists());
    }
}
