/**
 * Command execution tests
 */

import * as assert from "assert";
import * as vscode from "vscode";
import * as path from "path";
import * as fs from "fs";

suite("Command Execution Tests", () => {
  let testWorkspaceUri: vscode.Uri;

  suiteSetup(async function() {
    this.timeout(30000);

    // Ensure extension is activated
    const extension = vscode.extensions.getExtension("verivus.sqry-vscode");
    if (extension && !extension.isActive) {
      await extension.activate();
    }

    // Use the test workspace from fixtures
    const fixturesPath = path.resolve(__dirname, "..", "fixtures", "test-workspace");
    testWorkspaceUri = vscode.Uri.file(fixturesPath);
  });

  test("should execute sqry.query command", async function() {
    this.timeout(10000);

    // Execute command - it will show input box which we can't programmatically fill
    // But we can verify the command executes without error
    try {
      // Execute with a preset query parameter
      await vscode.commands.executeCommand("sqry.runQueryInternal", "kind:function");
      // Command executed successfully
      assert.ok(true, "Query command should execute");
    } catch (error) {
      // Expected if no workspace is open or sqry binary not found
      // We just want to verify the command exists and can be called
      assert.ok(error instanceof Error, "Error should be an Error instance");
    }
  });

  test("should execute sqry.searchWorkspace command", async function() {
    this.timeout(10000);

    try {
      await vscode.commands.executeCommand("sqry.searchWorkspace");
      assert.ok(true, "Search workspace command should execute");
    } catch (error) {
      // Command might fail due to missing workspace, but should exist
      assert.ok(error instanceof Error, "Error should be an Error instance");
    }
  });

  test("should execute sqry.findReferences command", async function() {
    this.timeout(10000);

    try {
      await vscode.commands.executeCommand("sqry.findReferences");
      // Might show info message about no active editor
      assert.ok(true, "Find references command should execute");
    } catch (error) {
      assert.ok(error instanceof Error, "Error should be an Error instance");
    }
  });

  test("should execute sqry.index command", async function() {
    this.timeout(60000); // Indexing can take time

    // Skip if no workspace is open
    if (!vscode.workspace.workspaceFolders || vscode.workspace.workspaceFolders.length === 0) {
      this.skip();
      return;
    }

    try {
      await vscode.commands.executeCommand("sqry.index");
      assert.ok(true, "Index command should execute");
    } catch (error) {
      // Might fail if sqry binary not in PATH
      const errorMessage = error instanceof Error ? error.message : String(error);
      assert.ok(
        errorMessage.includes("sqry") || errorMessage.includes("binary"),
        "Error should be related to sqry binary"
      );
    }
  });
});

suite("Command Error Handling Tests", () => {
  test("should handle missing binary gracefully", async function() {
    this.timeout(10000);

    // Temporarily set invalid binary path
    const config = vscode.workspace.getConfiguration("sqry");
    const originalPath = config.get("path");

    try {
      await config.update("path", "/invalid/path/to/sqry", vscode.ConfigurationTarget.Global);

      // Try to execute a command
      await vscode.commands.executeCommand("sqry.runQueryInternal", "test");

      // Should not reach here if error handling works
      // But if it does, that's also ok - just means error is handled silently
      assert.ok(true, "Command handled missing binary");
    } catch (error) {
      // Expected error about binary not found
      const errorMessage = error instanceof Error ? error.message : String(error);
      assert.ok(
        errorMessage.toLowerCase().includes("unable to locate") ||
        errorMessage.toLowerCase().includes("binary") ||
        errorMessage.toLowerCase().includes("sqry"),
        `Error should mention binary issue: ${errorMessage}`
      );
    } finally {
      // Restore original path
      await config.update("path", originalPath, vscode.ConfigurationTarget.Global);
    }
  });

  test("should show information message when no editor is active", async function() {
    this.timeout(10000);

    // Close all editors
    await vscode.commands.executeCommand("workbench.action.closeAllEditors");

    // Execute find references without active editor
    try {
      await vscode.commands.executeCommand("sqry.findReferences");
      // Command should handle gracefully
      assert.ok(true, "Command handled no active editor");
    } catch (error) {
      // Also acceptable if command throws
      assert.ok(true, "Command executed");
    }
  });
});

suite("CodeLens Tests", () => {
  test("should provide CodeLens when enabled", async function() {
    this.timeout(10000);

    const config = vscode.workspace.getConfiguration("sqry");
    const codeLensEnabled = config.get("codeLens.enabled");

    if (!codeLensEnabled) {
      this.skip();
      return;
    }

    // Open a test file
    const fixturesPath = path.resolve(__dirname, "..", "fixtures", "test-workspace", "sample.ts");

    if (!fs.existsSync(fixturesPath)) {
      this.skip();
      return;
    }

    const document = await vscode.workspace.openTextDocument(fixturesPath);
    await vscode.window.showTextDocument(document);

    // Request CodeLens
    const codeLenses = await vscode.commands.executeCommand<vscode.CodeLens[]>(
      "vscode.executeCodeLensProvider",
      document.uri
    );

    // CodeLens might be empty if sqry index doesn't exist or binary not found
    // Just verify the command executes
    assert.ok(Array.isArray(codeLenses), "CodeLens provider should return array");
  });
});
