use super::*;
use std::path::Path;

#[test]
fn test_executor_new() {
    let executor = QueryExecutor::new();
    // Executor is created successfully (plugins will be registered at runtime)
    assert!(executor.plugin_manager.plugins().is_empty());
}

#[test]
fn test_execute_on_graph_nonexistent_path() {
    let executor = QueryExecutor::new();
    let result = executor.execute_on_graph("kind:function", Path::new("/nonexistent/path"));
    // Should error because no graph exists
    assert!(result.is_err());
}

// Note: Full integration tests with actual plugins are in the CLI integration tests
// where plugins are properly registered. Tests for predicate evaluation and index
// operations were removed as part of the legacy index to CodeGraph migration.
