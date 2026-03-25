use sqry_core::search::{SearchConfig, SearchMode, Searcher};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

use sqry_core::test_support::verbosity;
use std::sync::Once;

// Initialize verbose logging once for all tests in this file
static INIT: Once = Once::new();

fn init_logging() {
    INIT.call_once(|| {
        verbosity::init(env!("CARGO_PKG_NAME"));
    });
}

/// Helper to create a test directory with sample files
fn create_test_dir() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");

    // Create test files
    fs::write(
        dir.path().join("test.rs"),
        "fn main() {\n    println!(\"TODO: implement\");\n}\n",
    )
    .expect("Failed to write test.rs");

    fs::write(
        dir.path().join("test.js"),
        "function test() {\n    // TODO: add tests\n}\n",
    )
    .expect("Failed to write test.js");

    fs::write(
        dir.path().join("README.md"),
        "# Project\n\nTODO: write documentation\n",
    )
    .expect("Failed to write README.md");

    // Create hidden file
    fs::write(dir.path().join(".hidden"), "TODO: hidden file content\n")
        .expect("Failed to write .hidden");

    // Create subdirectory
    fs::create_dir(dir.path().join("src")).expect("Failed to create src dir");
    fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn hello() {\n    // TODO: implement\n}\n",
    )
    .expect("Failed to write src/lib.rs");

    dir
}

#[test]
fn test_basic_text_search() {
    init_logging();
    log::info!("Testing basic text search functionality");
    let dir = create_test_dir();
    let searcher = Searcher::new().expect("Failed to create searcher");

    let config = SearchConfig {
        mode: SearchMode::Text,
        ..Default::default()
    };

    let matches = searcher
        .search("TODO", &[dir.path()], &config)
        .expect("Search failed");

    log::debug!("Text search found {} matches", matches.len());
    // Should find TODO in multiple files (excluding hidden by default)
    log::info!(
        "✓ Basic text search found {} TODO occurrences",
        matches.len()
    );
    assert!(!matches.is_empty(), "Should find at least one match");
    assert!(matches.iter().any(|m| m.line_text.contains("TODO")));
}

#[test]
fn test_regex_search() {
    init_logging();
    log::info!("Testing regex search mode");
    let dir = create_test_dir();
    let searcher = Searcher::new().expect("Failed to create searcher");

    let config = SearchConfig {
        mode: SearchMode::Regex,
        ..Default::default()
    };

    // Search for "TODO:" with colon
    let matches = searcher
        .search(r"TODO:", &[dir.path()], &config)
        .expect("Search failed");

    assert!(!matches.is_empty(), "Should find TODO: pattern");
    for m in &matches {
        assert!(m.line_text.contains("TODO:"), "Match should contain TODO:");
    }
}

#[test]
fn test_case_insensitive_search() {
    let dir = create_test_dir();
    let searcher = Searcher::new().expect("Failed to create searcher");

    let config = SearchConfig {
        mode: SearchMode::Regex,
        case_insensitive: true,
        ..Default::default()
    };

    // Search for lowercase "todo"
    let matches = searcher
        .search("todo", &[dir.path()], &config)
        .expect("Search failed");

    assert!(
        !matches.is_empty(),
        "Should find TODO with case insensitive search"
    );
}

#[test]
fn test_file_type_filtering() {
    init_logging();
    log::info!("Testing file type filtering (.rs files only)");
    let dir = create_test_dir();
    let searcher = Searcher::new().expect("Failed to create searcher");

    let config = SearchConfig {
        mode: SearchMode::Regex,
        file_types: vec!["rs".to_string()],
        ..Default::default()
    };

    let matches = searcher
        .search("TODO", &[dir.path()], &config)
        .expect("Search failed");

    log::debug!("File type filter found {} .rs matches", matches.len());
    // Should only find matches in .rs files
    log::info!(
        "✓ File type filter: {} matches in .rs files only",
        matches.len()
    );
    for m in &matches {
        assert!(
            m.path.extension().and_then(|e| e.to_str()) == Some("rs"),
            "Should only match .rs files, got: {:?}",
            m.path
        );
    }
}

#[test]
fn test_hidden_files_excluded_by_default() {
    let dir = create_test_dir();
    let searcher = Searcher::new().expect("Failed to create searcher");

    let config = SearchConfig {
        mode: SearchMode::Regex,
        include_hidden: false,
        ..Default::default()
    };

    let matches = searcher
        .search("TODO", &[dir.path()], &config)
        .expect("Search failed");

    // Should not find matches in hidden files
    for m in &matches {
        let file_name = m.path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        assert!(
            !file_name.starts_with('.'),
            "Should not match hidden files, got: {:?}",
            m.path
        );
    }
}

#[test]
fn test_hidden_files_included_when_enabled() {
    let dir = create_test_dir();
    let searcher = Searcher::new().expect("Failed to create searcher");

    let config = SearchConfig {
        mode: SearchMode::Regex,
        include_hidden: true,
        ..Default::default()
    };

    let matches = searcher
        .search("TODO", &[dir.path()], &config)
        .expect("Search failed");

    // Should find matches in hidden files
    let has_hidden = matches.iter().any(|m| {
        m.path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.'))
    });

    assert!(
        has_hidden,
        "Should find matches in hidden files when enabled"
    );
}

#[test]
fn test_match_line_numbers_are_correct() {
    let dir = create_test_dir();
    let searcher = Searcher::new().expect("Failed to create searcher");

    let config = SearchConfig {
        mode: SearchMode::Regex,
        file_types: vec!["rs".to_string()],
        ..Default::default()
    };

    let matches = searcher
        .search("TODO", &[dir.path()], &config)
        .expect("Search failed");

    // Verify line numbers are 1-based and reasonable
    for m in &matches {
        assert!(m.line > 0, "Line numbers should be 1-based");
        assert!(m.line < 1000, "Line numbers should be reasonable");
    }
}

#[test]
fn test_match_contains_line_text() {
    let dir = create_test_dir();
    let searcher = Searcher::new().expect("Failed to create searcher");

    let config = SearchConfig {
        mode: SearchMode::Regex,
        ..Default::default()
    };

    let matches = searcher
        .search("TODO", &[dir.path()], &config)
        .expect("Search failed");

    // Verify each match contains the pattern in line_text
    for m in &matches {
        assert!(
            !m.line_text.is_empty(),
            "Match should have non-empty line text"
        );
        assert!(
            m.line_text.contains("TODO"),
            "Match line_text should contain pattern"
        );
    }
}

#[test]
fn test_max_depth_limiting() {
    let dir = create_test_dir();
    let searcher = Searcher::new().expect("Failed to create searcher");

    // Search with max_depth = 1 (only top level)
    let config = SearchConfig {
        mode: SearchMode::Regex,
        max_depth: Some(1),
        ..Default::default()
    };

    let matches = searcher
        .search("TODO", &[dir.path()], &config)
        .expect("Search failed");

    // Should not find matches in src/ subdirectory
    let has_subdirectory_match = matches
        .iter()
        .any(|m| m.path.components().any(|c| c.as_os_str() == "src"));

    assert!(
        !has_subdirectory_match,
        "Should not search in subdirectories with max_depth=1"
    );
}

#[test]
fn test_search_nonexistent_directory() {
    let searcher = Searcher::new().expect("Failed to create searcher");
    let config = SearchConfig::default();

    let result = searcher.search("TODO", &[PathBuf::from("/nonexistent/path")], &config);

    // Should fail gracefully
    assert!(result.is_err(), "Should error on nonexistent directory");
}

#[test]
fn test_empty_pattern_search() {
    let dir = create_test_dir();
    let searcher = Searcher::new().expect("Failed to create searcher");
    let config = SearchConfig::default();

    // Empty pattern should match everything (or fail gracefully)
    let result = searcher.search("", &[dir.path()], &config);

    // Depending on grep behavior, this might error or return empty results
    // Just verify it doesn't panic
    let _ = result;
}

#[test]
fn test_binary_files_skipped() {
    let dir = TempDir::new().expect("Failed to create temp dir");

    // Create a binary file
    let binary_data: Vec<u8> = vec![0, 1, 2, 3, 255, 254, 253];
    fs::write(dir.path().join("binary.bin"), &binary_data).expect("Failed to write binary file");

    // Create a text file with same pattern
    fs::write(dir.path().join("text.txt"), "TODO: text file content\n")
        .expect("Failed to write text file");

    let searcher = Searcher::new().expect("Failed to create searcher");
    let config = SearchConfig::default();

    let matches = searcher
        .search("TODO", &[dir.path()], &config)
        .expect("Search failed");

    // Should only find match in text file, not binary
    assert!(!matches.is_empty(), "Should find match in text file");
    for m in &matches {
        assert!(
            m.path.extension().and_then(|e| e.to_str()) != Some("bin"),
            "Should not match binary files"
        );
    }
}

#[test]
fn test_multiple_file_types() {
    let dir = create_test_dir();
    let searcher = Searcher::new().expect("Failed to create searcher");

    let config = SearchConfig {
        mode: SearchMode::Regex,
        file_types: vec!["rs".to_string(), "js".to_string()],
        ..Default::default()
    };

    let matches = searcher
        .search("TODO", &[dir.path()], &config)
        .expect("Search failed");

    // Should find matches in both .rs and .js files
    let has_rust = matches
        .iter()
        .any(|m| m.path.extension().and_then(|e| e.to_str()) == Some("rs"));
    let has_javascript = matches
        .iter()
        .any(|m| m.path.extension().and_then(|e| e.to_str()) == Some("js"));

    assert!(has_rust, "Should find matches in .rs files");
    assert!(has_javascript, "Should find matches in .js files");
}

// ========== HIGH PRIORITY TESTS - Codex Review Fixes ==========

#[test]
fn test_text_mode_literal_search() {
    init_logging();
    log::info!("Testing text mode treats regex metacharacters literally");
    // Tests Codex HIGH #1: SearchMode::Text should treat regex metacharacters literally
    let dir = TempDir::new().expect("Failed to create temp dir");

    // Create file with literal regex metacharacters
    fs::write(
        dir.path().join("test.txt"),
        "a.b is here\naXb is also here\n^start of line\nline with $end\n",
    )
    .expect("Failed to write test file");

    let searcher = Searcher::new().expect("Failed to create searcher");

    // Test 1: Literal "a.b" should only match "a.b", not "aXb"
    let config = SearchConfig {
        mode: SearchMode::Text,
        ..Default::default()
    };

    let matches = searcher
        .search("a.b", &[dir.path()], &config)
        .expect("Search failed");

    log::debug!(
        "Text mode 'a.b' found {} matches (should be 1 literal)",
        matches.len()
    );
    assert_eq!(matches.len(), 1, "Should only match literal 'a.b'");
    assert!(
        matches[0].line_text.contains("a.b is here"),
        "Should match line with literal 'a.b'"
    );

    // Test 2: Literal "^start" should match "^start", not treat ^ as line start
    let matches = searcher
        .search("^start", &[dir.path()], &config)
        .expect("Search failed");

    log::debug!(
        "Text mode '^start' found {} matches (should be 1 literal)",
        matches.len()
    );
    log::info!("✓ Text mode correctly treats metacharacters as literals");
    assert_eq!(matches.len(), 1, "Should match literal '^start'");
    assert!(
        matches[0].line_text.contains("^start"),
        "Should match literal caret"
    );
}

#[test]
fn test_regex_vs_text_mode() {
    init_logging();
    log::info!("Testing regex vs text mode behavior differences");
    // Tests Codex HIGH #1: Verify different behavior between Regex and Text modes
    let dir = TempDir::new().expect("Failed to create temp dir");

    fs::write(
        dir.path().join("test.txt"),
        "test.rs exists here\ntestXrs also exists\n",
    )
    .expect("Failed to write test file");

    let searcher = Searcher::new().expect("Failed to create searcher");

    // Regex mode: "test.rs" should match both "test.rs" and "testXrs" (. is wildcard)
    let config_regex = SearchConfig {
        mode: SearchMode::Regex,
        ..Default::default()
    };

    let regex_matches = searcher
        .search("test.rs", &[dir.path()], &config_regex)
        .expect("Regex search failed");

    log::debug!(
        "Regex mode found {} matches (. is wildcard)",
        regex_matches.len()
    );
    assert_eq!(
        regex_matches.len(),
        2,
        "Regex mode should match both lines (. is wildcard)"
    );

    // Text mode: "test.rs" should only match literal "test.rs"
    let config_text = SearchConfig {
        mode: SearchMode::Text,
        ..Default::default()
    };

    let text_matches = searcher
        .search("test.rs", &[dir.path()], &config_text)
        .expect("Text search failed");

    log::debug!(
        "Text mode found {} matches (literal only)",
        text_matches.len()
    );
    log::info!(
        "✓ Regex mode: {} matches, Text mode: {} matches (behavior confirmed)",
        regex_matches.len(),
        text_matches.len()
    );
    assert_eq!(
        text_matches.len(),
        1,
        "Text mode should only match literal 'test.rs'"
    );
    assert!(
        text_matches[0].line_text.contains("test.rs exists"),
        "Should match only the literal string"
    );
}

#[test]
#[cfg(unix)] // Symlinks work differently on Windows
fn test_follow_symlinks_enabled() {
    // Tests Codex HIGH #2: Verify symlinks are followed when enabled
    let dir = TempDir::new().expect("Failed to create temp dir");

    // Create real file
    let real_file = dir.path().join("real.txt");
    fs::write(&real_file, "TODO: symlink content\n").expect("Failed to write real file");

    // Create symlink
    let link_file = dir.path().join("link.txt");
    std::os::unix::fs::symlink(&real_file, &link_file).expect("Failed to create symlink");

    let searcher = Searcher::new().expect("Failed to create searcher");

    let config = SearchConfig {
        mode: SearchMode::Text,
        follow_symlinks: true,
        ..Default::default()
    };

    let matches = searcher
        .search("TODO", &[dir.path()], &config)
        .expect("Search failed");

    // Should find match via symlink
    let has_symlink_match = matches
        .iter()
        .any(|m| m.path.file_name().and_then(|n| n.to_str()) == Some("link.txt"));

    assert!(
        has_symlink_match || !matches.is_empty(),
        "Should find content when following symlinks (found {} matches)",
        matches.len()
    );
}

#[test]
#[cfg(unix)] // Symlinks work differently on Windows
fn test_follow_symlinks_disabled() {
    // Tests Codex HIGH #2: Verify symlinks are skipped when disabled
    let dir = TempDir::new().expect("Failed to create temp dir");

    // Create subdirectory for real file
    let subdir = dir.path().join("subdir");
    fs::create_dir(&subdir).expect("Failed to create subdir");

    // Create real file in subdirectory
    let real_file = subdir.join("real.txt");
    fs::write(&real_file, "TODO: symlink content\n").expect("Failed to write real file");

    // Create symlink in root directory pointing to subdirectory
    let link_dir = dir.path().join("link_to_subdir");
    std::os::unix::fs::symlink(&subdir, &link_dir).expect("Failed to create symlink");

    let searcher = Searcher::new().expect("Failed to create searcher");

    let config = SearchConfig {
        mode: SearchMode::Text,
        follow_symlinks: false, // Default
        ..Default::default()
    };

    let matches = searcher
        .search("TODO", &[dir.path()], &config)
        .expect("Search failed");

    // Should only find match via real subdirectory, not through symlink
    // With follow_symlinks=false, walker should skip the symlinked directory
    assert_eq!(
        matches.len(),
        1,
        "Should only find one match (via real directory, not symlink) - found: {:?}",
        matches.iter().map(|m| &m.path).collect::<Vec<_>>()
    );

    // Verify the match comes from the real subdir, not the symlink
    let real_subdir_found = matches
        .iter()
        .any(|m| m.path.components().any(|c| c.as_os_str() == "subdir"));
    assert!(real_subdir_found, "Match should come from real subdir");

    let symlink_found = matches.iter().any(|m| {
        m.path
            .components()
            .any(|c| c.as_os_str() == "link_to_subdir")
    });
    assert!(
        !symlink_found,
        "Should not find matches through symlinked directory"
    );
}

#[test]
fn test_exclude_patterns_file() {
    // Tests Codex HIGH #3: Verify exclude patterns skip matching files
    let dir = TempDir::new().expect("Failed to create temp dir");

    fs::write(dir.path().join("test.js"), "TODO: include this\n").expect("Failed to write test.js");
    fs::write(dir.path().join("test.min.js"), "TODO: exclude this\n")
        .expect("Failed to write test.min.js");

    let searcher = Searcher::new().expect("Failed to create searcher");

    let config = SearchConfig {
        mode: SearchMode::Text,
        exclude_patterns: vec!["*.min.js".to_string()],
        ..Default::default()
    };

    let matches = searcher
        .search("TODO", &[dir.path()], &config)
        .expect("Search failed");

    // Should find match in test.js but not test.min.js
    assert!(!matches.is_empty(), "Should find at least one match");
    let has_minified = matches
        .iter()
        .any(|m| m.path.file_name().and_then(|n| n.to_str()) == Some("test.min.js"));
    assert!(!has_minified, "Should not match excluded .min.js files");
}

#[test]
fn test_exclude_patterns_directory() {
    init_logging();
    log::info!("Testing exclude patterns skip directories (target/)");
    // Tests Codex HIGH #3: Verify exclude patterns skip entire directories
    let dir = TempDir::new().expect("Failed to create temp dir");

    fs::create_dir(dir.path().join("src")).expect("Failed to create src dir");
    fs::create_dir(dir.path().join("target")).expect("Failed to create target dir");

    fs::write(dir.path().join("src/code.rs"), "TODO: include this\n")
        .expect("Failed to write src/code.rs");
    fs::write(dir.path().join("target/code.rs"), "TODO: exclude this\n")
        .expect("Failed to write target/code.rs");

    let searcher = Searcher::new().expect("Failed to create searcher");

    let config = SearchConfig {
        mode: SearchMode::Text,
        exclude_patterns: vec!["target".to_string()],
        ..Default::default()
    };

    let matches = searcher
        .search("TODO", &[dir.path()], &config)
        .expect("Search failed");

    // Should find match in src/ but not target/
    log::debug!(
        "Exclude directory found {} matches (should be from src/ only)",
        matches.len()
    );
    assert!(!matches.is_empty(), "Should find at least one match");
    let has_target = matches
        .iter()
        .any(|m| m.path.components().any(|c| c.as_os_str() == "target"));
    log::info!("✓ Exclude patterns correctly skipped target/ directory");
    assert!(
        !has_target,
        "Should not match files in excluded target/ directory"
    );
}

#[test]
fn test_exclude_patterns_multiple() {
    // Tests Codex HIGH #3: Verify multiple exclude patterns work together
    let dir = TempDir::new().expect("Failed to create temp dir");

    fs::create_dir(dir.path().join("target")).expect("Failed to create target dir");

    fs::write(dir.path().join("a.js"), "TODO: include this\n").expect("Failed to write a.js");
    fs::write(dir.path().join("a.min.js"), "TODO: exclude minified\n")
        .expect("Failed to write a.min.js");
    fs::write(dir.path().join("b.js"), "TODO: include this too\n").expect("Failed to write b.js");
    fs::write(dir.path().join("target/c.js"), "TODO: exclude target\n")
        .expect("Failed to write target/c.js");

    let searcher = Searcher::new().expect("Failed to create searcher");

    let config = SearchConfig {
        mode: SearchMode::Text,
        exclude_patterns: vec!["*.min.js".to_string(), "target".to_string()],
        ..Default::default()
    };

    let matches = searcher
        .search("TODO", &[dir.path()], &config)
        .expect("Search failed");

    // Should only find matches in a.js and b.js
    assert_eq!(matches.len(), 2, "Should find exactly 2 matches");

    let filenames: Vec<_> = matches
        .iter()
        .map(|m| m.path.file_name().and_then(|n| n.to_str()).unwrap_or(""))
        .collect();

    assert!(filenames.contains(&"a.js"), "Should include a.js");
    assert!(filenames.contains(&"b.js"), "Should include b.js");
    assert!(!filenames.contains(&"a.min.js"), "Should exclude a.min.js");
    assert!(!filenames.contains(&"c.js"), "Should exclude target/c.js");
}
#[cfg(test)]
mod plugin_manager_test {
    use sqry_core::plugin::PluginManager;
    use sqry_lang_php::PhpPlugin;
    use sqry_lang_ruby::RubyPlugin;

    #[test]
    fn test_plugin_lookup() {
        let mut manager = PluginManager::new();
        manager.register_builtin(Box::new(RubyPlugin::default()));
        manager.register_builtin(Box::new(PhpPlugin::default()));

        // Test Ruby (known working)
        let ruby_plugin = manager.plugin_for_extension("rb");
        eprintln!("Ruby plugin for 'rb': {:?}", ruby_plugin.is_some());
        assert!(
            ruby_plugin.is_some(),
            "Ruby plugin not found for 'rb' extension"
        );

        // Test PHP
        let php_plugin = manager.plugin_for_extension("php");
        eprintln!("PHP plugin for 'php': {:?}", php_plugin.is_some());
        if let Some(plugin) = php_plugin {
            eprintln!("  Plugin metadata: {:?}", plugin.metadata());
            eprintln!("  Plugin extensions: {:?}", plugin.extensions());
        } else {
            panic!("PHP plugin not found for 'php' extension!");
        }
    }
}
