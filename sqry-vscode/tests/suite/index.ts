/**
 * Test suite index - loads and runs all test files
 */

import * as path from 'path';
import Mocha from 'mocha';
import { glob } from 'glob';

export async function run(): Promise<void> {
  // Create the mocha test
  const mocha = new Mocha({
    ui: 'tdd',
    color: true,
    timeout: 30000, // Default 30 second timeout for all tests
  });

  const testsRoot = path.resolve(__dirname, '..');

  // Find all test files in the suite directory only
  const files = await glob('suite/**/*.test.js', { cwd: testsRoot });

  // Add files to the test suite
  files.forEach(f => mocha.addFile(path.resolve(testsRoot, f)));

  // Return a promise that resolves when mocha finishes
  return new Promise((resolve, reject) => {
    try {
      // Run the mocha test
      mocha.run(failures => {
        if (failures > 0) {
          reject(new Error(`${failures} tests failed.`));
        } else {
          resolve();
        }
      });
    } catch (err) {
      console.error(err);
      reject(err);
    }
  });
}
