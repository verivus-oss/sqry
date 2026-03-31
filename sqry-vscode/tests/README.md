# sqry-vscode Tests

This directory contains unit and integration tests for the sqry VS Code extension.

## Test Structure

```
tests/
├── fixtures/           # Test data and mock workspaces
│   └── test-workspace/ # Sample files for integration tests
├── suite/              # Integration tests (run in VS Code environment)
│   ├── index.ts       # Test suite loader
│   └── *.test.ts      # Integration test files
├── runTests.ts         # VS Code test runner
├── *.test.ts           # Unit tests (parsers, config, utilities)
└── README.md           # This file
```

## Running Tests

### Unit Tests (Fast, No VS Code Required)

```bash
npm run test:unit
```

Runs unit tests for:
- Configuration resolution
- Query parsers
- Index queue
- Utility functions

**Time**: ~100ms | **Requirements**: Node.js only

### Integration Tests (Requires VS Code)

```bash
npm run test:integration
```

Runs integration tests for:
- Extension activation
- LSP communication
- Search commands
- Index management
- UI components

**Time**: ~30s | **Requirements**: VS Code installed, sqry binary in PATH

### All Tests

```bash
npm test  # Runs unit tests only by default
```

**Note**: Integration tests must be run separately with `npm run test:integration` because they require the VS Code Extension Test framework.

### Debug Tests in VS Code

1. Open the extension workspace in VS Code
2. Press `F5` or use "Run > Start Debugging"
3. Choose "Extension Tests" from the launch configuration dropdown
4. Set breakpoints in test files
5. Tests will run in a new VS Code window

## Writing Tests

### Unit Tests

Create files matching `tests/*.test.ts`:

```typescript
import { expect } from "chai";
import { myFunction } from "../src/myModule";

describe("myModule", () => {
  it("should do something", () => {
    const result = myFunction("input");
    expect(result).to.equal("expected");
  });
});
```

### Integration Tests

Create files in `tests/suite/*.test.ts`:

```typescript
import * as assert from "assert";
import * as vscode from "vscode";

suite("My Feature Tests", () => {
  test("should activate extension", async () => {
    const ext = vscode.extensions.getExtension("verivuslabs.sqry-vscode");
    assert.ok(ext, "Extension should be installed");
    await ext.activate();
    assert.ok(ext.isActive, "Extension should be active");
  });
});
```

## Test Coverage Goals

| Component | Target | Current | Status |
|-----------|--------|---------|--------|
| Unit tests | 80% | ~60% | 🟡 Partial |
| Integration tests | 70% | 0% | 🔴 Not started |
| Overall | 75% | ~30% | 🟡 In progress |

## CI/CD Integration

Tests are designed to run in GitHub Actions:

```yaml
- name: Run unit tests
  run: npm run test:unit

- name: Run integration tests
  uses: GabrielBB/xvfb-action@v1  # Virtual display for headless VS Code
  with:
    run: npm run test:integration
```

## Troubleshooting

### "Cannot find module 'vscode'"

This error appears when running integration tests with regular mocha instead of VS Code Test framework. Use `npm run test:integration` instead of `npm test`.

### Tests timeout

Integration tests may take longer on first run while VS Code downloads. Increase timeout in `tests/suite/index.ts`:

```typescript
const mocha = new Mocha({
  timeout: 60000, // 60 seconds
});
```

### sqry binary not found

Integration tests require `sqry` to be in your PATH. Install it first:

```bash
cd ../..  # Go to repo root
cargo install --path sqry-cli --force
```

## References

- [VS Code Extension Testing Guide](https://code.visualstudio.com/api/working-with-extensions/testing-extension)
- [Mocha Documentation](https://mochajs.org/)
- [Chai Assertions](https://www.chaijs.com/)
- [@vscode/test-electron](https://github.com/microsoft/vscode-test)
