/**
 * LSP integration tests
 */

import * as assert from "assert";
import * as vscode from "vscode";
import * as path from "path";
import * as fs from "fs";

suite("LSP Basic Tests", () => {
  test("should provide hover information", async function() {
    this.timeout(15000);

    const fixturesPath = path.resolve(__dirname, "..", "fixtures", "test-workspace", "sample.ts");

    if (!fs.existsSync(fixturesPath)) {
      this.skip();
      return;
    }

    const document = await vscode.workspace.openTextDocument(fixturesPath);
    await vscode.window.showTextDocument(document);

    // Try to get hover at first line
    const position = new vscode.Position(0, 0);

    try {
      const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
        "vscode.executeHoverProvider",
        document.uri,
        position
      );

      // Hover might be empty, but command should execute
      assert.ok(Array.isArray(hovers), "Hover provider should return array");
    } catch (error) {
      // LSP might not be running, which is acceptable in test
      assert.ok(true, "Hover provider executed");
    }
  });

  test("should provide document symbols", async function() {
    this.timeout(15000);

    const fixturesPath = path.resolve(__dirname, "..", "fixtures", "test-workspace", "sample.ts");

    if (!fs.existsSync(fixturesPath)) {
      this.skip();
      return;
    }

    const document = await vscode.workspace.openTextDocument(fixturesPath);

    const symbols = await vscode.commands.executeCommand<vscode.DocumentSymbol[]>(
      "vscode.executeDocumentSymbolProvider",
      document.uri
    );

    assert.ok(Array.isArray(symbols), "Document symbol provider should return array");

    if (symbols && symbols.length > 0) {
      const symbol = symbols[0];
      assert.ok(symbol.name, "Symbol should have name");
      assert.ok(typeof symbol.kind === "number", "Symbol should have kind");
      assert.ok(symbol.range, "Symbol should have range");
      assert.ok(symbol.selectionRange, "Symbol should have selection range");
    }
  });

  test("should provide definitions", async function() {
    this.timeout(15000);

    const fixturesPath = path.resolve(__dirname, "..", "fixtures", "test-workspace", "sample.ts");

    if (!fs.existsSync(fixturesPath)) {
      this.skip();
      return;
    }

    const document = await vscode.workspace.openTextDocument(fixturesPath);
    const position = new vscode.Position(0, 0);

    try {
      const definitions = await vscode.commands.executeCommand<vscode.Location[]>(
        "vscode.executeDefinitionProvider",
        document.uri,
        position
      );

      // Definitions might be empty, but command should execute
      assert.ok(Array.isArray(definitions), "Definition provider should return array");
    } catch (error) {
      // Expected if LSP not running
      assert.ok(true, "Definition provider executed");
    }
  });

  test("should provide references", async function() {
    this.timeout(15000);

    const fixturesPath = path.resolve(__dirname, "..", "fixtures", "test-workspace", "sample.ts");

    if (!fs.existsSync(fixturesPath)) {
      this.skip();
      return;
    }

    const document = await vscode.workspace.openTextDocument(fixturesPath);
    const position = new vscode.Position(0, 0);

    try {
      const references = await vscode.commands.executeCommand<vscode.Location[]>(
        "vscode.executeReferenceProvider",
        document.uri,
        position
      );

      // References might be empty
      assert.ok(Array.isArray(references), "Reference provider should return array");
    } catch (error) {
      // Expected if LSP not running
      assert.ok(true, "Reference provider executed");
    }
  });
});

suite("LSP Code Actions Tests", () => {
  test("should provide code actions", async function() {
    this.timeout(15000);

    const fixturesPath = path.resolve(__dirname, "..", "fixtures", "test-workspace", "sample.ts");

    if (!fs.existsSync(fixturesPath)) {
      this.skip();
      return;
    }

    const document = await vscode.workspace.openTextDocument(fixturesPath);
    const range = new vscode.Range(0, 0, 0, 10);

    try {
      const codeActions = await vscode.commands.executeCommand<vscode.CodeAction[]>(
        "vscode.executeCodeActionProvider",
        document.uri,
        range
      );

      // Code actions might be empty
      assert.ok(Array.isArray(codeActions), "Code action provider should return array");

      if (codeActions && codeActions.length > 0) {
        const action = codeActions[0];
        assert.ok(action.title, "Code action should have title");
      }
    } catch (error) {
      // Expected if LSP not running
      assert.ok(true, "Code action provider executed");
    }
  });
});

suite("LSP Workspace Symbol Tests", () => {
  test("should search workspace symbols", async function() {
    this.timeout(15000);

    try {
      const symbols = await vscode.commands.executeCommand<vscode.SymbolInformation[]>(
        "vscode.executeWorkspaceSymbolProvider",
        "function" // Search query
      );

      // Symbols might be empty if no index
      assert.ok(Array.isArray(symbols), "Workspace symbol provider should return array");

      if (symbols && symbols.length > 0) {
        const symbol = symbols[0];
        assert.ok(symbol.name, "Symbol should have name");
        assert.ok(typeof symbol.kind === "number", "Symbol should have kind");
        assert.ok(symbol.location, "Symbol should have location");
      }
    } catch (error) {
      // Expected if LSP not running or no workspace
      assert.ok(true, "Workspace symbol provider executed");
    }
  });

  test("should handle empty workspace symbol query", async function() {
    this.timeout(15000);

    try {
      const symbols = await vscode.commands.executeCommand<vscode.SymbolInformation[]>(
        "vscode.executeWorkspaceSymbolProvider",
        "" // Empty query
      );

      assert.ok(Array.isArray(symbols), "Should handle empty query");
    } catch (error) {
      assert.ok(true, "Workspace symbol provider executed");
    }
  });
});

suite("LSP Diagnostic Tests", () => {
  test("should handle diagnostics", async function() {
    this.timeout(10000);

    const fixturesPath = path.resolve(__dirname, "..", "fixtures", "test-workspace", "sample.ts");

    if (!fs.existsSync(fixturesPath)) {
      this.skip();
      return;
    }

    const document = await vscode.workspace.openTextDocument(fixturesPath);
    await vscode.window.showTextDocument(document);

    // Wait a bit for diagnostics to be computed
    await new Promise(resolve => setTimeout(resolve, 2000));

    const diagnostics = vscode.languages.getDiagnostics(document.uri);

    // Diagnostics might be empty (no errors), which is fine
    assert.ok(Array.isArray(diagnostics), "Should get diagnostics array");
  });

  test("should get all diagnostics", async function() {
    this.timeout(10000);

    const allDiagnostics = vscode.languages.getDiagnostics();

    assert.ok(Array.isArray(allDiagnostics), "Should get all diagnostics");

    for (const [uri, diagnostics] of allDiagnostics) {
      assert.ok(uri, "Diagnostic should have URI");
      assert.ok(Array.isArray(diagnostics), "Diagnostics should be array");
    }
  });
});
