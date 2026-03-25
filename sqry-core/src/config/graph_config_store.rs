//! Graph config store - unified config partition under `.sqry/graph/config/`
//!
//! Implements Step 1 of the Unified Graph Config Partition feature:
//! - Path resolution for config files and directories
//! - Network filesystem detection
//! - Foundation for atomic config operations
//!
//! # Storage Layout
//!
//! Under `<project_root>/.sqry/graph/`:
//! - `config/config.json` - canonical config file
//! - `config/config.json.previous` - last known-good config
//! - `config/config.json.corrupt.<timestamp>` - quarantined corrupt files
//! - `config/config.lock` - advisory lock file
//!
//! # Design
//!
//! See: `docs/development/unified-graph-config-partition/02_DESIGN.md`

use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors that can occur with graph config operations
#[derive(Debug, Error)]
pub enum GraphConfigError {
    /// Config directory not initialized
    #[error("Config directory not found at {0}. Run `sqry config init` to create it.")]
    NotInitialized(PathBuf),

    /// Network filesystem detected
    #[error(
        "Network filesystem detected at {0}. Config operations may be unreliable. Set config.durability.allow_network_filesystems=true to proceed."
    )]
    NetworkFilesystem(PathBuf),

    /// IO error
    #[error("IO error at {0}: {1}")]
    IoError(PathBuf, #[source] std::io::Error),

    /// Invalid path
    #[error("Invalid path: {0}")]
    InvalidPath(String),
}

#[cfg(target_os = "linux")]
const NFS_SUPER_MAGIC: i128 = 0x6969;
#[cfg(target_os = "linux")]
const SMB_SUPER_MAGIC: i128 = 0x517B;
#[cfg(target_os = "linux")]
const CIFS_MAGIC_NUMBER: i128 = 0xFF53_4D42;
#[cfg(target_os = "linux")]
const AFS_SUPER_MAGIC: i128 = 0x5346_414F;
#[cfg(target_os = "linux")]
const CODA_SUPER_MAGIC: i128 = 0x7375_7245;

/// Result type for graph config operations
pub type Result<T> = std::result::Result<T, GraphConfigError>;

/// Path resolver for graph config files
///
/// Provides canonical paths for all config-related files and directories
/// under `.sqry/graph/config/`.
///
/// # Example
///
/// ```rust,ignore
/// use sqry_core::config::GraphConfigPaths;
///
/// let paths = GraphConfigPaths::new("/home/user/project")?;
/// let config_file = paths.config_file();
/// let lock_file = paths.lock_file();
/// ```
#[derive(Debug, Clone)]
pub struct GraphConfigPaths {
    /// Project root directory
    project_root: PathBuf,
    /// Override for graph directory (for testing)
    graph_dir_override: Option<PathBuf>,
}

impl GraphConfigPaths {
    /// Create a new path resolver from a project root
    ///
    /// # Arguments
    ///
    /// * `project_root` - Root directory of the project
    ///
    /// # Errors
    ///
    /// Returns error if the path is invalid or inaccessible
    pub fn new<P: AsRef<Path>>(project_root: P) -> Result<Self> {
        let project_root = project_root.as_ref();

        // Validate path exists and is a directory
        if !project_root.exists() {
            return Err(GraphConfigError::InvalidPath(format!(
                "Project root does not exist: {}",
                project_root.display()
            )));
        }

        if !project_root.is_dir() {
            return Err(GraphConfigError::InvalidPath(format!(
                "Project root is not a directory: {}",
                project_root.display()
            )));
        }

        Ok(Self {
            project_root: project_root.to_path_buf(),
            graph_dir_override: None,
        })
    }

    /// Create path resolver with an explicit graph directory override
    ///
    /// This is primarily for testing or when the graph directory is in
    /// a non-standard location.
    ///
    /// # Arguments
    ///
    /// * `project_root` - Root directory of the project
    /// * `graph_dir` - Override path for `.sqry/graph` directory
    ///
    /// # Errors
    ///
    /// Returns an error if the project root is invalid or inaccessible.
    pub fn with_graph_dir<P: AsRef<Path>, G: AsRef<Path>>(
        project_root: P,
        graph_dir: G,
    ) -> Result<Self> {
        let mut paths = Self::new(project_root)?;
        paths.graph_dir_override = Some(graph_dir.as_ref().to_path_buf());
        Ok(paths)
    }

    /// Get the graph directory path (`.sqry/graph`)
    #[must_use]
    pub fn graph_dir(&self) -> PathBuf {
        self.graph_dir_override
            .clone()
            .unwrap_or_else(|| self.project_root.join(".sqry").join("graph"))
    }

    /// Get the config directory path (`.sqry/graph/config`)
    #[must_use]
    pub fn config_dir(&self) -> PathBuf {
        self.graph_dir().join("config")
    }

    /// Get the canonical config file path (`.sqry/graph/config/config.json`)
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_dir().join("config.json")
    }

    /// Get the previous config file path (`.sqry/graph/config/config.json.previous`)
    #[must_use]
    pub fn previous_file(&self) -> PathBuf {
        self.config_dir().join("config.json.previous")
    }

    /// Get the lock file path (`.sqry/graph/config/config.lock`)
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.config_dir().join("config.lock")
    }

    /// Generate a corrupt quarantine file path with timestamp
    ///
    /// Format: `.sqry/graph/config/config.json.corrupt.<timestamp>`
    ///
    /// # Arguments
    ///
    /// * `timestamp` - UTC timestamp in RFC3339 format
    #[must_use]
    pub fn corrupt_file(&self, timestamp: &str) -> PathBuf {
        self.config_dir()
            .join(format!("config.json.corrupt.{timestamp}"))
    }

    /// Check if the config directory exists
    ///
    /// Returns `true` if `.sqry/graph/config/` exists and is a directory.
    #[must_use]
    pub fn config_dir_exists(&self) -> bool {
        let config_dir = self.config_dir();
        config_dir.exists() && config_dir.is_dir()
    }

    /// Check if the config file exists
    ///
    /// Returns `true` if `.sqry/graph/config/config.json` exists.
    #[must_use]
    pub fn config_file_exists(&self) -> bool {
        self.config_file().exists()
    }

    /// Detect if the path is on a network filesystem
    ///
    /// Network filesystems (NFS, CIFS, SMB) can violate atomic rename and
    /// fsync durability assumptions. This detection emits warnings to help
    /// users avoid data loss scenarios.
    ///
    /// # Platform Support
    ///
    /// - Linux: Uses `statfs` with `f_type` magic numbers (NFS, SMB, CIFS, AFS, CODA)
    /// - macOS: Uses `statfs` with `f_fstypename` string (nfs, smbfs, afpfs, webdav, ftp)
    /// - Windows: UNC path detection + `GetDriveTypeW` for mapped network drives
    /// - Other platforms: Returns `Ok(false)` (assumes local)
    ///
    /// # Returns
    ///
    /// Returns `Ok(true)` if network filesystem detected, `Ok(false)` otherwise.
    /// On Linux, returns an error if the path doesn't exist and no ancestor can be found.
    /// On macOS and Windows, returns `Ok(false)` on syscall/API failure (safe default).
    ///
    /// # Errors
    ///
    /// Returns an error on Linux if filesystem inspection fails and no ancestor
    /// path exists. macOS and Windows implementations prefer `Ok(false)` over errors.
    pub fn is_network_filesystem(&self) -> Result<bool> {
        let path = self.graph_dir();

        #[cfg(target_os = "linux")]
        {
            Self::is_network_filesystem_linux(&path)
        }

        #[cfg(target_os = "macos")]
        {
            self.is_network_filesystem_macos(&path)
        }

        #[cfg(windows)]
        {
            self.is_network_filesystem_windows(&path)
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            log::debug!(
                "Network filesystem detection not implemented for this platform. \
                 Assuming local filesystem at {}",
                path.display()
            );
            Ok(false)
        }
    }

    /// Linux-specific network filesystem detection using statfs
    #[cfg(target_os = "linux")]
    fn is_network_filesystem_linux(path: &Path) -> Result<bool> {
        use std::ffi::CString;

        let path_cstr = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|e| GraphConfigError::InvalidPath(e.to_string()))?;

        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };

        let result = unsafe { libc::statfs(path_cstr.as_ptr(), &raw mut stat) };

        if result != 0 {
            let err = std::io::Error::last_os_error();
            // If path doesn't exist yet (ENOENT), walk up to find an existing ancestor
            if err.kind() == std::io::ErrorKind::NotFound {
                let mut current = path.parent();
                while let Some(parent) = current {
                    if parent.exists() {
                        return Self::is_network_filesystem_linux(parent);
                    }
                    current = parent.parent();
                }
            }
            return Err(GraphConfigError::IoError(path.to_path_buf(), err));
        }

        // libc varies here: glibc uses signed word-sized values, musl uses
        // unsigned longs, and some Linux targets use narrower unsigned types.
        // Normalize to a single wide integer before comparing magic numbers.
        let fs_type = i128::from(stat.f_type);
        let is_network = matches!(
            fs_type,
            NFS_SUPER_MAGIC
                | SMB_SUPER_MAGIC
                | CIFS_MAGIC_NUMBER
                | AFS_SUPER_MAGIC
                | CODA_SUPER_MAGIC
        );

        if is_network {
            log::warn!(
                "Network filesystem detected at {} (type: 0x{:X}). \
                 Config operations may be unreliable. Consider using a local filesystem.",
                path.display(),
                fs_type
            );
        }

        Ok(is_network)
    }

    /// macOS-specific network filesystem detection using `statfs` with `f_fstypename`
    ///
    /// Unlike Linux (which uses `f_type` integer magic numbers), macOS `statfs`
    /// provides a human-readable `f_fstypename` char array identifying the filesystem.
    #[cfg(target_os = "macos")]
    fn is_network_filesystem_macos(&self, path: &Path) -> Result<bool> {
        use std::ffi::CString;
        use std::mem::MaybeUninit;

        // Network filesystem type names on macOS (from f_fstypename)
        const NETWORK_FS_TYPES: &[&str] = &[
            "nfs",    // NFS
            "smbfs",  // SMB/CIFS
            "afpfs",  // AFP (legacy)
            "webdav", // WebDAV
            "ftp",    // FTP mounts
        ];

        // Handle non-existent paths by checking ancestors (mirrors Linux behavior)
        let check_path = if path.exists() {
            path.to_path_buf()
        } else {
            path.ancestors()
                .find(|p| p.exists())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("/"))
        };

        let c_path = CString::new(check_path.as_os_str().as_encoded_bytes())
            .map_err(|e| GraphConfigError::InvalidPath(e.to_string()))?;
        let mut stat: MaybeUninit<libc::statfs> = MaybeUninit::uninit();

        let result = unsafe { libc::statfs(c_path.as_ptr(), stat.as_mut_ptr()) };
        if result != 0 {
            // On error, assume local (safe default)
            return Ok(false);
        }

        let stat = unsafe { stat.assume_init() };
        let fs_type = unsafe {
            std::ffi::CStr::from_ptr(stat.f_fstypename.as_ptr())
                .to_string_lossy()
                .to_lowercase()
        };

        let is_network = NETWORK_FS_TYPES.iter().any(|&t| fs_type.contains(t));

        if is_network {
            log::warn!(
                "Network filesystem detected at {} (type: {}). \
                 Config operations may be unreliable. Consider using a local filesystem.",
                path.display(),
                fs_type
            );
        }

        Ok(is_network)
    }

    /// Windows-specific network filesystem detection
    ///
    /// Uses two complementary strategies:
    /// - UNC path detection (`\\server\share`, `\\?\UNC\...`) via `Path::components()`
    /// - Mapped network drive detection (`X:\`) via `GetDriveTypeW` (`DRIVE_REMOTE` = 4)
    #[cfg(windows)]
    fn is_network_filesystem_windows(&self, path: &Path) -> Result<bool> {
        use std::path::{Component, Prefix};

        let first_component = path.components().next();

        if let Some(Component::Prefix(prefix_component)) = first_component {
            match prefix_component.kind() {
                // UNC paths are network shares
                Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _) => {
                    log::warn!(
                        "Network filesystem detected at {} (UNC path). \
                         Config operations may be unreliable. Consider using a local filesystem.",
                        path.display()
                    );
                    return Ok(true);
                }
                // Disk prefixes need GetDriveTypeW check for mapped network drives
                Prefix::Disk(_) | Prefix::VerbatimDisk(_) => {
                    let root = format!("{}\\", prefix_component.as_os_str().to_string_lossy());
                    let wide_path: Vec<u16> =
                        root.encode_utf16().chain(std::iter::once(0)).collect();
                    let drive_type = unsafe {
                        windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(wide_path.as_ptr())
                    };
                    // DRIVE_REMOTE = 4
                    let is_network = drive_type == 4;
                    if is_network {
                        log::warn!(
                            "Network filesystem detected at {} (mapped network drive). \
                             Config operations may be unreliable. Consider using a local filesystem.",
                            path.display()
                        );
                    }
                    return Ok(is_network);
                }
                // Device namespace paths (\\.\...) and verbatim local paths (\\?\...) are local
                Prefix::DeviceNS(_) | Prefix::Verbatim(_) => {
                    return Ok(false);
                }
            }
        }

        // Relative paths or root-only paths: assume local
        Ok(false)
    }

    /// Validate that the config directory is suitable for operations
    ///
    /// Checks:
    /// - Config directory exists (or can be created)
    /// - Not on a network filesystem (unless explicitly allowed)
    ///
    /// # Arguments
    ///
    /// * `allow_network_fs` - If true, skip network filesystem check
    ///
    /// # Errors
    ///
    /// Returns error if validation fails
    pub fn validate(&self, allow_network_fs: bool) -> Result<()> {
        // Check network filesystem if not allowed
        if !allow_network_fs && self.is_network_filesystem()? {
            return Err(GraphConfigError::NetworkFilesystem(self.graph_dir()));
        }

        Ok(())
    }
}

/// Graph config store - main API for config operations
///
/// Provides the high-level API for loading, saving, and managing
/// the unified config partition under `.sqry/graph/config/`.
///
/// # Example
///
/// ```rust,ignore
/// use sqry_core::config::GraphConfigStore;
///
/// let store = GraphConfigStore::new("/home/user/project")?;
/// store.validate(false)?;
/// ```
#[derive(Debug)]
pub struct GraphConfigStore {
    paths: GraphConfigPaths,
}

impl GraphConfigStore {
    /// Create a new config store for the given project root
    ///
    /// # Arguments
    ///
    /// * `project_root` - Root directory of the project
    ///
    /// # Errors
    ///
    /// Returns error if the project root is invalid
    pub fn new<P: AsRef<Path>>(project_root: P) -> Result<Self> {
        Ok(Self {
            paths: GraphConfigPaths::new(project_root)?,
        })
    }

    /// Create a new config store with explicit graph directory override
    ///
    /// # Arguments
    ///
    /// * `project_root` - Root directory of the project
    /// * `graph_dir` - Override path for `.sqry/graph` directory
    ///
    /// # Errors
    ///
    /// Returns an error if the project root is invalid or inaccessible.
    pub fn with_graph_dir<P: AsRef<Path>, G: AsRef<Path>>(
        project_root: P,
        graph_dir: G,
    ) -> Result<Self> {
        Ok(Self {
            paths: GraphConfigPaths::with_graph_dir(project_root, graph_dir)?,
        })
    }

    /// Get the path resolver
    #[must_use]
    pub fn paths(&self) -> &GraphConfigPaths {
        &self.paths
    }

    /// Validate the store is ready for operations
    ///
    /// # Arguments
    ///
    /// * `allow_network_fs` - If true, allow network filesystems (best-effort)
    ///
    /// # Errors
    ///
    /// Returns error if validation fails
    pub fn validate(&self, allow_network_fs: bool) -> Result<()> {
        self.paths.validate(allow_network_fs)
    }

    /// Check if the config directory is initialized
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.paths.config_dir_exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_new_with_valid_path() {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();

        assert_eq!(paths.project_root, temp.path());
    }

    #[test]
    fn test_new_with_nonexistent_path() {
        let result = GraphConfigPaths::new("/nonexistent/path/that/does/not/exist");
        assert!(result.is_err());
        assert!(matches!(result, Err(GraphConfigError::InvalidPath(_))));
    }

    #[test]
    fn test_graph_dir_default() {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();

        let expected = temp.path().join(".sqry").join("graph");
        assert_eq!(paths.graph_dir(), expected);
    }

    #[test]
    fn test_graph_dir_override() {
        let temp = TempDir::new().unwrap();
        let override_dir = temp.path().join("custom-graph");
        std::fs::create_dir_all(&override_dir).unwrap();

        let paths = GraphConfigPaths::with_graph_dir(temp.path(), &override_dir).unwrap();

        assert_eq!(paths.graph_dir(), override_dir);
    }

    #[test]
    fn test_config_dir_path() {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();

        let expected = temp.path().join(".sqry").join("graph").join("config");
        assert_eq!(paths.config_dir(), expected);
    }

    #[test]
    fn test_config_file_path() {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();

        let expected = temp
            .path()
            .join(".sqry")
            .join("graph")
            .join("config")
            .join("config.json");
        assert_eq!(paths.config_file(), expected);
    }

    #[test]
    fn test_previous_file_path() {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();

        let expected = temp
            .path()
            .join(".sqry")
            .join("graph")
            .join("config")
            .join("config.json.previous");
        assert_eq!(paths.previous_file(), expected);
    }

    #[test]
    fn test_lock_file_path() {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();

        let expected = temp
            .path()
            .join(".sqry")
            .join("graph")
            .join("config")
            .join("config.lock");
        assert_eq!(paths.lock_file(), expected);
    }

    #[test]
    fn test_corrupt_file_path() {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();

        let timestamp = "2025-12-15T21:30:00Z";
        let expected = temp
            .path()
            .join(".sqry")
            .join("graph")
            .join("config")
            .join(format!("config.json.corrupt.{timestamp}"));
        assert_eq!(paths.corrupt_file(timestamp), expected);
    }

    #[test]
    fn test_config_dir_exists_false_when_not_created() {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();

        assert!(!paths.config_dir_exists());
    }

    #[test]
    fn test_config_dir_exists_true_when_created() {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();

        // Create the config directory
        std::fs::create_dir_all(paths.config_dir()).unwrap();

        assert!(paths.config_dir_exists());
    }

    #[test]
    fn test_config_file_exists() {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();

        // Initially doesn't exist
        assert!(!paths.config_file_exists());

        // Create config file
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(paths.config_file(), "{}").unwrap();

        assert!(paths.config_file_exists());
    }

    #[test]
    fn test_validate_missing_config_dir_ok_for_init() {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();

        // Validation should pass even if config dir doesn't exist yet
        // (it will be created during init)
        let result = paths.validate(false);
        assert!(result.is_ok());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_is_network_filesystem_on_local() {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();

        // tempfile should be on local filesystem
        let result = paths.is_network_filesystem();
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_is_network_filesystem_with_nonexistent_path() {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();

        // Should check parent directory when path doesn't exist
        let result = paths.is_network_filesystem();
        assert!(result.is_ok());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_is_network_filesystem_local_on_macos() {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();

        // tempfile should be on local filesystem (APFS/HFS+)
        let result = paths.is_network_filesystem();
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_is_network_filesystem_nonexistent_path_macos() {
        let temp = TempDir::new().unwrap();
        // Use a graph dir override pointing to a non-existent subdirectory
        let nonexistent = temp.path().join("does").join("not").join("exist");
        let paths = GraphConfigPaths::with_graph_dir(temp.path(), &nonexistent).unwrap();

        // Ancestor fallback should find the temp directory (local filesystem)
        let result = paths.is_network_filesystem();
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    #[cfg(windows)]
    fn test_is_network_filesystem_local_on_windows() {
        let temp = TempDir::new().unwrap();
        let paths = GraphConfigPaths::new(temp.path()).unwrap();

        // tempfile should be on a local drive
        let result = paths.is_network_filesystem();
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    #[cfg(windows)]
    fn test_is_network_filesystem_unc_path() {
        let temp = TempDir::new().unwrap();
        // Override graph dir with a UNC-style path
        let unc_path = PathBuf::from(r"\\server\share\project\.sqry\graph");
        let paths = GraphConfigPaths {
            project_root: temp.path().to_path_buf(),
            graph_dir_override: Some(unc_path),
        };

        // UNC paths should be detected as network filesystems
        let result = paths.is_network_filesystem();
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_store_new() {
        let temp = TempDir::new().unwrap();
        let store = GraphConfigStore::new(temp.path()).unwrap();

        assert_eq!(store.paths.project_root, temp.path());
    }

    #[test]
    fn test_store_with_graph_dir() {
        let temp = TempDir::new().unwrap();
        let override_dir = temp.path().join("custom");
        std::fs::create_dir_all(&override_dir).unwrap();

        let store = GraphConfigStore::with_graph_dir(temp.path(), &override_dir).unwrap();

        assert_eq!(store.paths.graph_dir(), override_dir);
    }

    #[test]
    fn test_store_is_initialized() {
        let temp = TempDir::new().unwrap();
        let store = GraphConfigStore::new(temp.path()).unwrap();

        // Initially not initialized
        assert!(!store.is_initialized());

        // Create config directory
        std::fs::create_dir_all(store.paths.config_dir()).unwrap();

        assert!(store.is_initialized());
    }

    #[test]
    fn test_store_validate() {
        let temp = TempDir::new().unwrap();
        let store = GraphConfigStore::new(temp.path()).unwrap();

        // Validation should pass for local filesystem
        let result = store.validate(false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_paths_accessor() {
        let temp = TempDir::new().unwrap();
        let store = GraphConfigStore::new(temp.path()).unwrap();

        let paths = store.paths();
        assert_eq!(paths.project_root, temp.path());
    }
}
