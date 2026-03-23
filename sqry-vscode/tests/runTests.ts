/**
 * Test runner for VS Code extension integration tests
 * Uses @vscode/test-electron to run tests in VS Code environment
 */

import * as path from 'path';
import { runTests } from '@vscode/test-electron';

async function main() {
  try {
    // Unset ELECTRON_RUN_AS_NODE environment variable if set
    // This prevents VS Code from running as Node.js instead of Electron
    // (common issue when running from VS Code's integrated terminal)
    if (process.env.ELECTRON_RUN_AS_NODE) {
      delete process.env.ELECTRON_RUN_AS_NODE;
    }

    // The folder containing the Extension Manifest package.json
    // Passed to `--extensionDevelopmentPath`
    const extensionDevelopmentPath = path.resolve(__dirname, '../../');

    // The path to the extension test script
    // Passed to --extensionTestsPath
    const extensionTestsPath = path.resolve(__dirname, './suite/index');

    // Download VS Code, unzip it and run the integration test
    await runTests({
      extensionDevelopmentPath,
      extensionTestsPath,
      launchArgs: [
        '--disable-extensions', // Disable other extensions for clean test environment
        '--disable-workspace-trust', // Don't prompt for workspace trust
      ],
    });
  } catch (err) {
    console.error('Failed to run tests:', err);
    process.exit(1);
  }
}

main();
