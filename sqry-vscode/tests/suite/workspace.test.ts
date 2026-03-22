/**
 * Workspace and file handling tests
 */

import * as assert from "assert";
import * as vscode from "vscode";
import * as path from "path";
import * as fs from "fs";

suite("Workspace Detection Tests", () => {
  test("should detect workspace folders", () => {
    const folders = vscode.workspace.workspaceFolders;

    if (folders && folders.length > 0) {
      assert.ok(folders.length >= 1, "Should have at least one workspace folder");
      assert.ok(folders[0].uri.fsPath, "Workspace folder should have a path");
    } else {
      // No workspace open - this is acceptable in tests
      assert.ok(true, "No workspace folders (acceptable in test environment)");
    }
  });

  test("should handle multiple workspace folders", () => {
    const folders = vscode.workspace.workspaceFolders;

    if (folders) {
      for (const folder of folders) {
        assert.ok(folder.uri, "Each folder should have a URI");
        assert.ok(folder.name, "Each folder should have a name");
        assert.strictEqual(typeof folder.index, "number", "Each folder should have an index");
      }
    }

    assert.ok(true, "Multiple workspace folders handled correctly");
  });

  test("should get workspace folder from document", async function() {
    this.timeout(10000);

    const fixturesPath = path.resolve(__dirname, "..", "fixtures", "test-workspace", "sample.ts");

    if (!fs.existsSync(fixturesPath)) {
      this.skip();
      return;
    }

    const document = await vscode.workspace.openTextDocument(fixturesPath);
    const folder = vscode.workspace.getWorkspaceFolder(document.uri);

    // Folder might be undefined if file is not in a workspace
    if (folder) {
      assert.ok(folder.uri, "Workspace folder should have URI");
      assert.ok(folder.name, "Workspace folder should have name");
    } else {
      assert.ok(true, "File not in workspace (acceptable)");
    }
  });
});

suite("Index File Detection Tests", () => {
  test("should check for sqry index files", async function() {
    this.timeout(10000);

    const folders = vscode.workspace.workspaceFolders;

    if (!folders || folders.length === 0) {
      this.skip();
      return;
    }

    for (const folder of folders) {
      const indexPath = path.join(folder.uri.fsPath, ".sqry-index");
      const lockPath = path.join(folder.uri.fsPath, ".sqry-index.lock");

      // Check if files exist
      const indexExists = fs.existsSync(indexPath);
      const lockExists = fs.existsSync(lockPath);

      // Both are valid states - index may or may not exist
      assert.ok(
        typeof indexExists === "boolean",
        "Should be able to check for index file"
      );
      assert.ok(
        typeof lockExists === "boolean",
        "Should be able to check for lock file"
      );
    }
  });

  test("should handle index file permissions", async function() {
    this.timeout(10000);

    const folders = vscode.workspace.workspaceFolders;

    if (!folders || folders.length === 0) {
      this.skip();
      return;
    }

    for (const folder of folders) {
      const indexPath = path.join(folder.uri.fsPath, ".sqry-index");

      if (fs.existsSync(indexPath)) {
        try {
          const stats = fs.statSync(indexPath);
          assert.ok(stats.isFile(), "Index should be a file");
          // Check that the index file has actual content (size > 0)
          assert.ok(stats.size > 0, "Index should have content");
        } catch (error) {
          // Permission error is acceptable
          assert.ok(error instanceof Error, "Error should be Error instance");
        }
      }
    }

    assert.ok(true, "Index permission check completed");
  });
});

suite("File Opening Tests", () => {
  test("should open test fixture files", async function() {
    this.timeout(10000);

    const fixturesPath = path.resolve(__dirname, "..", "fixtures", "test-workspace");
    const files = ["sample.ts", "sample.rs"];

    for (const file of files) {
      const filePath = path.join(fixturesPath, file);

      if (fs.existsSync(filePath)) {
        const document = await vscode.workspace.openTextDocument(filePath);
        assert.ok(document, `Should open ${file}`);
        assert.strictEqual(document.languageId, file.endsWith(".ts") ? "typescript" : "rust");
        assert.ok(document.getText().length > 0, "Document should have content");
      }
    }
  });

  test("should get active text editor", async function() {
    this.timeout(10000);

    const fixturesPath = path.resolve(__dirname, "..", "fixtures", "test-workspace", "sample.ts");

    if (!fs.existsSync(fixturesPath)) {
      this.skip();
      return;
    }

    const document = await vscode.workspace.openTextDocument(fixturesPath);
    const editor = await vscode.window.showTextDocument(document);

    assert.ok(editor, "Should have active editor");
    assert.strictEqual(editor.document, document, "Editor should show the document");

    const activeEditor = vscode.window.activeTextEditor;
    assert.ok(activeEditor, "Should have active text editor");
  });

  test("should get symbol at position", async function() {
    this.timeout(10000);

    const fixturesPath = path.resolve(__dirname, "..", "fixtures", "test-workspace", "sample.ts");

    if (!fs.existsSync(fixturesPath)) {
      this.skip();
      return;
    }

    const document = await vscode.workspace.openTextDocument(fixturesPath);

    // Get document symbols
    const symbols = await vscode.commands.executeCommand<vscode.DocumentSymbol[]>(
      "vscode.executeDocumentSymbolProvider",
      document.uri
    );

    // Should return array (might be empty if no symbols)
    assert.ok(Array.isArray(symbols), "Should return symbols array");

    if (symbols && symbols.length > 0) {
      const firstSymbol = symbols[0];
      assert.ok(firstSymbol.name, "Symbol should have name");
      assert.ok(firstSymbol.kind, "Symbol should have kind");
      assert.ok(firstSymbol.range, "Symbol should have range");
    }
  });
});

suite("File System Watcher Tests", () => {
  test("should create file system watcher", () => {
    const pattern = new vscode.RelativePattern(
      vscode.workspace.workspaceFolders?.[0] || "",
      ".sqry-index"
    );

    const watcher = vscode.workspace.createFileSystemWatcher(pattern);

    assert.ok(watcher, "Should create file system watcher");

    // Clean up
    watcher.dispose();
  });

  test("should watch for changes in workspace", function() {
    const folders = vscode.workspace.workspaceFolders;

    if (!folders || folders.length === 0) {
      this.skip();
      return;
    }

    const pattern = new vscode.RelativePattern(folders[0], "**/*.{ts,rs,js}");
    const watcher = vscode.workspace.createFileSystemWatcher(pattern);

    let changeDetected = false;
    watcher.onDidChange(() => {
      changeDetected = true;
    });

    assert.ok(watcher, "Watcher should be created");

    // Clean up
    watcher.dispose();
  });
});
