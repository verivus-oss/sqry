//! Workspace path resolution for MCP tools
//!
//! Provides 4-tier resolution strategy:
//! 1. Explicit `path` parameter (highest priority)
//! 2. `SQRY_MCP_WORKSPACE_ROOT` environment variable (primary security boundary)
//! 3. `SQRY_WORKSPACE_ROOT` environment variable (legacy fallback)
//! 4. Upward directory discovery from CWD (fallback)
//!
//! Discovery results are cached in an LRU cache with platform-specific
//! path normalization for case-insensitive filesystems.

use anyhow::{Context, Result, bail};
use sqry_core::config::WorkspaceConfig;
use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};

//=============================================================================
// Discovery Cache
//=============================================================================

/// Global discovery cache mapping normalized path strings to canonical workspace paths.
///
/// Cache keys are normalized based on platform:
/// - Windows: lowercase + backslash separators
/// - macOS: Runtime detection via pathconf (case-sensitive vs case-insensitive)
/// - Unix: unchanged (case-sensitive)
///
/// Uses `Mutex<Option<...>>` so tests can reset state between runs.
/// Must be initialized via `init_discovery_cache()` before first use
/// (typically during server startup).
static DISCOVERY_CACHE: parking_lot::Mutex<Option<lru::LruCache<String, PathBuf>>> =
    parking_lot::Mutex::new(None);

/// Initialize the discovery cache with the specified capacity.
///
/// This function must be called during server initialization before any
/// cache access. Subsequent calls are no-ops (idempotent).
///
/// # Panics
///
/// Panics if capacity is zero (prevented by `NonZeroUsize` type).
pub fn init_discovery_cache(capacity: std::num::NonZeroUsize) {
    let mut cache = DISCOVERY_CACHE.lock();
    if cache.is_none() {
        tracing::info!(capacity = capacity.get(), "Initializing discovery cache");
        *cache = Some(lru::LruCache::new(capacity));
    }
}

/// Normalize a path string for use as a cache key.
///
/// Normalization strategy is platform-specific:
/// - **Windows**: Lowercase + convert forward slashes to backslashes
/// - **macOS**: Runtime detection via `pathconf(_PC_CASE_SENSITIVE)`
///   - Case-insensitive filesystems: lowercase
///   - Case-sensitive filesystems: unchanged
///   - Error/indeterminate: assume case-sensitive (safer default)
/// - **Unix**: Unchanged (case-sensitive)
///
/// # Returns
///
/// A normalized string suitable for hash-based lookup on the current platform.
///
/// **Note**: Not yet integrated into tool implementations. Tool files still use local
/// `resolve_workspace_path()` functions. Will be used after tool refactoring.
#[allow(dead_code)]
fn normalize_discovery_key(path: &str) -> String {
    #[cfg(windows)]
    {
        path.to_lowercase().replace('/', "\\")
    }

    #[cfg(target_os = "macos")]
    {
        // Runtime detection via pathconf
        match is_case_sensitive_macos(path) {
            Ok(false) => path.to_lowercase(), // Case-insensitive filesystem
            Ok(true) | Err(_) => path.to_string(), // Case-sensitive or error (preserve case)
        }
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        path.to_string() // Case-sensitive Unix
    }
}

/// macOS: Check if a path is on a case-sensitive filesystem via pathconf.
///
/// Uses `pathconf(_PC_CASE_SENSITIVE)` to query filesystem properties.
/// If the path doesn't exist, probes the parent directory.
///
/// # Returns
///
/// - `Ok(true)` if filesystem is case-sensitive
/// - `Ok(false)` if filesystem is case-insensitive (HFS+, APFS non-sensitive)
/// - `Err(_)` on pathconf error
///
/// # Error Handling
///
/// - `errno=0, result=-1`: Indeterminate → return `Ok(true)` (conservative)
/// - `errno!=0`: Actual error → return `Err`
#[cfg(target_os = "macos")]
fn is_case_sensitive_macos(path: &str) -> Result<bool> {
    use std::ffi::CString;

    // Probe the path or its parent if path doesn't exist
    let probe_path = if Path::new(path).exists() {
        path.to_string()
    } else {
        Path::new(path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("/") // Fallback to root if no parent
            .to_string()
    };

    let c_path = CString::new(probe_path.as_bytes())?;

    // SAFETY: pathconf is a standard POSIX function, c_path is a valid C string
    let result = unsafe { libc::pathconf(c_path.as_ptr(), libc::_PC_CASE_SENSITIVE) };

    match result {
        -1 => {
            // Check errno to distinguish error from indeterminate
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if errno == 0 {
                // Indeterminate (pathconf returned -1, errno=0)
                // Conservative fallback: assume case-sensitive
                tracing::debug!(path, "pathconf indeterminate, assuming case-sensitive");
                Ok(true)
            } else {
                // Actual error
                bail!(
                    "pathconf(_PC_CASE_SENSITIVE) failed for {}: {}",
                    probe_path,
                    std::io::Error::last_os_error()
                )
            }
        }
        0 => Ok(false), // Case-insensitive
        _ => Ok(true),  // Case-sensitive
    }
}

/// Resolve a workspace path with caching.
///
/// This is the public API for path resolution with discovery caching.
/// Use this instead of `WorkspaceResolver::new(...).resolve()` when caching
/// is desired.
///
/// # Cache Behavior
///
/// - **Cache hit**: Return cached canonical path
/// - **Cache miss**: Resolve via `WorkspaceResolver`, cache result
///
/// # Errors
///
/// Returns an error if:
/// - Cache not initialized (call `init_discovery_cache()` first)
/// - Workspace resolution fails (no .sqry/graph found)
/// - Path canonicalization fails
///
/// **Note**: Used by `engine_for_workspace()` to provide cached workspace discovery.
/// Local tool `resolve_workspace_path()` helpers remain for backwards compatibility.
pub fn resolve_workspace_path(explicit_path: &str) -> Result<PathBuf> {
    // Normalize path for cache lookup
    let key = normalize_discovery_key(explicit_path);

    // Check cache (short lock scope)
    {
        let mut cache = DISCOVERY_CACHE.lock();
        let lru = cache
            .as_mut()
            .context("Discovery cache not initialized - call init_discovery_cache() first")?;
        if let Some(cached) = lru.get(&key) {
            tracing::debug!(
                path = explicit_path,
                cached = %cached.display(),
                "Discovery cache hit"
            );
            return Ok(cached.clone());
        }
    }

    // Cache miss - resolve via WorkspaceResolver (outside lock)
    tracing::debug!(path = explicit_path, "Discovery cache miss, resolving");

    let path_buf = PathBuf::from(explicit_path);
    let resolver = WorkspaceResolver::new(Some(path_buf));
    let resolved_path = resolver.resolve()?;

    // Insert into cache (short lock scope)
    {
        let mut cache = DISCOVERY_CACHE.lock();
        if let Some(lru) = cache.as_mut() {
            lru.put(key, resolved_path.clone());
            tracing::debug!(
                path = explicit_path,
                resolved = %resolved_path.display(),
                cache_size = lru.len(),
                "Discovery result cached"
            );
        }
    }

    Ok(resolved_path)
}

/// Resolves workspace root using 3-tier strategy
pub struct WorkspaceResolver {
    explicit_root: Option<PathBuf>,
}

impl WorkspaceResolver {
    /// Create a new resolver with optional explicit root
    pub fn new(explicit_root: Option<PathBuf>) -> Self {
        Self { explicit_root }
    }

    /// Resolve workspace root using priority order:
    /// 1. Explicit parameter
    /// 2. `SQRY_MCP_WORKSPACE_ROOT` env var (primary security boundary)
    /// 3. `SQRY_WORKSPACE_ROOT` env var (backward compatibility)
    /// 4. Discovery from CWD
    pub fn resolve(&self) -> Result<PathBuf> {
        // Priority 1: Explicit workspace_root parameter
        if let Some(root) = &self.explicit_root {
            tracing::info!("Using explicit workspace_root: {:?}", root);
            return self.validate_and_canonicalize(root);
        }

        // Priority 2: SQRY_MCP_WORKSPACE_ROOT environment variable (primary)
        if let Ok(root) = env::var("SQRY_MCP_WORKSPACE_ROOT") {
            tracing::info!("Using SQRY_MCP_WORKSPACE_ROOT env var: {}", root);
            let path = PathBuf::from(root);
            return self.validate_and_canonicalize(&path);
        }

        // Priority 3: SQRY_WORKSPACE_ROOT environment variable (backward compatibility)
        if let Ok(root) = env::var("SQRY_WORKSPACE_ROOT") {
            tracing::info!("Using SQRY_WORKSPACE_ROOT env var (legacy): {}", root);
            let path = PathBuf::from(root);
            return self.validate_and_canonicalize(&path);
        }

        // Priority 4: Discovery fallback
        tracing::info!("Using workspace discovery from CWD");
        let cwd = env::current_dir()?;
        self.discover_workspace(&cwd)
    }

    fn validate_and_canonicalize(&self, root: &Path) -> Result<PathBuf> {
        let canonical = root.canonicalize()?;
        self.validate_workspace(&canonical)?;
        Ok(canonical)
    }

    #[allow(clippy::unused_self)] // May use self in future for config/caching
    fn validate_workspace(&self, root: &Path) -> Result<()> {
        if !root.is_dir() {
            bail!(
                "Not a valid sqry workspace: {} (not a directory)",
                root.display()
            );
        }
        Ok(())
    }

    #[allow(clippy::unused_self)] // May use self in future for config/caching
    fn discover_workspace(&self, start: &Path) -> Result<PathBuf> {
        let config = WorkspaceConfig::load_or_default()?;
        let max_depth = config.effective_discovery_depth()?;

        let mut visited = HashSet::new();
        let mut current = start.canonicalize()?;

        for _ in 0..max_depth {
            // Symlink loop detection
            if !visited.insert(current.clone()) {
                bail!("Symlink loop detected at {}", current.display());
            }

            // Check for .sqry/graph
            if current.join(".sqry/graph").exists() {
                return Ok(current);
            }

            // Move to parent
            current = match current.parent() {
                Some(p) => p.canonicalize()?,
                None => bail!("No .sqry workspace found in parent directories"),
            };
        }

        bail!("Workspace discovery exceeded depth limit ({max_depth})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_explicit_parameter_priority() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path();
        fs::create_dir_all(workspace.join(".sqry/graph")).unwrap();

        let resolver = WorkspaceResolver::new(Some(workspace.to_path_buf()));
        let result = resolver.resolve();
        assert!(result.is_ok());
    }

    #[test]
    fn test_directory_without_graph_accepted() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path();
        // Don't create .sqry/graph — should still be accepted as workspace

        let resolver = WorkspaceResolver::new(Some(workspace.to_path_buf()));
        let result = resolver.resolve();
        assert!(
            result.is_ok(),
            "Directory without .sqry/graph should be accepted"
        );
    }

    #[test]
    #[serial_test::serial(workspace_env)]
    fn test_discovery_from_subdirectory() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path();
        let subdir = workspace.join("src/deep/nested");
        fs::create_dir_all(workspace.join(".sqry/graph")).unwrap();
        fs::create_dir_all(&subdir).unwrap();

        let old_cwd = env::current_dir().unwrap();
        env::set_current_dir(&subdir).unwrap();

        let resolver = WorkspaceResolver::new(None);
        let result = resolver.resolve();

        env::set_current_dir(old_cwd).unwrap();

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().canonicalize().unwrap(),
            workspace.canonicalize().unwrap()
        );
    }

    #[test]
    #[serial_test::serial(workspace_env)]
    #[ignore = "Symlink loop behavior is platform-dependent and may resolve differently"]
    fn test_symlink_loop_detected() {
        // Symlink loop detection test
        // Skip on platforms without symlink support
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let temp = TempDir::new().unwrap();
            let dir_a = temp.path().join("a");
            let dir_b = temp.path().join("b");

            fs::create_dir(&dir_a).unwrap();
            fs::create_dir(&dir_b).unwrap();

            // Create circular symlinks: a/next -> b, b/next -> a
            symlink(&dir_b, dir_a.join("next")).unwrap();
            symlink(&dir_a, dir_b.join("next")).unwrap();

            let old_cwd = env::current_dir().unwrap();
            env::set_current_dir(&dir_a).unwrap();

            let resolver = WorkspaceResolver::new(None);
            let result = resolver.resolve();

            env::set_current_dir(old_cwd).unwrap();

            assert!(result.is_err());
            let err_msg = result.unwrap_err().to_string();
            assert!(
                err_msg.contains("Symlink loop") || err_msg.contains("No .sqry workspace found")
            );
        }
    }

    #[test]
    #[serial_test::serial(workspace_env)]
    fn test_depth_limit_enforced() {
        // Create a deep directory structure without .sqry.
        // Use short segment names ("d0".."dN") to stay within Windows MAX_PATH (260 chars).
        // 30 levels of "dN/" is ~90 chars of segments, well within limits even with temp prefix.
        let temp = TempDir::new().unwrap();
        let mut deep_path = temp.path().to_path_buf();

        for i in 0..30 {
            deep_path = deep_path.join(format!("d{i}"));
        }
        fs::create_dir_all(&deep_path).unwrap();

        let old_cwd = env::current_dir().unwrap();
        env::set_current_dir(&deep_path).unwrap();

        let resolver = WorkspaceResolver::new(None);
        let result = resolver.resolve();

        env::set_current_dir(old_cwd).unwrap();

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("depth limit") || err_msg.contains("No .sqry workspace found"));
    }

    #[test]
    #[serial_test::serial(workspace_env)]
    fn test_env_var_resolution() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path();
        fs::create_dir_all(workspace.join(".sqry/graph")).unwrap();

        unsafe {
            env::set_var("SQRY_WORKSPACE_ROOT", workspace);
        }
        let resolver = WorkspaceResolver::new(None);
        let result = resolver.resolve();
        unsafe {
            env::remove_var("SQRY_WORKSPACE_ROOT");
        }

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().canonicalize().unwrap(),
            workspace.canonicalize().unwrap()
        );
    }
}

#[cfg(test)]
mod discovery_cache_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Reset the discovery cache to uninitialized state for test isolation.
    fn reset_discovery_cache() {
        let mut cache = DISCOVERY_CACHE.lock();
        *cache = None;
    }

    #[test]
    #[serial_test::serial(discovery_cache)]
    fn test_discovery_cache_requires_initialization() {
        reset_discovery_cache();

        // Attempt to resolve before initialization
        let result = resolve_workspace_path("/tmp/test");

        // Should fail with "not initialized" error
        match result {
            Err(e) => assert!(e.to_string().contains("not initialized")),
            Ok(_) => panic!("Expected error, got success"),
        }
    }

    #[test]
    #[serial_test::serial(discovery_cache)]
    fn test_discovery_cache_normalization() {
        // Initialize cache for this test
        init_discovery_cache(std::num::NonZeroUsize::new(100).unwrap());

        let temp = TempDir::new().unwrap();
        let workspace = temp.path();
        fs::create_dir_all(workspace.join(".sqry/graph")).unwrap();

        let path_str = workspace.to_str().unwrap();

        // First resolution should cache
        let result1 = resolve_workspace_path(path_str);
        assert!(result1.is_ok());

        // Second resolution should hit cache
        let result2 = resolve_workspace_path(path_str);
        assert!(result2.is_ok());

        assert_eq!(
            result1.unwrap().canonicalize().unwrap(),
            result2.unwrap().canonicalize().unwrap()
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_macos_pathconf_error_handling() {
        // Test pathconf with invalid path
        let result = is_case_sensitive_macos("/nonexistent/path/that/does/not/exist/very/deep");

        // Should either succeed (probed parent) or fail gracefully
        match result {
            Ok(is_sensitive) => {
                // Should assume case-sensitive on error/indeterminate
                assert!(is_sensitive);
            }
            Err(_) => {
                // Error is acceptable if entire path chain is invalid
            }
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_macos_pathconf_root() {
        // Test pathconf on root directory (always exists)
        let result = is_case_sensitive_macos("/");

        // Should succeed for root
        assert!(result.is_ok());
    }

    #[test]
    fn test_normalize_discovery_key_idempotent() {
        let path = "/tmp/test";
        let key1 = normalize_discovery_key(path);
        let key2 = normalize_discovery_key(&key1);

        // Normalization should be idempotent
        #[cfg(not(windows))]
        assert_eq!(key1, key2);

        #[cfg(windows)]
        {
            // Windows normalization changes both case and separators
            // Second normalization on already-normalized key should be unchanged
            assert_eq!(key1.to_lowercase(), key2);
        }
    }
}
