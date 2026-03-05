//! File-based module resolution for Rust.
//!
//! Resolves `mod` declarations to file paths following Rust's module resolution rules.
//! Supports both the traditional `mod.rs` convention and the newer flat structure.
//!
//! # Cross-File Module Linking
//!
//! This module provides the resolution logic for `mod foo;` declarations.
//! The actual cross-file edge creation requires:
//!
//! 1. All files to be indexed (to build complete `ExportMap`)
//! 2. Module context propagation (knowing each file's module path)
//! 3. Pass 4 processing to create import edges between modules
//!
//! Currently, module resolutions are stored as [`PendingModuleLink`] records
//! for future Pass 4 processing. See XXX for the full implementation plan.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::confidence::ConfidenceTracker;

/// Module resolution result.
#[derive(Debug, Clone)]
pub enum ModuleResolution {
    /// Module resolved to a file path
    Resolved(PathBuf),
    /// Module is inline (defined with braces, not a separate file)
    Inline,
    /// Could not resolve (file not found)
    NotFound { searched: Vec<PathBuf> },
}

/// A pending module link for cross-file resolution in Pass 4.
///
/// This captures the information needed to create a cross-file edge
/// from a `mod foo;` declaration to the file that defines the module.
///
/// # Note
///
/// Cross-file module linking is not yet implemented. This structure
/// is stored for future use when Pass 4 supports module-to-file linking.
#[derive(Debug, Clone)]
pub struct PendingModuleLink {
    /// Qualified name of the declared module (e.g., "`parent::foo`")
    pub module_qualified_name: String,
    /// File containing the `mod` declaration
    pub source_file: PathBuf,
    /// Resolved file path for the module content
    pub target_file: PathBuf,
    /// Line number of the `mod` declaration
    pub line: usize,
    /// Column number of the `mod` declaration
    pub column: usize,
}

/// Rust module resolver.
///
/// Resolves `mod foo;` declarations to file paths following Rust conventions.
pub struct ModuleResolver {
    /// Workspace root (where Cargo.toml is)
    workspace_root: PathBuf,
    /// Map of crate names to their root source files
    crate_roots: HashMap<String, PathBuf>,
    /// Cache of resolved modules: (`parent_file`, `mod_name`) -> path
    cache: HashMap<(PathBuf, String), ModuleResolution>,
    /// Pending module links for Pass 4 cross-file resolution
    pending_links: Vec<PendingModuleLink>,
}

impl ModuleResolver {
    /// Create a new module resolver.
    #[must_use]
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            crate_roots: HashMap::new(),
            cache: HashMap::new(),
            pending_links: Vec::new(),
        }
    }

    /// Record a pending module link for future Pass 4 processing.
    ///
    /// This stores the information needed to create a cross-file edge
    /// once all files have been indexed.
    pub fn record_pending_link(
        &mut self,
        module_qualified_name: String,
        source_file: PathBuf,
        target_file: PathBuf,
        line: usize,
        column: usize,
    ) {
        self.pending_links.push(PendingModuleLink {
            module_qualified_name,
            source_file,
            target_file,
            line,
            column,
        });
    }

    /// Get all pending module links.
    ///
    /// These are module declarations that resolved to external files
    /// and need cross-file edge creation in Pass 4.
    #[must_use]
    pub fn pending_links(&self) -> &[PendingModuleLink] {
        &self.pending_links
    }

    /// Take ownership of pending module links.
    ///
    /// This clears the internal collection and returns all pending links.
    pub fn take_pending_links(&mut self) -> Vec<PendingModuleLink> {
        std::mem::take(&mut self.pending_links)
    }

    /// Register a crate root (from Cargo metadata).
    pub fn register_crate(&mut self, crate_name: &str, root_path: PathBuf) {
        self.crate_roots.insert(crate_name.to_string(), root_path);
    }

    /// Handle `#[path = "..."]` attribute.
    ///
    /// Resolves the path relative to the declaring file's directory.
    ///
    /// # Arguments
    ///
    /// * `attr_value` - The path value from the attribute (e.g., "custom.rs")
    /// * `declaring_file` - The file containing the `mod` declaration
    ///
    /// # Returns
    ///
    /// `Some(PathBuf)` if the path exists, `None` otherwise.
    #[must_use]
    pub fn resolve_path_attribute(
        &self,
        attr_value: &str,
        declaring_file: &Path,
    ) -> Option<PathBuf> {
        let parent_dir = declaring_file.parent()?;
        let resolved = parent_dir.join(attr_value);
        if resolved.exists() {
            Some(resolved)
        } else {
            None
        }
    }

    /// Resolve a module declaration.
    ///
    /// # Arguments
    ///
    /// * `parent_file` - The file containing the `mod` declaration
    /// * `mod_name` - The module name (e.g., "foo" from `mod foo;`)
    /// * `confidence` - Confidence tracker for recording limitations
    ///
    /// # Returns
    ///
    /// The resolution result.
    pub fn resolve(
        &mut self,
        parent_file: &Path,
        mod_name: &str,
        confidence: &mut ConfidenceTracker,
    ) -> ModuleResolution {
        let cache_key = (parent_file.to_path_buf(), mod_name.to_string());

        if let Some(cached) = self.cache.get(&cache_key) {
            return cached.clone();
        }

        let result = Self::resolve_uncached(parent_file, mod_name, confidence);
        self.cache.insert(cache_key, result.clone());
        result
    }

    fn resolve_uncached(
        parent_file: &Path,
        mod_name: &str,
        confidence: &mut ConfidenceTracker,
    ) -> ModuleResolution {
        let parent_dir = parent_file.parent().unwrap_or(Path::new("."));
        let parent_stem = parent_file.file_stem().and_then(|s| s.to_str());

        // Determine search paths based on parent file name
        let search_paths = Self::get_search_paths(parent_dir, parent_stem, mod_name);

        for path in &search_paths {
            if path.exists() {
                return ModuleResolution::Resolved(path.clone());
            }
        }

        confidence.add_limitation(&format!(
            "Module '{}' not found, searched: {:?}",
            mod_name,
            search_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
        ));

        ModuleResolution::NotFound {
            searched: search_paths,
        }
    }

    fn get_search_paths(
        parent_dir: &Path,
        parent_stem: Option<&str>,
        mod_name: &str,
    ) -> Vec<PathBuf> {
        let mut paths = Vec::new();

        match parent_stem {
            // If parent is lib.rs, main.rs, or mod.rs, look in same directory
            Some("lib" | "main" | "mod") => {
                // foo.rs
                paths.push(parent_dir.join(format!("{mod_name}.rs")));
                // foo/mod.rs
                paths.push(parent_dir.join(mod_name).join("mod.rs"));
            }
            // Otherwise, look in a subdirectory named after the parent
            Some(stem) => {
                // parent/foo.rs
                paths.push(parent_dir.join(stem).join(format!("{mod_name}.rs")));
                // parent/foo/mod.rs
                paths.push(parent_dir.join(stem).join(mod_name).join("mod.rs"));
            }
            None => {
                paths.push(parent_dir.join(format!("{mod_name}.rs")));
            }
        }

        paths
    }

    /// Get the workspace root.
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Clear the resolution cache.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// Compute the module path for a file relative to the crate root.
    ///
    /// This determines where a file sits in the module hierarchy based on
    /// Rust's file-based module system conventions.
    ///
    /// # Returns
    ///
    /// - `None` for crate roots (`lib.rs`, `main.rs`)
    /// - `Some("extra")` for `src/extra.rs`
    /// - `Some("foo::bar")` for `src/foo/bar.rs`
    /// - `Some("foo")` for `src/foo/mod.rs`
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let resolver = ModuleResolver::new("/workspace".into());
    /// assert_eq!(resolver.compute_file_module_path(Path::new("/workspace/src/lib.rs")), None);
    /// assert_eq!(resolver.compute_file_module_path(Path::new("/workspace/src/extra.rs")), Some("extra".to_string()));
    /// assert_eq!(resolver.compute_file_module_path(Path::new("/workspace/src/foo/bar.rs")), Some("foo::bar".to_string()));
    /// ```
    #[must_use]
    pub fn compute_file_module_path(&self, file_path: &Path) -> Option<String> {
        // Find the src directory containing this file
        let src_dir = Self::find_src_dir(file_path)?;

        // Get path relative to src/
        let relative = file_path.strip_prefix(&src_dir).ok()?;

        // Get file stem (filename without extension)
        let file_stem = relative.file_stem()?.to_str()?;

        // Crate roots have no module prefix
        if file_stem == "lib" || file_stem == "main" {
            return None;
        }

        // For mod.rs, use parent directory name(s)
        if file_stem == "mod" {
            let parent = relative.parent()?;
            let parent_name = parent.file_name()?.to_str()?;
            let grandparent = parent.parent();

            return if grandparent.is_none_or(|p| p.as_os_str().is_empty()) {
                // Direct child of src/: src/foo/mod.rs -> "foo"
                Some(parent_name.to_string())
            } else {
                // Nested: src/foo/bar/mod.rs -> "foo::bar"
                let mut parts: Vec<&str> = grandparent?
                    .components()
                    .filter_map(|c| match c {
                        std::path::Component::Normal(os) => os.to_str(),
                        _ => None,
                    })
                    .collect();
                parts.push(parent_name);
                Some(parts.join("::"))
            };
        }

        // For regular files (foo.rs), use parent path + file stem
        let parent = relative.parent();
        if parent.is_none_or(|p| p.as_os_str().is_empty()) {
            // Direct child of src/: src/extra.rs -> "extra"
            Some(file_stem.to_string())
        } else {
            // Nested: src/foo/bar.rs -> "foo::bar"
            let mut parts: Vec<&str> = parent?
                .components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(os) => os.to_str(),
                    _ => None,
                })
                .collect();
            parts.push(file_stem);
            Some(parts.join("::"))
        }
    }

    /// Find the `src/` directory containing a file path.
    ///
    /// Walks up the directory tree looking for a directory named "src".
    fn find_src_dir(file_path: &Path) -> Option<PathBuf> {
        let mut current = file_path.parent()?;
        loop {
            if current.file_name()?.to_str()? == "src" {
                return Some(current.to_path_buf());
            }
            current = current.parent()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_workspace() -> TempDir {
        let dir = TempDir::new().unwrap();

        // Create src/lib.rs
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "mod foo;\nmod bar;").unwrap();

        // Create src/foo.rs (flat style)
        fs::write(src.join("foo.rs"), "pub fn hello() {}").unwrap();

        // Create src/bar/mod.rs (traditional style)
        let bar = src.join("bar");
        fs::create_dir_all(&bar).unwrap();
        fs::write(bar.join("mod.rs"), "mod baz;").unwrap();
        fs::write(bar.join("baz.rs"), "pub fn world() {}").unwrap();

        dir
    }

    #[test]
    fn test_resolve_flat_module() {
        let workspace = setup_test_workspace();
        let mut resolver = ModuleResolver::new(workspace.path().to_path_buf());
        let mut confidence = ConfidenceTracker::default();

        let lib_rs = workspace.path().join("src/lib.rs");
        let result = resolver.resolve(&lib_rs, "foo", &mut confidence);

        match result {
            ModuleResolution::Resolved(path) => {
                assert!(path.ends_with("foo.rs"));
            }
            _ => panic!("Expected Resolved"),
        }
    }

    #[test]
    fn test_resolve_traditional_module() {
        let workspace = setup_test_workspace();
        let mut resolver = ModuleResolver::new(workspace.path().to_path_buf());
        let mut confidence = ConfidenceTracker::default();

        let lib_rs = workspace.path().join("src/lib.rs");
        let result = resolver.resolve(&lib_rs, "bar", &mut confidence);

        match result {
            ModuleResolution::Resolved(path) => {
                assert!(path.ends_with("bar/mod.rs"));
            }
            _ => panic!("Expected Resolved"),
        }
    }

    #[test]
    fn test_resolve_nested_module() {
        let workspace = setup_test_workspace();
        let mut resolver = ModuleResolver::new(workspace.path().to_path_buf());
        let mut confidence = ConfidenceTracker::default();

        let bar_mod = workspace.path().join("src/bar/mod.rs");
        let result = resolver.resolve(&bar_mod, "baz", &mut confidence);

        match result {
            ModuleResolution::Resolved(path) => {
                assert!(path.ends_with("baz.rs"));
            }
            _ => panic!("Expected Resolved"),
        }
    }

    #[test]
    fn test_resolve_not_found() {
        let workspace = setup_test_workspace();
        let mut resolver = ModuleResolver::new(workspace.path().to_path_buf());
        let mut confidence = ConfidenceTracker::default();

        let lib_rs = workspace.path().join("src/lib.rs");
        let result = resolver.resolve(&lib_rs, "nonexistent", &mut confidence);

        match result {
            ModuleResolution::NotFound { searched } => {
                assert!(!searched.is_empty());
            }
            _ => panic!("Expected NotFound"),
        }
    }

    #[test]
    fn test_cache_hit() {
        let workspace = setup_test_workspace();
        let mut resolver = ModuleResolver::new(workspace.path().to_path_buf());
        let mut confidence = ConfidenceTracker::default();

        let lib_rs = workspace.path().join("src/lib.rs");

        // First call
        let _ = resolver.resolve(&lib_rs, "foo", &mut confidence);

        // Second call should hit cache
        let result = resolver.resolve(&lib_rs, "foo", &mut confidence);

        match result {
            ModuleResolution::Resolved(path) => {
                assert!(path.ends_with("foo.rs"));
            }
            _ => panic!("Expected Resolved from cache"),
        }
    }

    #[test]
    fn test_path_attribute_resolution() {
        let workspace = setup_test_workspace();

        // Create custom.rs in src/
        let custom_path = workspace.path().join("src/custom.rs");
        fs::write(&custom_path, "pub fn custom_fn() {}").unwrap();

        let resolver = ModuleResolver::new(workspace.path().to_path_buf());
        let lib_rs = workspace.path().join("src/lib.rs");

        // Test resolving #[path = "custom.rs"]
        let result = resolver.resolve_path_attribute("custom.rs", &lib_rs);
        assert!(result.is_some());
        assert!(result.unwrap().ends_with("custom.rs"));
    }

    #[test]
    fn test_path_attribute_subdirectory() {
        let workspace = setup_test_workspace();

        // Create subdir/special.rs
        let subdir = workspace.path().join("src/subdir");
        fs::create_dir_all(&subdir).unwrap();
        fs::write(subdir.join("special.rs"), "pub fn special() {}").unwrap();

        let resolver = ModuleResolver::new(workspace.path().to_path_buf());
        let lib_rs = workspace.path().join("src/lib.rs");

        // Test resolving #[path = "subdir/special.rs"]
        let result = resolver.resolve_path_attribute("subdir/special.rs", &lib_rs);
        assert!(result.is_some());
        assert!(result.unwrap().ends_with("special.rs"));
    }

    #[test]
    fn test_path_attribute_not_found() {
        let workspace = setup_test_workspace();
        let resolver = ModuleResolver::new(workspace.path().to_path_buf());
        let lib_rs = workspace.path().join("src/lib.rs");

        // Test resolving non-existent path
        let result = resolver.resolve_path_attribute("nonexistent.rs", &lib_rs);
        assert!(result.is_none());
    }

    // ========== compute_file_module_path tests ==========

    #[test]
    fn test_compute_file_module_path_lib_rs() {
        let workspace = setup_test_workspace();
        let resolver = ModuleResolver::new(workspace.path().to_path_buf());
        let lib_rs = workspace.path().join("src/lib.rs");

        assert_eq!(resolver.compute_file_module_path(&lib_rs), None);
    }

    #[test]
    fn test_compute_file_module_path_main_rs() {
        let workspace = setup_test_workspace();
        // Create main.rs
        fs::write(workspace.path().join("src/main.rs"), "fn main() {}").unwrap();

        let resolver = ModuleResolver::new(workspace.path().to_path_buf());
        let main_rs = workspace.path().join("src/main.rs");

        assert_eq!(resolver.compute_file_module_path(&main_rs), None);
    }

    #[test]
    fn test_compute_file_module_path_simple_module() {
        let workspace = setup_test_workspace();
        let resolver = ModuleResolver::new(workspace.path().to_path_buf());
        let foo_rs = workspace.path().join("src/foo.rs");

        assert_eq!(
            resolver.compute_file_module_path(&foo_rs),
            Some("foo".to_string())
        );
    }

    #[test]
    fn test_compute_file_module_path_nested_module() {
        let workspace = setup_test_workspace();
        // Create src/foo/nested.rs
        let nested_dir = workspace.path().join("src/foo");
        fs::create_dir_all(&nested_dir).unwrap();
        fs::write(nested_dir.join("nested.rs"), "pub fn nested() {}").unwrap();

        let resolver = ModuleResolver::new(workspace.path().to_path_buf());
        let nested_rs = workspace.path().join("src/foo/nested.rs");

        assert_eq!(
            resolver.compute_file_module_path(&nested_rs),
            Some("foo::nested".to_string())
        );
    }

    #[test]
    fn test_compute_file_module_path_mod_rs() {
        let workspace = setup_test_workspace();
        let resolver = ModuleResolver::new(workspace.path().to_path_buf());
        let bar_mod = workspace.path().join("src/bar/mod.rs");

        assert_eq!(
            resolver.compute_file_module_path(&bar_mod),
            Some("bar".to_string())
        );
    }

    #[test]
    fn test_compute_file_module_path_nested_mod_rs() {
        let workspace = setup_test_workspace();
        // Create src/bar/sub/mod.rs
        let nested_mod = workspace.path().join("src/bar/sub");
        fs::create_dir_all(&nested_mod).unwrap();
        fs::write(nested_mod.join("mod.rs"), "pub fn sub() {}").unwrap();

        let resolver = ModuleResolver::new(workspace.path().to_path_buf());
        let sub_mod = workspace.path().join("src/bar/sub/mod.rs");

        assert_eq!(
            resolver.compute_file_module_path(&sub_mod),
            Some("bar::sub".to_string())
        );
    }

    #[test]
    fn test_compute_file_module_path_deeply_nested() {
        let workspace = setup_test_workspace();
        // Create src/a/b/c.rs
        let deep_dir = workspace.path().join("src/a/b");
        fs::create_dir_all(&deep_dir).unwrap();
        fs::write(deep_dir.join("c.rs"), "pub fn deep() {}").unwrap();

        let resolver = ModuleResolver::new(workspace.path().to_path_buf());
        let deep_rs = workspace.path().join("src/a/b/c.rs");

        assert_eq!(
            resolver.compute_file_module_path(&deep_rs),
            Some("a::b::c".to_string())
        );
    }

    #[test]
    fn test_compute_file_module_path_no_src_dir() {
        // File outside src/ directory should return None
        let resolver = ModuleResolver::new(PathBuf::from("/workspace"));
        let random_file = Path::new("/some/random/file.rs");

        assert_eq!(resolver.compute_file_module_path(random_file), None);
    }
}
