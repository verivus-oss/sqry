//! Bridging header detection and C function symbol extraction for Swift↔C FFI.

use dashmap::DashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Parser, Tree};

/// Global cache of bridging header paths discovered per directory.
/// Key: Swift file directory path
/// Value: Path to bridging header (if found)
static BRIDGING_HEADER_CACHE: std::sync::LazyLock<DashMap<PathBuf, Option<PathBuf>>> =
    std::sync::LazyLock::new(DashMap::new);

/// Index mapping C function names to their source header.
/// Key: C function name
/// Value: Header file path
static C_FUNCTION_INDEX: std::sync::LazyLock<DashMap<String, PathBuf>> =
    std::sync::LazyLock::new(DashMap::new);

/// Locates bridging headers for Swift files.
pub struct BridgingHeaderLocator;

enum CacheLookup {
    Hit(Option<PathBuf>),
    Miss,
}

impl BridgingHeaderLocator {
    /// Find the bridging header for a given Swift file.
    /// Searches upward through parent directories for common patterns:
    /// - `*-Bridging-Header.h`
    /// - `ModuleName-Bridging-Header.h`
    /// - Headers next to `Package.swift`
    pub fn find_header(swift_file: &Path) -> Option<PathBuf> {
        let parent = swift_file.parent()?;

        // Check cache first
        match Self::cached_header(parent) {
            CacheLookup::Hit(cached) => return cached,
            CacheLookup::Miss => {}
        }

        // Search upward through directory tree
        let result = Self::search_upward_for_header(parent);

        // Cache result (even if None)
        BRIDGING_HEADER_CACHE.insert(parent.to_path_buf(), result.clone());
        result
    }

    /// Clear the cache (useful for tests).
    #[allow(dead_code)]
    pub fn clear_cache() {
        BRIDGING_HEADER_CACHE.clear();
    }

    fn cached_header(parent: &Path) -> CacheLookup {
        BRIDGING_HEADER_CACHE
            .get(parent)
            .map_or(CacheLookup::Miss, |entry| CacheLookup::Hit(entry.clone()))
    }

    fn search_upward_for_header(start_dir: &Path) -> Option<PathBuf> {
        let mut current_dir = start_dir.to_path_buf();
        loop {
            if let Some(header) = Self::find_bridging_header_in_dir(&current_dir) {
                return Some(header);
            }

            if Self::has_package_swift(&current_dir) {
                return Self::find_bridging_header_in_dir(&current_dir);
            }

            if !current_dir.pop() {
                return None;
            }
        }
    }

    fn find_bridging_header_in_dir(dir: &Path) -> Option<PathBuf> {
        let entries = fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && name.ends_with("-Bridging-Header.h")
            {
                return Some(path);
            }
        }
        None
    }

    fn has_package_swift(dir: &Path) -> bool {
        dir.join("Package.swift").exists()
    }
}

/// Index of C functions extracted from bridging headers.
pub struct SwiftBridgingIndex;

impl SwiftBridgingIndex {
    /// Parse a bridging header and index all C function declarations.
    ///
    /// # Errors
    ///
    /// Returns an error when the header file cannot be read, when the C parser
    /// fails to build an AST, or when tree-sitter encounters invalid syntax.
    pub fn index_header(header_path: &Path) -> Result<(), String> {
        let content =
            fs::read_to_string(header_path).map_err(|e| format!("Failed to read header: {e}"))?;

        let tree = Self::parse_c_header(&content)?;
        let functions = Self::extract_c_functions(&tree, content.as_bytes());

        // Index all functions
        for func_name in functions {
            C_FUNCTION_INDEX.insert(func_name, header_path.to_path_buf());
        }

        Ok(())
    }

    /// Check if a function name is a known C function from a bridging header.
    pub fn is_c_function(name: &str) -> Option<PathBuf> {
        C_FUNCTION_INDEX
            .get(name)
            .map(|entry| entry.value().clone())
    }

    /// Parse C header with tree-sitter-c.
    fn parse_c_header(content: &str) -> Result<Tree, String> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .map_err(|e| format!("Failed to set C language: {e}"))?;

        parser
            .parse(content, None)
            .ok_or_else(|| "Failed to parse C header".to_string())
    }

    /// Extract function names from C AST.
    fn extract_c_functions(tree: &Tree, content: &[u8]) -> Vec<String> {
        let mut functions = Vec::new();
        let mut stack = vec![tree.root_node()];

        while let Some(node) = stack.pop() {
            // Look for function declarations and definitions
            if node.kind() == "function_declarator" {
                Self::record_function_names(node, content, &mut functions);
            }

            // Push children
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }

        functions
    }

    fn record_function_names(node: Node<'_>, content: &[u8], functions: &mut Vec<String>) {
        if let Some(name) = Self::primary_function_name(node, content) {
            functions.push(name);
        }

        if let Some(name) = Self::declarator_function_name(node, content)
            && !functions.contains(&name)
        {
            functions.push(name);
        }
    }

    fn primary_function_name(node: Node<'_>, content: &[u8]) -> Option<String> {
        let name_node = node.child(0)?;
        if name_node.kind() != "identifier" {
            return None;
        }
        name_node.utf8_text(content).ok().map(str::to_string)
    }

    fn declarator_function_name(node: Node<'_>, content: &[u8]) -> Option<String> {
        let name_node = node.child_by_field_name("declarator")?;
        if name_node.kind() != "identifier" {
            return None;
        }
        name_node.utf8_text(content).ok().map(str::to_string)
    }

    /// Clear the index (useful for tests).
    #[allow(dead_code)]
    pub fn clear() {
        C_FUNCTION_INDEX.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_parse_simple_c_header() {
        let header = r"
            void hello_world(void);
            int calculate(int x, int y);
            char* get_string(void);
        ";

        let tree = SwiftBridgingIndex::parse_c_header(header).expect("parse failed");
        let functions = SwiftBridgingIndex::extract_c_functions(&tree, header.as_bytes());

        assert!(functions.contains(&"hello_world".to_string()));
        assert!(functions.contains(&"calculate".to_string()));
        assert!(functions.contains(&"get_string".to_string()));
    }

    #[test]
    fn test_bridging_header_locator() {
        let temp = TempDir::new().unwrap();
        let project_root = temp.path();

        // Create bridging header
        let header_path = project_root.join("MyApp-Bridging-Header.h");
        let mut header = File::create(&header_path).unwrap();
        writeln!(header, "void test_function(void);").unwrap();

        // Create Swift file in subdirectory
        let src_dir = project_root.join("Sources").join("MyApp");
        fs::create_dir_all(&src_dir).unwrap();
        let swift_file = src_dir.join("main.swift");
        File::create(&swift_file).unwrap();

        // Clear cache before test
        BridgingHeaderLocator::clear_cache();

        // Should find header
        let found = BridgingHeaderLocator::find_header(&swift_file);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), header_path);
    }

    #[test]
    fn test_index_and_lookup() {
        SwiftBridgingIndex::clear();

        let temp = TempDir::new().unwrap();
        let header_path = temp.path().join("test.h");
        let mut header = File::create(&header_path).unwrap();
        writeln!(header, "void my_c_function(int x);").unwrap();
        drop(header);

        // Index the header
        SwiftBridgingIndex::index_header(&header_path).expect("index failed");

        // Lookup should find it
        assert!(SwiftBridgingIndex::is_c_function("my_c_function").is_some());
        assert!(SwiftBridgingIndex::is_c_function("unknown_function").is_none());
    }
}
