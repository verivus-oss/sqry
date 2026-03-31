# VSCode Extension Integration Tests

This document outlines the integration tests needed for the sqry VSCode extension.

## Test Categories

### 1. Query Execution Tests

#### 1.1 Simple Text Search
- **Test**: Search for plain text (e.g., "Error")
- **Expected**: Should fall back to old parser and return text matches
- **Verifies**: Query parser fallback logic works

#### 1.2 Structured Query Search
- **Test**: Search with field predicates (e.g., "kind:function")
- **Expected**: Should use new AST parser and return matching symbols
- **Verifies**: New query parser works for structured queries

#### 1.3 Mixed Query Search
- **Test**: Search with multiple predicates (e.g., "kind:function returns:Result")
- **Expected**: Should return functions that return Result type
- **Verifies**: Complex query parsing and execution

#### 1.4 Invalid Query
- **Test**: Search with invalid syntax (e.g., "kind:")
- **Expected**: Should show helpful error message
- **Verifies**: Error handling for malformed queries

### 2. Index Management Tests

#### 2.1 Missing Index
- **Test**: Open workspace without .sqry-index file
- **Expected**: Should prompt to create index
- **Verifies**: Auto-index on open setting works

#### 2.2 Outdated Index
- **Test**: Trigger query execution with outdated/corrupted index
- **Expected**: Should show error with "Rebuild Index" button
- **Verifies**: Index error detection and rebuild flow

#### 2.3 Manual Index Rebuild
- **Test**: Run "Sqry: Index Workspace" command
- **Expected**: Should show progress and rebuild index successfully
- **Verifies**: Manual indexing works

#### 2.4 Index Status Check
- **Test**: Check if workspace has valid index
- **Expected**: Should correctly detect index presence
- **Verifies**: Index detection logic

### 3. Timeout Handling Tests

#### 3.1 Query Timeout
- **Test**: Run a query that exceeds timeoutMs setting
- **Expected**: Should show error with "Increase Timeout" button
- **Verifies**: Timeout detection and user guidance

#### 3.2 Timeout Setting Update
- **Test**: Click "Increase Timeout" and modify setting
- **Expected**: Should open settings and allow timeout adjustment
- **Verifies**: Settings integration

### 4. Binary Path Tests

#### 4.1 Missing Binary
- **Test**: Set sqry.path to non-existent binary
- **Expected**: Should show error with "Open Settings" button
- **Verifies**: Binary validation

#### 4.2 Custom Binary Path
- **Test**: Set sqry.path to custom location
- **Expected**: Should use custom binary for LSP
- **Verifies**: Custom path configuration

#### 4.3 Binary in PATH
- **Test**: Set sqry.path to "sqry" (use PATH)
- **Expected**: Should find and use binary from PATH
- **Verifies**: PATH resolution

### 5. UI Integration Tests

#### 5.1 Search from Sidebar
- **Test**: Click search icon in Semantic Results panel
- **Expected**: Should show input dialog and execute search
- **Verifies**: UI integration for search action

#### 5.2 Query from Sidebar
- **Test**: Click query icon in Semantic Results panel
- **Expected**: Should show input dialog for semantic query
- **Verifies**: UI integration for query action

#### 5.3 Welcome View Actions
- **Test**: Click "Search Workspace" from welcome view
- **Expected**: Should prompt for search term and execute
- **Verifies**: Welcome view integration

#### 5.4 Results Display
- **Test**: Execute search and verify results appear
- **Expected**: Should show symbols and text matches in tree view
- **Verifies**: Results rendering

#### 5.5 Result Navigation
- **Test**: Click on a search result
- **Expected**: Should open file and navigate to symbol/line
- **Verifies**: Navigation from results

### 6. LSP Communication Tests

#### 6.1 LSP Server Startup
- **Test**: Open workspace and check LSP activation
- **Expected**: LSP server should start and report ready
- **Verifies**: LSP initialization

#### 6.2 LSP Request/Response
- **Test**: Send sqry/search request via LSP
- **Expected**: Should receive properly formatted response
- **Verifies**: LSP protocol compliance

#### 6.3 LSP Error Handling
- **Test**: Trigger LSP error (e.g., invalid path)
- **Expected**: Should propagate error to UI with helpful message
- **Verifies**: LSP error propagation

#### 6.4 LSP Restart on Config Change
- **Test**: Change sqry.path setting
- **Expected**: LSP should restart with new binary
- **Verifies**: Configuration hot-reload

### 7. Multi-Workspace Tests

#### 7.1 Multiple Workspace Folders
- **Test**: Open multi-root workspace
- **Expected**: Should index and search each folder independently
- **Verifies**: Multi-workspace support

#### 7.2 Workspace Folder Detection
- **Test**: Run search with multiple folders open
- **Expected**: Should search in active workspace folder
- **Verifies**: Correct workspace resolution

### 8. Error Recovery Tests

#### 8.1 Query Execution Failure Recovery
- **Test**: Trigger query failure, then fix and retry
- **Expected**: Should recover and execute successfully
- **Verifies**: Error recovery without reload

#### 8.2 LSP Crash Recovery
- **Test**: Kill LSP process and trigger new request
- **Expected**: Should restart LSP and continue working
- **Verifies**: LSP resilience

#### 8.3 Index Corruption Recovery
- **Test**: Corrupt index file and trigger search
- **Expected**: Should detect corruption and offer rebuild
- **Verifies**: Index validation and recovery

### 9. Performance Tests

#### 9.1 Large Repository Search
- **Test**: Search in repository with 10k+ files
- **Expected**: Should complete within timeout and show results
- **Verifies**: Performance at scale

#### 9.2 Concurrent Searches
- **Test**: Trigger multiple searches in quick succession
- **Expected**: Should handle gracefully (queue or cancel)
- **Verifies**: Concurrency handling

#### 9.3 Index Build Performance
- **Test**: Index large repository and measure time
- **Expected**: Should complete in reasonable time with progress
- **Verifies**: Indexing performance

## Test Implementation Plan

### Phase 1: Unit Tests
- Create mocks for VSCode API
- Test error handling functions in isolation
- Test query parameter construction
- Test result parsing

### Phase 2: Integration Tests
- Set up test workspace with known files
- Test LSP communication with real binary
- Test full query execution flow
- Test UI interactions via VSCode extension test runner

### Phase 3: End-to-End Tests
- Test complete user workflows
- Test error scenarios and recovery
- Test multi-workspace scenarios
- Performance benchmarks

## Test Infrastructure

### Required Tools
- `@vscode/test-electron` - VSCode extension testing framework
- `mocha` - Test runner (already in devDependencies)
- `chai` - Assertions (already in devDependencies)
- Test workspace fixtures

### Test Fixtures Needed
1. Sample workspace with:
   - Various file types (Rust, TypeScript, etc.)
   - Pre-built .sqry-index
   - Known symbols for verification
2. Corrupted index file for error testing
3. Empty workspace for indexing tests

### CI Integration
- Run tests on multiple OS (Linux, macOS, Windows)
- Test with different VSCode versions
- Test with different sqry binary versions
- Generate coverage reports

## Success Criteria

All tests must:
- ✅ Pass consistently (no flakiness)
- ✅ Complete within reasonable time (<5min total)
- ✅ Cover >80% of code paths
- ✅ Run in CI on every PR
- ✅ Include both happy path and error scenarios

## Current Status

- [ ] Test infrastructure set up
- [ ] Unit tests written
- [ ] Integration tests written
- [ ] E2E tests written
- [ ] CI integration complete
- [ ] Documentation complete
