# End-to-End MCP Codebase Tests

Comprehensive integration tests for sqry MCP server using the active sqry codebase index.

## Overview

The `e2e_codebase_tests.rs` file contains 15 end-to-end tests that verify real-world MCP server functionality by querying the actual indexed sqry codebase. These tests cover all major MCP tool categories and validate both success and error handling.

## Test Coverage

### Search & Discovery (Tests 1-4)
1. **Semantic Search** - Find symbols by meaning (`semantic_search`)
2. **Pattern Search** - Find symbols by name pattern (`pattern_search`)
3. **Document Symbols** - List all symbols in a file (`get_document_symbols`)
4. **Workspace Symbols** - Search symbols across workspace (`get_workspace_symbols`)

### Graph Analysis (Tests 5-6)
5. **Graph Statistics** - Get codebase metrics (`get_graph_stats`)
6. **Index Status** - Check index health and metadata (`get_index_status`)

### Code Navigation (Tests 7-8)
7. **Find Definition** - Locate symbol definition (`get_definition`)
8. **Find References** - Find all symbol usages (`get_references`)

### Advanced Queries (Tests 9-12)
9. **Hierarchical Search** - Search with file/container grouping (`hierarchical_search`)
10. **List Files** - Get indexed files by language (`list_files`)
11. **Relation Query** - Find callers/callees (`relation_query`)
12. **List Symbols** - Get symbols filtered by kind (`list_symbols`)

### Deep Analysis (Tests 13-15)
13. **Explain Code** - Get detailed symbol explanation with context (`explain_code`)
14. **Cross-Language** - Detect cross-language calls (`cross_language_edges`)
15. **Dependencies** - Analyze dependency tree (`show_dependencies`)

## Running the Tests

### Run all E2E tests:
```bash
cargo test --package sqry-mcp --test e2e_codebase_tests
```

### Run single-threaded (recommended for consistency):
```bash
cargo test --package sqry-mcp --test e2e_codebase_tests -- --test-threads=1
```

### Run specific test:
```bash
cargo test --package sqry-mcp --test e2e_codebase_tests test_e2e_graph_statistics
```

## Test Results

**Status**: ✅ All 15 tests passing
**Runtime**: ~148 seconds (serial execution)
**Coverage**: 15 different MCP tools tested

## Test Architecture

### Error Handling
Tests use `validate_and_extract_response()` helper that:
- Validates JSON-RPC 2.0 protocol compliance
- Handles both success and error responses gracefully
- Extracts text content from MCP response format
- Allows some queries to return errors (e.g., symbol not found)

### Prerequisites
- sqry index must be built (`.sqry/graph/snapshot.sqry`)
- Tests run against actual codebase index (383,655 nodes, 652,530 edges)
- MCP server spawned for each test via `McpTestClient`

## Implementation Details

**File**: `sqry-mcp/tests/e2e_codebase_tests.rs`
**Test Framework**: Rust standard test harness
**Protocol**: MCP (Model Context Protocol) over stdio
**Response Format**: JSON-RPC 2.0

### Key Patterns

```rust
// Standard test pattern
let mut client = McpTestClient::new_initialized()?;
let response = client.call("tools/call", json!({
    "name": "tool_name",
    "arguments": { /* params */ }
}), request_id)?;
let text = validate_and_extract_response(&response)?;
assert!(!text.is_empty(), "Should return results");
```

## Maintenance

When adding new MCP tools:
1. Add corresponding E2E test
2. Use `validate_and_extract_response()` helper
3. Handle both success and error cases
4. Test against actual indexed codebase
5. Verify assertions are meaningful but tolerant

## Integration with CI

These tests validate:
- MCP protocol compliance
- Tool parameter validation
- Graph query correctness
- Error handling robustness
- Real-world codebase compatibility

Recommended to run before releases to ensure MCP server stability.
