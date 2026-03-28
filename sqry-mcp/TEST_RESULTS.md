# sqry MCP Server - Test Results (Deprecated)

**Status**: ⚠️ Superseded by `docs/development/sqry-mcp-rmcp-default/06_TEST_EXECUTION.md`
**Version**: 0.17.0
**Last Verified**: 2025-10-06
**Deprecated Notice Date**: 2026-01-12

## Test Summary

```
======================================
  sqry MCP Server Integration Tests
======================================

Tests run:    10
Passed:       10
Failed:       0
```

## Test Coverage

### Protocol Compliance (JSON-RPC 2.0)

| Test | Status | Description |
|------|--------|-------------|
| **Initialize** | ✅ PASS | Server responds to `initialize` with protocol version and capabilities |
| **Tools List** | ✅ PASS | Returns all 3 tools (search, query, index_status) with correct schemas |
| **Unknown Method** | ✅ PASS | Returns proper JSON-RPC error for unknown methods |
| **Request ID Propagation** | ✅ PASS | Request IDs correctly propagated to responses |

### Tool Functionality

| Tool | Status | Description |
|------|--------|-------------|
| **sqry_search** | ✅ PASS | Fuzzy search returns valid JSON results |
| **sqry_query** | ✅ PASS | Structured queries return valid JSON results |
| **sqry_index_status** | ✅ PASS | Index status returns expected fields (status, path, symbol_count) |

### Error Handling

| Test | Status | Description |
|------|--------|-------------|
| **Unknown Tool** | ✅ PASS | Returns proper error for non-existent tools |
| **Output Truncation** | ✅ PASS | Large outputs truncated at 50KB limit |

### Concurrency

| Test | Status | Description |
|------|--------|-------------|
| **Multiple Calls** | ✅ PASS | Sequential tool calls work correctly with proper ID tracking |

## Test Details

### 1. Initialize Request
**Purpose**: Verify MCP server handshake
**Request**:
```json
{"jsonrpc":"2.0","method":"initialize","params":{},"id":1}
```
**Expected**: Server returns protocol version `2024-11-05` and server info
**Result**: ✅ PASS

### 2. Tools List
**Purpose**: Verify all tools are exposed
**Expected**: 3 tools with correct names and input schemas
**Result**: ✅ PASS
**Tools**:
- `sqry_search` - Fuzzy symbol search
- `sqry_query` - Structured queries with boolean logic
- `sqry_index_status` - Index health check

### 3. Search Tool (`sqry_search`)
**Purpose**: Test fuzzy search functionality
**Command**: `sqry --fuzzy search "pattern" "path" --json --limit 50`
**Expected**: Valid JSON with `query`, `stats`, `results`
**Result**: ✅ PASS

### 4. Query Tool (`sqry_query`)
**Purpose**: Test structured query execution
**Command**: `sqry query "kind:function" --json --limit 1000 "path"`
**Expected**: Valid JSON with query results
**Result**: ✅ PASS

### 5. Index Status Tool (`sqry_index_status`)
**Purpose**: Check index metadata
**Command**: `sqry index --status --json "path"`
**Expected**: JSON with `status`, `index_path`, `symbol_count`
**Result**: ✅ PASS

### 6. Error Handling
**Purpose**: Verify graceful error responses
**Tests**:
- Unknown method → JSON-RPC error
- Unknown tool → Error message
- Large output → Truncated at 50KB

**Result**: ✅ PASS

### 7. Output Truncation
**Purpose**: Prevent overwhelming responses
**Test**: Query all functions (potentially large result)
**Expected**: Output ≤ 50KB + "[TRUNCATED]" marker
**Result**: ✅ PASS (50,015 bytes, properly truncated)

### 8. Request ID Propagation
**Purpose**: Verify JSON-RPC 2.0 compliance
**Test**: Send request with `id:999`
**Expected**: Response has `id:999`
**Result**: ✅ PASS

### 9. Multiple Tool Calls
**Purpose**: Test sequential execution
**Test**: Call search then query
**Expected**: Both return correct IDs and results
**Result**: ✅ PASS

## Known Limitations

1. **Bash Subshells**: Test harness uses subshells with `set +e` to handle MCP server's stdin loop
2. **Arithmetic Expressions**: Bash `((TESTS_RUN++))` returns exit code 1 when value is 0, incompatible with `set -e`
3. **Index Requirement**: `sqry_search` requires pre-built index (automatically created in tests)

## Performance

| Operation | Time | Notes |
|-----------|------|-------|
| Initialize | <100ms | Protocol handshake |
| Tools List | <100ms | Schema enumeration |
| Search (fuzzy) | ~20ms | With warm index |
| Query | ~15ms | Boolean logic |
| Index Status | ~5ms | Metadata read |

## CLI Argument Order Discovery

During testing, discovered critical CLI argument order requirements:

```bash
# ✅ CORRECT
sqry --fuzzy search "pattern" "path" --json --limit 50
sqry query "kind:function" --json --limit 1000 "path"
sqry index --status --json "path"

# ❌ INCORRECT (causes "unexpected argument" errors)
sqry search --fuzzy "pattern" "path" --json --limit 50
sqry query "kind:function" "path" --json --limit 1000
sqry index "path" --status --json
```

**Key Rules**:
1. Global flags (--fuzzy, --json, --limit) MUST come before positional arguments
2. Subcommand comes after global flags but before positional arguments
3. Order: `sqry [GLOBAL_FLAGS] SUBCOMMAND [SUBCOMMAND_FLAGS] <ARGS>`

## Integration Test Script

**Legacy Script**: `sqry-mcp/test_mcp_server.sh` (removed in rmcp-only migration)

**Run Tests**:
```bash
cargo test -p sqry-mcp
```

## Dependencies

- **jq**: JSON parsing and validation
- **sqry**: v0.17.0+ (fuzzy search and index status support)
- **bash**: 4.0+ (associative arrays)

## Conclusion

The sqry MCP server is **production-ready** with:
- ✅ Full JSON-RPC 2.0 compliance
- ✅ All 3 core tools functional
- ✅ Proper error handling
- ✅ Output safety limits
- ✅ 100% test pass rate

**Ready for**: Codex CLI, Gemini CLI, Windsurf, Claude Desktop, Cursor IDE integration
