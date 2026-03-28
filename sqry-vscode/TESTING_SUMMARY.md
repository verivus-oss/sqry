# Testing Summary - VSCode Extension

## Current Status

### What We Fixed Today

1. **Query Parser Fallback Issue**
   - **Problem**: Simple text searches (e.g., "Error") were failing with "Invalid predicate format" error
   - **Root Cause**: New AST query parser was successfully parsing but validation was failing, preventing fallback to old parser
   - **Fix**: Added validation error handling to fall back to old parser when validation fails
   - **Location**: `sqry-core/src/query/executor.rs:561-570`

2. **LSP Error Logging**
   - **Problem**: Errors weren't showing detailed information for debugging
   - **Fix**: Added debug and error logging to show full error chain
   - **Location**: `sqry-lsp/src/handlers/search.rs:19-31`

3. **Extension Error Handling**
   - **Problem**: Generic error messages weren't actionable
   - **Fix**: Added specific error handlers for:
     - Query execution failures → "Rebuild Index" button
     - Timeout errors → "Increase Timeout" button
     - Binary path issues → "Open Settings" button
   - **Location**: `tools/sqry-vscode/src/extension.ts:344-408`

4. **VSCode Extension Packaging**
   - **Problem**: Extension wasn't including `node_modules` dependencies
   - **Fix**: Removed `node_modules/` from `.vscodeignore`
   - **Location**: `tools/sqry-vscode/.vscodeignore`

### Test Infrastructure Created

1. **Test Documentation**
   - `INTEGRATION_TESTS.md` - Comprehensive test plan covering all scenarios
   - `tests/README.md` - Guide for running and writing tests

2. **Test Structure**
   - `tests/integration/search.test.ts` - Integration test skeleton
   - `tests/fixtures/test-workspace/` - Sample files for testing
     - `sample.rs` - Rust test file with known symbols
     - `sample.ts` - TypeScript test file with known symbols

3. **Test Categories Defined**
   - Query execution (text search, structured queries, mixed queries)
   - Index management (missing, outdated, rebuild)
   - Timeout handling
   - Binary path validation
   - UI integration (sidebar, welcome view, results)
   - LSP communication
   - Multi-workspace support
   - Error recovery
   - Performance

## Next Steps for Testing

### Phase 1: Implement Core Integration Tests (Priority 1)

```bash
# Tests to implement first
tests/integration/
├── query-execution.test.ts     # Test query parsing and execution
├── error-handling.test.ts      # Test all error scenarios
└── index-management.test.ts    # Test indexing workflows
```

**Estimated effort**: 2-3 days

Key tests:
- [x] Test text search fallback ("Error" → old parser)
- [ ] Test structured queries ("kind:function" → new parser)
- [ ] Test query execution failures → Rebuild Index prompt
- [ ] Test timeout errors → Increase Timeout prompt
- [ ] Test binary not found → Open Settings prompt

### Phase 2: UI and LSP Tests (Priority 2)

```bash
tests/integration/
├── ui-integration.test.ts      # Test sidebar, icons, navigation
└── lsp-communication.test.ts   # Test LSP protocol
```

**Estimated effort**: 2 days

Key tests:
- [ ] Test search icon triggers input dialog
- [ ] Test result navigation opens correct file/line
- [ ] Test LSP server starts on activation
- [ ] Test LSP request/response format

### Phase 3: Advanced Scenarios (Priority 3)

```bash
tests/integration/
├── multi-workspace.test.ts     # Test multi-root workspaces
├── performance.test.ts         # Benchmark indexing and search
└── error-recovery.test.ts      # Test crash recovery
```

**Estimated effort**: 2-3 days

### Phase 4: Unit Tests (Priority 4)

```bash
tests/unit/
├── parsers.test.ts            # Test query parameter parsing
├── config.test.ts             # Test configuration handling
└── conversion.test.ts         # Test result conversion
```

**Estimated effort**: 1-2 days

## Testing Commands

```bash
# Run all tests
npm test

# Run specific test file
npx mocha -r ts-node/register tests/integration/search.test.ts

# Run with debug logging
RUST_LOG=debug npm test

# Generate coverage report (after setup)
npm run test:coverage
```

## CI/CD Integration

### Recommended CI Setup

```yaml
# .github/workflows/test-extension.yml
name: Test VSCode Extension

on: [push, pull_request]

jobs:
  test:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
        vscode-version: [stable, insiders]

    runs-on: ${{ matrix.os }}

    steps:
      - uses: actions/checkout@v2

      - name: Setup Node
        uses: actions/setup-node@v2
        with:
          node-version: '18'

      - name: Setup Rust
        uses: actions-rs/toolchain@v1
        with:
          toolchain: stable

      - name: Build sqry
        run: cargo build --release

      - name: Install sqry
        run: cargo install --path sqry-cli --force

      - name: Install extension dependencies
        working-directory: tools/sqry-vscode
        run: npm install

      - name: Compile extension
        working-directory: tools/sqry-vscode
        run: npm run compile

      - name: Run tests
        working-directory: tools/sqry-vscode
        run: npm test
```

## Test Coverage Goals

| Component | Target Coverage | Current | Status |
|-----------|----------------|---------|--------|
| Query Execution | 90% | 0% | ❌ Not started |
| Error Handling | 80% | 0% | ❌ Not started |
| Index Management | 80% | 0% | ❌ Not started |
| LSP Communication | 70% | 0% | ❌ Not started |
| UI Integration | 60% | 0% | ❌ Not started |
| **Overall** | **80%** | **0%** | ❌ **Not started** |

## Known Issues to Test For

1. **Index Version Compatibility**
   - Old index format may not work with new LSP
   - Test: Detect and auto-rebuild when version mismatch

2. **Concurrent Query Handling**
   - Multiple searches in quick succession
   - Test: Queue or cancel previous requests

3. **Large Repository Performance**
   - Indexing 10k+ files
   - Test: Should complete within reasonable time

4. **LSP Crash Recovery**
   - What happens if LSP process dies
   - Test: Should restart gracefully

5. **Multi-Workspace Edge Cases**
   - Searching across workspace boundaries
   - Test: Correct workspace resolution

## Success Criteria

Tests are considered complete when:
- ✅ All Phase 1 tests passing
- ✅ CI running tests on every PR
- ✅ Coverage >80% on core functionality
- ✅ No flaky tests (>99% pass rate)
- ✅ Tests complete in <5 minutes total
- ✅ Documentation complete and up-to-date

## Resources

- [VSCode Extension Testing Guide](https://code.visualstudio.com/api/working-with-extensions/testing-extension)
- [Mocha Documentation](https://mochajs.org/)
- [Chai Assertions](https://www.chaijs.com/)
- Test fixtures: `tests/fixtures/test-workspace/`
- Test documentation: `tests/README.md`
- Full test plan: `INTEGRATION_TESTS.md`

## Notes for Reviewers

When reviewing test PRs, ensure:
1. Test names clearly describe what is being tested
2. Tests are isolated (no dependencies between tests)
3. Appropriate timeouts are set for async operations
4. Test fixtures are documented
5. Both happy path and error scenarios are covered
6. Mock data is realistic
7. Cleanup happens in teardown hooks
