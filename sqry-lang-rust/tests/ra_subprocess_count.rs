//! Integration tests for RA bridge subprocess spawn count.
//!
//! These tests verify that the performance fix correctly caches rust-analyzer
//! availability checks, reducing subprocess spawns from O(2N) to O(1).
//!
//! Note: Unix-only tests that manipulate PATH require `serial_test` for isolation.

#[cfg(unix)]
mod test_helpers {
    use std::path::Path;

    /// RAII guard for PATH environment variable manipulation.
    ///
    /// Saves the original PATH on creation and restores it on drop.
    /// Uses `unsafe` because `set_var` is not thread-safe, but this is
    /// acceptable in tests guarded by `serial_test`.
    pub struct PathGuard {
        original: String,
    }

    impl PathGuard {
        pub fn new() -> Self {
            Self {
                original: std::env::var("PATH").unwrap_or_default(),
            }
        }

        /// Prepend a directory to PATH.
        pub fn prepend(&self, dir: &Path) {
            // SAFETY: Single-threaded test (serial_test), PATH restoration guaranteed
            unsafe {
                std::env::set_var("PATH", format!("{}:{}", dir.display(), self.original));
            }
        }
    }

    impl Drop for PathGuard {
        fn drop(&mut self) {
            // SAFETY: Restoring original PATH
            unsafe {
                std::env::set_var("PATH", &self.original);
            }
        }
    }
}

#[cfg(unix)]
mod unix_tests {
    use super::test_helpers::PathGuard;
    use serial_test::serial;
    use sqry_core::graph::GraphBuilder;
    use sqry_core::graph::unified::StagingGraph;
    use sqry_lang_rust::relations::graph_builder::RustGraphBuilder;
    use std::os::unix::fs::PermissionsExt;

    /// T4: Verify exactly 1 subprocess spawn per builder instance
    ///
    /// This test creates a shim that counts invocations, then builds graphs
    /// for multiple files using the SAME builder. The counter should only
    /// increment once (on first file), proving the cache works.
    #[test]
    #[serial]
    fn test_single_subprocess_per_builder() {
        use tempfile::tempdir;

        let _guard = PathGuard::new();

        // Create a counter file
        let temp = tempdir().unwrap();
        let counter_file = temp.path().join("ra_call_count");
        std::fs::write(&counter_file, "0").unwrap();

        // Create a shim script that increments counter (POSIX sh, not bash)
        let shim_dir = temp.path().join("shim");
        std::fs::create_dir_all(&shim_dir).unwrap();

        let shim_path = shim_dir.join("rust-analyzer");
        std::fs::write(
            &shim_path,
            format!(
                r#"#!/bin/sh
count=$(cat {counter})
echo $((count + 1)) > {counter}
echo "rust-analyzer 1.85.0 (fake)"
"#,
                counter = counter_file.display()
            ),
        )
        .unwrap();

        let mut perms = std::fs::metadata(&shim_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&shim_path, perms).unwrap();

        _guard.prepend(&shim_dir);

        // Build graph for multiple files using SAME builder
        let builder = RustGraphBuilder::default();
        let files = vec!["test1.rs", "test2.rs", "test3.rs"];

        for file_name in &files {
            let file_path = temp.path().join(file_name);
            std::fs::write(&file_path, "fn main() {}").unwrap();

            let content = std::fs::read(&file_path).unwrap();
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_rust::LANGUAGE.into())
                .unwrap();
            let tree = parser.parse(&content, None).unwrap();

            let mut staging = StagingGraph::new();
            builder
                .build_graph(&tree, &content, &file_path, &mut staging)
                .unwrap();
        }

        // Verify exactly 1 subprocess call
        let count: u32 = std::fs::read_to_string(&counter_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        assert_eq!(
            count,
            1,
            "Expected exactly 1 subprocess spawn for {} files, got {}",
            files.len(),
            count
        );
    }

    /// Additional test: Verify cloned builders each get their own subprocess call.
    ///
    /// Clone creates independent cache, so each cloned builder should
    /// trigger its own subprocess spawn (but still only once per builder).
    #[test]
    #[serial]
    fn test_cloned_builders_separate_subprocess_calls() {
        use tempfile::tempdir;

        let _guard = PathGuard::new();

        // Create a counter file
        let temp = tempdir().unwrap();
        let counter_file = temp.path().join("ra_call_count");
        std::fs::write(&counter_file, "0").unwrap();

        // Create a shim script that increments counter
        let shim_dir = temp.path().join("shim");
        std::fs::create_dir_all(&shim_dir).unwrap();

        let shim_path = shim_dir.join("rust-analyzer");
        std::fs::write(
            &shim_path,
            format!(
                r#"#!/bin/sh
count=$(cat {counter})
echo $((count + 1)) > {counter}
echo "rust-analyzer 1.85.0 (fake)"
"#,
                counter = counter_file.display()
            ),
        )
        .unwrap();

        let mut perms = std::fs::metadata(&shim_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&shim_path, perms).unwrap();

        _guard.prepend(&shim_dir);

        // Create first builder and use it
        let builder1 = RustGraphBuilder::default();
        let file_path = temp.path().join("test1.rs");
        std::fs::write(&file_path, "fn main() {}").unwrap();

        let content = std::fs::read(&file_path).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(&content, None).unwrap();

        let mut staging = StagingGraph::new();
        builder1
            .build_graph(&tree, &content, &file_path, &mut staging)
            .unwrap();

        // Count should be 1
        let count1: u32 = std::fs::read_to_string(&counter_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(count1, 1, "First builder should spawn 1 subprocess");

        // Clone and use second builder
        let builder2 = builder1.clone();
        let file_path2 = temp.path().join("test2.rs");
        std::fs::write(&file_path2, "fn foo() {}").unwrap();

        let content2 = std::fs::read(&file_path2).unwrap();
        let tree2 = parser.parse(&content2, None).unwrap();

        let mut staging2 = StagingGraph::new();
        builder2
            .build_graph(&tree2, &content2, &file_path2, &mut staging2)
            .unwrap();

        // Count should be 2 (cloned builder has fresh cache)
        let count2: u32 = std::fs::read_to_string(&counter_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            count2, 2,
            "Cloned builder should spawn its own subprocess (total 2)"
        );

        // Use original builder again - should NOT increment (cached)
        let file_path3 = temp.path().join("test3.rs");
        std::fs::write(&file_path3, "fn bar() {}").unwrap();

        let content3 = std::fs::read(&file_path3).unwrap();
        let tree3 = parser.parse(&content3, None).unwrap();

        let mut staging3 = StagingGraph::new();
        builder1
            .build_graph(&tree3, &content3, &file_path3, &mut staging3)
            .unwrap();

        let count3: u32 = std::fs::read_to_string(&counter_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            count3, 2,
            "Original builder should still use cache (no new subprocess)"
        );
    }
}
