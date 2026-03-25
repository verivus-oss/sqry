import * as vscode from "vscode";
import { readSettings } from "./config";
import { downloadBinary, findExistingBinary, getBinaryVersion, detectPlatform } from "./binaryDownloader";
import { SqryCodeLensProvider } from "./codeLens";
import { SearchPanel } from "./searchPanel";
import { SqryClient } from "./sqryClient";
import { addToHistory, clearHistory, formatRelativeTime, SearchHistoryEntry } from "./searchHistory";
import { SqryCodeActionProvider } from "./codeActions";
import { SqryDiagnosticsProvider } from "./diagnosticsProvider";
import { SqryHoverProvider } from "./hoverProvider";
import { SqryGraphPanel, GraphNode, GraphEdge } from "./graphPanel";
import { SqryStatusBar } from "./statusBar";
import { exportAsJson, exportAsMarkdown, exportAsCsv } from "./exportResults";
import { AutoIndexManager } from "./autoIndex";

const HISTORY_STATE_KEY = "sqry.searchHistory";

let client: SqryClient | undefined;
let outputChannel: vscode.OutputChannel | undefined;
let searchPanel: SearchPanel | undefined;
let statusBar: SqryStatusBar | undefined;
let diagnosticsProvider: SqryDiagnosticsProvider | undefined;

export async function activate(
  context: vscode.ExtensionContext,
): Promise<void> {
  outputChannel = vscode.window.createOutputChannel("Sqry");
  context.subscriptions.push(outputChannel);

  client = new SqryClient(outputChannel);
  context.subscriptions.push(client);

  const initialized = await initializeClient(context, client, outputChannel);
  if (!initialized) {
    return;
  }

  searchPanel = new SearchPanel(context, client, outputChannel);
  context.subscriptions.push(searchPanel);

  const statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
  statusBar = new SqryStatusBar(statusBarItem, outputChannel ?? null);
  context.subscriptions.push(statusBar);

  // Diagnostics provider — publishes findings to VS Code Problems panel
  const diagCollection = vscode.languages.createDiagnosticCollection("sqry");
  diagnosticsProvider = new SqryDiagnosticsProvider(diagCollection, client, outputChannel);
  context.subscriptions.push(diagnosticsProvider);

  // Code action provider — quick fixes for sqry diagnostics
  const codeActionProvider = new SqryCodeActionProvider();
  context.subscriptions.push(
    vscode.languages.registerCodeActionsProvider("*", codeActionProvider, {
      providedCodeActionKinds: SqryCodeActionProvider.providedCodeActionKinds,
    }),
  );

  // Hover provider — show sqry caller/callee counts in tooltips
  if (vscode.workspace.getConfiguration("sqry").get<boolean>("hover.enabled", true)) {
    const hoverProvider = new SqryHoverProvider(client, outputChannel ?? null);
    context.subscriptions.push(
      vscode.languages.registerHoverProvider("*", hoverProvider),
      hoverProvider,
    );
  }

  // Refresh diagnostics when a file is opened
  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument((doc) => {
      const workspace = doc.uri.scheme === "file"
        ? vscode.workspace.getWorkspaceFolder(doc.uri)
        : undefined;
      if (workspace && diagnosticsProvider) {
        void diagnosticsProvider.refreshForFile(doc.uri, workspace);
      }
    }),
  );

  // Clear diagnostics when a file is closed
  context.subscriptions.push(
    vscode.workspace.onDidCloseTextDocument((doc) => {
      diagnosticsProvider?.clearFile(doc.uri);
    }),
  );

  // Refresh status bar when config changes (e.g., binary path changed)
  context.subscriptions.push(
    client.onDidChangeConfig(async () => {
      const workspace = getActiveWorkspaceFolder();
      if (workspace && client) {
        try {
          const status = await client.getIndexStatus(workspace);
          statusBar?.update(status);
        } catch {
          // Config change may cause temporary LSP unavailability
        }
      }
    }),
  );

  const runQuery = async (query?: string): Promise<void> => {
    const activeClient = client;
    if (!activeClient) {
      return;
    }

    let actualQuery = query;
    if (!actualQuery) {
      actualQuery = await vscode.window.showInputBox({
        prompt: "Enter a sqry semantic query",
        placeHolder: "kind:function AND name:parse, callers:process, returns:Result",
      });
    }

    if (!actualQuery) {
      return;
    }

    const workspace = getActiveWorkspaceFolder();

    await vscode.window.withProgress(
      {
        location: vscode.ProgressLocation.Window,
        title: `sqry: ${actualQuery}`,
      },
      async () => {
        try {
          const result = await activeClient.runQuery(actualQuery, workspace);
          searchPanel?.update(result);
          const history = context.workspaceState.get<SearchHistoryEntry[]>(HISTORY_STATE_KEY, []);
          await context.workspaceState.update(HISTORY_STATE_KEY, addToHistory(history, actualQuery));
        } catch (error) {
          await handleError(error);
        }
      },
    );
  };

  const codeLensProvider = new SqryCodeLensProvider(client);
  context.subscriptions.push(
    vscode.languages.registerCodeLensProvider(
      { scheme: "file" },
      codeLensProvider,
    ),
    codeLensProvider,
    // Note: When triggered from tree view menu icons, VSCode passes context objects
    // instead of undefined. We need to validate that preset is actually a string.
    vscode.commands.registerCommand("sqry.query", (preset?: unknown) =>
      runQuery(typeof preset === "string" ? preset : undefined),
    ),
    vscode.commands.registerCommand("sqry.runQueryInternal", (preset: string) =>
      runQuery(preset),
    ),
    vscode.commands.registerCommand("sqry.searchWorkspace", async () => {
      const activeClient = client;
      if (!activeClient) {
        return;
      }

      const searchTerm = await vscode.window.showInputBox({
        prompt: "Enter a text pattern for sqry search",
        placeHolder: "function name, async fetch, error handling",
      });

      if (!searchTerm) {
        return;
      }

      const workspace = getActiveWorkspaceFolder();

      await vscode.window.withProgress(
        {
          location: vscode.ProgressLocation.Window,
          title: `sqry search: ${searchTerm}`,
        },
        async () => {
          try {
            const result = await activeClient.runSearch(searchTerm, workspace);
            searchPanel?.update(result);
            const history = context.workspaceState.get<SearchHistoryEntry[]>(HISTORY_STATE_KEY, []);
            await context.workspaceState.update(HISTORY_STATE_KEY, addToHistory(history, searchTerm));
          } catch (error) {
            await handleError(error);
          }
        },
      );
    }),
    vscode.commands.registerCommand("sqry.findReferences", async () => {
      const activeClient = client;
      if (!activeClient) {
        return;
      }

      const editor = vscode.window.activeTextEditor;
      if (!editor) {
        void vscode.window.showInformationMessage(
          "sqry: Open a file and place the cursor on a symbol to find references.",
        );
        return;
      }

      const symbolName = await pickSymbolName(editor.document, editor.selection.active);
      if (!symbolName) {
        void vscode.window.showInformationMessage(
          "sqry: Unable to detect symbol under cursor.",
        );
        return;
      }

      const relationItems: Array<vscode.QuickPickItem & { value: string }> = [
        { label: "Callers", value: "callers" },
        { label: "Callees", value: "callees" },
        { label: "References", value: "references" },
        { label: "Returns", value: "returns" },
      ];

      const relation = await vscode.window.showQuickPick(relationItems, {
        placeHolder: `Select a relation for ${symbolName}`,
      });

      if (!relation) {
        return;
      }

      const workspace = getActiveWorkspaceFolder();
      const expression = `${relation.value}:${symbolName}`;

      await vscode.window.withProgress(
        {
          location: vscode.ProgressLocation.Window,
          title: `sqry: ${expression}`,
        },
        async () => {
          try {
            const result = await activeClient.runQuery(expression, workspace);
            searchPanel?.update(result);
          } catch (error) {
            await handleError(error);
          }
        },
      );
    }),
    vscode.commands.registerCommand("sqry.index", async () => {
      const activeClient = client;
      if (!activeClient) {
        return;
      }
      const folders = getAllWorkspaceFolders();
      let workspace: vscode.WorkspaceFolder | undefined;
      if (folders.length > 1) {
        workspace = await pickWorkspaceFolder("Select workspace folder to index");
      } else {
        workspace = getActiveWorkspaceFolder();
      }
      if (!workspace) {
        if (folders.length === 0) {
          void vscode.window.showWarningMessage(
            "sqry: No workspace folder detected. Open a folder before indexing.",
          );
        }
        return;
      }

      // Progress is handled by LSP WorkDoneProgress notifications from the server.
      // The vscode-languageclient automatically displays these in the notification area.
      try {
        await activeClient.runIndex(workspace);
        // Success message is sent by LSP server via window/showMessage with symbol counts
        await refreshIndexStats(activeClient, workspace);
      } catch (error) {
        await handleError(error);
      }
    }),
    vscode.commands.registerCommand("sqry.refreshStats", async () => {
      const activeClient = client;
      if (!activeClient || !searchPanel) {
        return;
      }
      const workspace = getActiveWorkspaceFolder();
      if (!workspace) {
        void vscode.window.showWarningMessage(
          "sqry: No workspace folder detected.",
        );
        return;
      }
      try {
        const status = await activeClient.getIndexStatus(workspace);
        searchPanel.setIndexStatus(status);
        statusBar?.update(status);
        outputChannel?.appendLine(`[sqry] Refreshed index stats: ${status.symbol_count} symbols, ${status.file_count} files`);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        outputChannel?.appendLine(`[sqry] Failed to refresh stats: ${message}`);
        void vscode.window.showWarningMessage(`sqry: Failed to refresh stats: ${message}`);
      }
    }),
    vscode.commands.registerCommand("sqry.showOutput", () => {
      outputChannel?.show(true);
    }),
    vscode.commands.registerCommand("sqry.clearResults", () => {
      if (searchPanel) {
        searchPanel.clearResults();
      }
    }),
    vscode.commands.registerCommand("sqry.searchHistory", async () => {
      const history = context.workspaceState.get<SearchHistoryEntry[]>(HISTORY_STATE_KEY, []);
      if (history.length === 0) {
        void vscode.window.showInformationMessage("No search history yet");
        return;
      }

      const items: Array<vscode.QuickPickItem & { value: string }> = history.map((entry) => ({
        label: entry.query,
        description: formatRelativeTime(entry.timestamp),
        value: entry.query,
      }));

      items.push({
        label: "$(trash) Clear History",
        description: "",
        value: "__clear__",
      });

      const selected = await vscode.window.showQuickPick(items, {
        placeHolder: "Select a previous search to re-run",
      });

      if (!selected) {
        return;
      }

      if (selected.value === "__clear__") {
        await context.workspaceState.update(HISTORY_STATE_KEY, clearHistory());
        void vscode.window.showInformationMessage("Search history cleared");
        return;
      }

      await runQuery(selected.value);
    }),
    vscode.commands.registerCommand("sqry.scanWorkspace", async () => {
      if (!diagnosticsProvider) {
        return;
      }
      const folders = getAllWorkspaceFolders();
      let workspace: vscode.WorkspaceFolder | undefined;
      if (folders.length > 1) {
        workspace = await pickWorkspaceFolder("Select workspace folder to scan");
      } else {
        workspace = getActiveWorkspaceFolder();
      }
      if (!workspace) {
        return;
      }
      await vscode.window.withProgress(
        {
          location: vscode.ProgressLocation.Notification,
          title: "sqry: Scanning workspace...",
        },
        () => diagnosticsProvider!.scanWorkspace(workspace!),
      );
    }),
    vscode.commands.registerCommand("sqry.restartLsp", async () => {
      const activeClient = client;
      if (!activeClient) {
        return;
      }
      outputChannel?.appendLine("[sqry] Restarting language server...");
      try {
        await activeClient.restart();
        outputChannel?.appendLine("[sqry] Language server restarted successfully");
        void vscode.window.showInformationMessage("sqry: Language server restarted.");
        // Refresh stats after restart
        const workspace = getActiveWorkspaceFolder();
        if (workspace && searchPanel) {
          const status = await activeClient.getIndexStatus(workspace);
          searchPanel.setIndexStatus(status);
          statusBar?.update(status);
        }
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        outputChannel?.appendLine(`[sqry] Failed to restart language server: ${message}`);
        void vscode.window.showErrorMessage(`sqry: Failed to restart language server: ${message}`);
      }
    }),
    vscode.commands.registerCommand("sqry.rebuildIndex", async () => {
      const activeClient = client;
      if (!activeClient) {
        return;
      }
      const workspace = getActiveWorkspaceFolder();
      if (!workspace) {
        void vscode.window.showWarningMessage("sqry: No workspace folder detected.");
        return;
      }
      try {
        await activeClient.runIndex(workspace);
        await refreshIndexStats(activeClient, workspace);
      } catch (error) {
        await handleError(error);
      }
    }),
    vscode.commands.registerCommand(
      "sqry.loadMore",
      async (
        itemType: "symbols" | "files" | "languageFiles" | "crossLanguage",
        nextOffset: number,
        language?: string,
      ) => {
        if (!searchPanel) {
          return;
        }
        const languageSuffix = language ? ` (${language})` : "";
        outputChannel?.appendLine(`[sqry] Load more ${itemType}${languageSuffix} from offset ${nextOffset}`);
        await searchPanel.loadMore(itemType, nextOffset, language);
      },
    ),
    vscode.commands.registerCommand("sqry.showCallGraph", async () => {
      const activeClient = client;
      if (!activeClient) {
        return;
      }

      // Get symbol name — from editor selection or prompt
      const editor = vscode.window.activeTextEditor;
      let symbolName: string | undefined;

      if (editor) {
        const wordRange = editor.document.getWordRangeAtPosition(editor.selection.active);
        if (wordRange) {
          symbolName = editor.document.getText(wordRange);
        }
      }

      if (!symbolName) {
        symbolName = await vscode.window.showInputBox({
          prompt: "Enter symbol name for call graph",
          placeHolder: "function or method name",
        });
      }

      if (!symbolName) {
        return;
      }

      const workspace = getActiveWorkspaceFolder();
      const graphPanel = SqryGraphPanel.createOrShow(context.extensionUri, "callGraph");

      try {
        // Fetch callers and callees
        const [callerResult, calleeResult] = await Promise.all([
          activeClient.runQuery(`callers:${symbolName}`, workspace),
          activeClient.runQuery(`callees:${symbolName}`, workspace),
        ]);

        // Build graph nodes and edges
        const nodes: GraphNode[] = [{ id: symbolName, label: symbolName, kind: "target" }];
        const edges: GraphEdge[] = [];

        for (const caller of (callerResult?.symbols ?? [])) {
          const callerId = `caller:${caller.name}`;
          nodes.push({
            id: callerId,
            label: caller.name,
            kind: caller.kind,
            file: caller.filePath,
            line: caller.startLine ? caller.startLine - 1 : undefined,
            language: caller.language,
          });
          edges.push({ source: callerId, target: symbolName });
        }

        for (const callee of (calleeResult?.symbols ?? [])) {
          const calleeId = `callee:${callee.name}`;
          nodes.push({
            id: calleeId,
            label: callee.name,
            kind: callee.kind,
            file: callee.filePath,
            line: callee.startLine ? callee.startLine - 1 : undefined,
            language: callee.language,
          });
          edges.push({ source: symbolName, target: calleeId });
        }

        graphPanel.sendGraphData(nodes, edges);
      } catch (error) {
        graphPanel.sendError(error instanceof Error ? error.message : String(error));
      }
    }),
    vscode.commands.registerCommand("sqry.showDependencies", async () => {
      const activeClient = client;
      if (!activeClient) {
        return;
      }

      const workspace = getActiveWorkspaceFolder();
      if (!workspace) {
        return;
      }

      const graphPanel = SqryGraphPanel.createOrShow(context.extensionUri, "dependencies");

      try {
        const result = await activeClient.listCrossLanguageRelations(workspace);

        const nodeSet = new Set<string>();
        const nodes: Array<{ id: string; label: string; language?: string }> = [];
        const edges: Array<{ source: string; target: string; label?: string }> = [];

        for (const rel of (result?.relations ?? [])) {
          if (!nodeSet.has(rel.from_symbol)) {
            nodeSet.add(rel.from_symbol);
            nodes.push({ id: rel.from_symbol, label: rel.from_symbol, language: rel.from_language });
          }
          if (!nodeSet.has(rel.to_symbol)) {
            nodeSet.add(rel.to_symbol);
            nodes.push({ id: rel.to_symbol, label: rel.to_symbol, language: rel.to_language });
          }
          edges.push({ source: rel.from_symbol, target: rel.to_symbol, label: rel.relation_type });
        }

        graphPanel.sendGraphData(nodes, edges);
      } catch (error) {
        graphPanel.sendError(error instanceof Error ? error.message : String(error));
      }
    }),
    vscode.commands.registerCommand("sqry.filterResults", async () => {
      if (!searchPanel) {
        return;
      }

      const languages = searchPanel.getAvailableLanguages();
      const kinds = searchPanel.getAvailableKinds();

      if (languages.length === 0 && kinds.length === 0) {
        void vscode.window.showInformationMessage(
          "sqry: No search results to filter. Run a search first.",
        );
        return;
      }

      // Step 1: language filter
      const langPicks: vscode.QuickPickItem[] = languages.map((l) => ({ label: l, picked: true }));
      const selectedLangs = await vscode.window.showQuickPick(langPicks, {
        canPickMany: true,
        placeHolder: "Filter by language (select all to keep, cancel to abort)",
      });
      if (!selectedLangs) {
        return;
      }

      // Step 2: kind filter
      const kindPicks: vscode.QuickPickItem[] = kinds.map((k) => ({ label: k, picked: true }));
      const selectedKinds = await vscode.window.showQuickPick(kindPicks, {
        canPickMany: true,
        placeHolder: "Filter by kind (select all to keep, cancel to abort)",
      });
      if (!selectedKinds) {
        return;
      }

      searchPanel.setFilters({
        languages: new Set(selectedLangs.map((l) => l.label)),
        kinds: new Set(selectedKinds.map((k) => k.label)),
      });

      const summary = searchPanel.getFilterSummary();
      if (summary) {
        void vscode.window.showInformationMessage(`sqry: ${summary}`);
      }
    }),
    vscode.commands.registerCommand("sqry.sortResults", async () => {
      if (!searchPanel) {
        return;
      }

      const options: Array<vscode.QuickPickItem & { value: string }> = [
        { label: "Default (search order)", value: "default" },
        { label: "By Name (A-Z)", value: "name" },
        { label: "By File Path", value: "file" },
        { label: "By Kind", value: "kind" },
        { label: "By Line Number", value: "line" },
      ];

      const selected = await vscode.window.showQuickPick(options, {
        placeHolder: "Sort results by",
      });

      if (selected) {
        searchPanel.setSortOrder(selected.value as Parameters<typeof searchPanel.setSortOrder>[0]);
      }
    }),
    vscode.commands.registerCommand("sqry.exportResults", async () => {
      if (!searchPanel) {
        return;
      }
      const symbols = searchPanel.getSymbols();
      if (symbols.length === 0) {
        void vscode.window.showInformationMessage("sqry: No results to export");
        return;
      }

      const format = await vscode.window.showQuickPick(
        [{ label: "JSON" }, { label: "Markdown" }, { label: "CSV" }],
        { placeHolder: "Export format" },
      );
      if (!format) {
        return;
      }

      let content: string;
      let language: string;
      switch (format.label) {
        case "JSON":
          content = exportAsJson(symbols);
          language = "json";
          break;
        case "Markdown":
          content = exportAsMarkdown(symbols);
          language = "markdown";
          break;
        case "CSV":
          content = exportAsCsv(symbols);
          language = "csv";
          break;
        default:
          return;
      }

      const doc = await vscode.workspace.openTextDocument({ content, language });
      await vscode.window.showTextDocument(doc);
    }),
    vscode.workspace.onDidChangeWorkspaceFolders(() => {
      void maybeAutoIndex();
    }),
  );

  // ---------------------------------------------------------------------------
  // Auto-index on save (sqry.autoIndexOnSave)
  // ---------------------------------------------------------------------------
  const autoIndexManager = new AutoIndexManager();
  context.subscriptions.push({ dispose: () => autoIndexManager.dispose() });

  const triggerAutoIndex = (rootPath: string, workspaceFolder: vscode.WorkspaceFolder): void => {
    autoIndexManager.schedule(rootPath, 30_000, () => {
      void (async () => {
        const activeClient = client;
        if (!activeClient) {
          return;
        }

        outputChannel?.appendLine(`[sqry] Auto-indexing ${workspaceFolder.name} after save...`);
        autoIndexManager.startBuild(rootPath);
        statusBar?.setBuilding();

        try {
          await activeClient.runIndex(workspaceFolder);
          await refreshIndexStats(activeClient, workspaceFolder);
        } catch (error) {
          const message = error instanceof Error ? error.message : String(error);
          outputChannel?.appendLine(`[sqry] Auto-index failed: ${message}`);
        } finally {
          const needsFollowUp = autoIndexManager.completeBuild(rootPath);
          if (needsFollowUp) {
            outputChannel?.appendLine(
              `[sqry] Dirty latch set — scheduling follow-up rebuild for ${workspaceFolder.name}`,
            );
            triggerAutoIndex(rootPath, workspaceFolder);
          }
        }
      })();
    });
  };

  context.subscriptions.push(
    vscode.workspace.onDidSaveTextDocument((doc) => {
      const setting = vscode.workspace.getConfiguration("sqry").get<string>("autoIndexOnSave", "never");
      if (setting !== "debounced") {
        return;
      }

      const workspaceFolder = vscode.workspace.getWorkspaceFolder(doc.uri);
      if (!workspaceFolder) {
        return;
      }

      const rootPath = workspaceFolder.uri.fsPath;

      if (autoIndexManager.isBuilding(rootPath)) {
        autoIndexManager.markDirty(rootPath);
        outputChannel?.appendLine(
          `[sqry] Save during active build for ${workspaceFolder.name} — will rebuild after completion`,
        );
        return;
      }

      triggerAutoIndex(rootPath, workspaceFolder);
    }),
  );

  await maybeAutoIndex();
}

/**
 * Initialize the sqry client, handling binary resolution and download.
 * Returns true if the client was successfully initialized.
 */
async function initializeClient(
  context: vscode.ExtensionContext,
  sqryClient: SqryClient,
  channel: vscode.OutputChannel,
): Promise<boolean> {
  try {
    await sqryClient.initialize();
    return true;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    channel.appendLine(`[sqry] Failed to initialize extension: ${message}`);

    statusBar?.setError(message);

    const isBinaryError = message.includes("Unable to locate sqry binary") || message.includes("sqry path is empty");
    if (isBinaryError) {
      return resolveBinary(context, sqryClient, channel);
    }

    return showInitializationError(message, channel);
  }
}

/**
 * Handle binary-not-found: try existing download, then prompt user.
 * Returns true if the client was successfully initialized.
 */
async function resolveBinary(
  context: vscode.ExtensionContext,
  sqryClient: SqryClient,
  channel: vscode.OutputChannel,
): Promise<boolean> {
  // Try to use a previously downloaded binary
  if (await tryExistingDownload(context, sqryClient, channel)) {
    return true;
  }

  // Check autoDownload setting
  const autoDownload = vscode.workspace.getConfiguration("sqry").get<boolean>("autoDownload", true);
  if (!autoDownload) {
    channel.appendLine("[sqry] Auto-download disabled (sqry.autoDownload = false)");
    void vscode.window.showErrorMessage(
      "sqry binary not found. Set sqry.path or enable sqry.autoDownload.",
      "Open Settings"
    ).then((selection) => {
      if (selection === "Open Settings") {
        void vscode.commands.executeCommand("workbench.action.openSettings", "sqry.path");
      }
    });
    return false;
  }

  // Check if platform is supported before prompting
  try {
    detectPlatform();
  } catch (platformError) {
    const platformMessage = platformError instanceof Error ? platformError.message : String(platformError);
    void vscode.window.showErrorMessage(platformMessage);
    return false;
  }

  // Prompt user to download
  const selection = await vscode.window.showInformationMessage(
    "sqry binary not found. Download from GitHub?",
    "Download",
    "Configure Path",
    "Dismiss"
  );

  if (selection === "Download") {
    return tryDownloadBinary(context, sqryClient, channel);
  }
  if (selection === "Configure Path") {
    await vscode.commands.executeCommand("workbench.action.openSettings", "sqry.path");
  }
  return false;
}

/**
 * Show a generic initialization error. Returns false (activation failed).
 */
async function showInitializationError(
  message: string,
  channel: vscode.OutputChannel,
): Promise<boolean> {
  const selection = await vscode.window.showErrorMessage(
    `sqry extension failed to initialize: ${message}`,
    "Open Settings",
    "View Output",
    "Dismiss"
  );

  if (selection === "Open Settings") {
    await vscode.commands.executeCommand("workbench.action.openSettings", "sqry.path");
  } else if (selection === "View Output") {
    channel.show();
  }
  return false;
}

/**
 * Try to find and use a previously downloaded binary.
 * Returns true if initialization succeeded.
 */
async function tryExistingDownload(
  context: vscode.ExtensionContext,
  sqryClient: SqryClient,
  channel: vscode.OutputChannel,
): Promise<boolean> {
  try {
    const version = getBinaryVersion();
    const existing = await findExistingBinary(context.globalStorageUri, version);
    if (existing) {
      channel.appendLine(`[sqry] Found previously downloaded binary: ${existing}`);
      sqryClient.setDownloadedBinaryPath(existing);
      await sqryClient.initialize();
      return true;
    }
  } catch (error) {
    const msg = error instanceof Error ? error.message : String(error);
    channel.appendLine(`[sqry] Could not use existing download: ${msg}`);
  }
  return false;
}

/**
 * Download the binary with progress UI and initialize the client.
 * Returns true if successful.
 */
async function tryDownloadBinary(
  context: vscode.ExtensionContext,
  sqryClient: SqryClient,
  channel: vscode.OutputChannel,
): Promise<boolean> {
  try {
    const binaryPath = await vscode.window.withProgress(
      {
        location: vscode.ProgressLocation.Notification,
        title: "Downloading sqry binary...",
        cancellable: true,
      },
      async (_progress, token) => {
        return downloadBinary(context, channel, token);
      },
    );

    sqryClient.setDownloadedBinaryPath(binaryPath);
    await sqryClient.initialize();
    channel.appendLine("[sqry] Extension activated with downloaded binary");
    return true;
  } catch (error) {
    const msg = error instanceof Error ? error.message : String(error);
    channel.appendLine(`[sqry] Download failed: ${msg}`);

    if (msg.includes("cancelled")) {
      // User cancelled — silent
      return false;
    }

    void vscode.window.showErrorMessage(`Failed to download sqry binary: ${msg}`);
    return false;
  }
}

export function deactivate(): void {
  client?.dispose();
  client = undefined;
}

async function maybeAutoIndex(): Promise<void> {
  const settings = readSettings();
  if (settings.autoIndexOnOpen === "never") {
    return;
  }
  const folders = vscode.workspace.workspaceFolders ?? [];
  if (!folders.length || !client) {
    return;
  }

  for (const folder of folders) {
    const hasIndex = await folderHasIndex(folder);
    if (hasIndex) {
      continue;
    }

    if (settings.autoIndexOnOpen === "always") {
      await indexWithProgress(folder);
    } else if (settings.autoIndexOnOpen === "prompt") {
      const answer = await vscode.window.showInformationMessage(
        `sqry: No index found for ${folder.name}. Run "sqry index" now?`,
        "Index Now",
        "Skip",
      );
      if (answer === "Index Now") {
        await indexWithProgress(folder);
      }
    }
  }
}

/** Check if a recent lock file indicates build is in progress. Returns true if build is active. */
async function isLockFileActive(
  lockPath: vscode.Uri,
  folderName: string,
): Promise<boolean> {
  try {
    const lockStat = await vscode.workspace.fs.stat(lockPath);
    const lockAge = Date.now() - lockStat.mtime;
    const lockAgeMin = Math.floor(lockAge / 60000);
    outputChannel?.appendLine(`[sqry] Lock file found, age: ${lockAgeMin} min`);

    if (lockAge < 30 * 60 * 1000) {
      outputChannel?.appendLine(`[sqry] Build in progress for ${folderName} (lock age: ${lockAgeMin} min)`);
      return true;
    }
    outputChannel?.appendLine(`[sqry] Stale lock detected for ${folderName} (age: ${lockAgeMin} min)`);
  } catch {
    outputChannel?.appendLine(`[sqry] No lock file found`);
  }
  return false;
}

/** Validate index via LSP. Returns true if index is healthy, false if invalid, undefined if can't validate. */
async function validateIndexViaLSP(
  folder: vscode.WorkspaceFolder,
): Promise<boolean | undefined> {
  if (!client) {
    return undefined;
  }

  try {
    outputChannel?.appendLine(`[sqry] Validating index via LSP for ${folder.name}...`);
    const status = await client.getIndexStatus(folder);
    outputChannel?.appendLine(
      `[sqry] LSP index status: exists=${status.exists}, symbols=${status.symbol_count}, files=${status.file_count}`,
    );

    if (status.exists && status.symbol_count && status.symbol_count > 0) {
      if (status.building) {
        const ageMin = status.build_age_seconds ? Math.floor(status.build_age_seconds / 60) : 0;
        outputChannel?.appendLine(`[sqry] Build in progress for ${folder.name} (${ageMin} min)`);
      }
      outputChannel?.appendLine(`[sqry] Index is healthy for ${folder.name}`);
      searchPanel?.setIndexStatus(status);
      statusBar?.update(status);
      return true;
    }

    outputChannel?.appendLine(
      `[sqry] Index validation failed for ${folder.name}: exists=${status.exists}, symbols=${status.symbol_count}`,
    );
    return false;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    outputChannel?.appendLine(`[sqry] Index validation error for ${folder.name}: ${message}`);
    return false;
  }
}

/** Check if index file is too old (> 7 days). */
function isIndexStale(indexMtime: number, folderName: string): boolean {
  const indexAge = Date.now() - indexMtime;
  const maxAge = 7 * 24 * 60 * 60 * 1000; // 7 days

  if (indexAge > maxAge) {
    const ageDays = Math.floor(indexAge / (24 * 60 * 60 * 1000));
    outputChannel?.appendLine(`[sqry] Index for ${folderName} is ${ageDays} days old, needs rebuild`);
    return true;
  }
  return false;
}

async function folderHasIndex(
  folder: vscode.WorkspaceFolder,
): Promise<boolean> {
  try {
    const indexPath = vscode.Uri.joinPath(folder.uri, ".sqry-index");
    const lockPath = vscode.Uri.joinPath(folder.uri, ".sqry-index.lock");

    outputChannel?.appendLine(`[sqry] Checking index for folder: ${folder.name} at ${folder.uri.fsPath}`);

    // Check if index file exists
    let indexStat;
    try {
      indexStat = await vscode.workspace.fs.stat(indexPath);
      outputChannel?.appendLine(`[sqry] Index file exists at ${indexPath.fsPath}`);
    } catch {
      outputChannel?.appendLine(`[sqry] Index file not found at ${indexPath.fsPath}`);
      const lockActive = await isLockFileActive(lockPath, folder.name);
      return lockActive; // Return true only if build is in progress
    }

    // Index exists - validate via LSP if available
    const lspResult = await validateIndexViaLSP(folder);
    if (lspResult !== undefined) {
      return lspResult;
    }

    // Fallback: check index age
    return !isIndexStale(indexStat.mtime, folder.name);
  } catch (error) {
    outputChannel?.appendLine(`[sqry] Error checking index for ${folder.name}: ${error}`);
    return false;
  }
}

function getActiveWorkspaceFolder():
  | vscode.WorkspaceFolder
  | undefined {
  const editor = vscode.window.activeTextEditor;
  if (editor) {
    return vscode.workspace.getWorkspaceFolder(editor.document.uri);
  }
  return vscode.workspace.workspaceFolders?.[0];
}

function getAllWorkspaceFolders(): readonly vscode.WorkspaceFolder[] {
  return vscode.workspace.workspaceFolders ?? [];
}

/**
 * Prompt user to pick a workspace folder when multiple roots exist.
 * Returns the selected folder, or undefined if cancelled.
 */
async function pickWorkspaceFolder(
  placeHolder: string,
): Promise<vscode.WorkspaceFolder | undefined> {
  const folders = getAllWorkspaceFolders();
  if (folders.length === 0) {
    return undefined;
  }
  if (folders.length === 1) {
    return folders[0];
  }
  const picked = await vscode.window.showQuickPick(
    folders.map(f => ({ label: f.name, description: f.uri.fsPath, folder: f })),
    { placeHolder },
  );
  return picked?.folder;
}

async function pickSymbolName(
  document: vscode.TextDocument,
  position: vscode.Position,
): Promise<string | undefined> {
  const wordRange = document.getWordRangeAtPosition(position, /[\w:]+/);
  let fallback = wordRange ? document.getText(wordRange) : undefined;

  const symbols = (await vscode.commands.executeCommand<vscode.DocumentSymbol[]>(
    "vscode.executeDocumentSymbolProvider",
    document.uri,
  )) ?? [];

  const target = locateSymbol(symbols, position);
  if (target) {
    return buildQualifiedName(target.symbol, target.ancestors) ?? target.symbol.name;
  }

  if (!fallback) {
    fallback = extractWordAtPosition(document, position);
  }

  return fallback;
}

function locateSymbol(
  symbols: vscode.DocumentSymbol[],
  position: vscode.Position,
  ancestors: vscode.DocumentSymbol[] = [],
): { symbol: vscode.DocumentSymbol; ancestors: vscode.DocumentSymbol[] } | undefined {
  for (const symbol of symbols) {
    if (symbol.range.contains(position)) {
      const nextAncestors = [...ancestors, symbol];
      if (symbol.children?.length) {
        const match = locateSymbol(symbol.children, position, nextAncestors);
        if (match) {
          return match;
        }
      }
      return { symbol, ancestors };
    }
  }
  return undefined;
}

function buildQualifiedName(
  symbol: vscode.DocumentSymbol,
  ancestors: vscode.DocumentSymbol[],
): string {
  const parts = ancestors
    .filter((ancestor) =>
      ancestor.kind === vscode.SymbolKind.Class ||
      ancestor.kind === vscode.SymbolKind.Struct ||
      ancestor.kind === vscode.SymbolKind.Namespace ||
      ancestor.kind === vscode.SymbolKind.Module,
    )
    .map((ancestor) => ancestor.name);
  parts.push(symbol.name);
  return parts.join(".");
}

function extractWordAtPosition(
  document: vscode.TextDocument,
  position: vscode.Position,
): string | undefined {
  const line = document.lineAt(position.line).text;
  if (!line) {
    return undefined;
  }

  const isWordChar = (char: string | undefined): boolean =>
    !!char && /\w/.test(char);

  let start = position.character;
  let end = position.character;

  while (start > 0 && isWordChar(line[start - 1])) {
    start -= 1;
  }
  while (end < line.length && isWordChar(line[end])) {
    end += 1;
  }

  if (start === end) {
    return undefined;
  }

  return line.slice(start, end);
}

/** Handle binary/path configuration errors. Returns true if handled. */
async function handleBinaryError(message: string): Promise<boolean> {
  if (!message.includes("Unable to locate sqry binary") && !message.includes("sqry path is empty")) {
    return false;
  }

  const selection = await vscode.window.showErrorMessage(
    `sqry error: ${message}`,
    "Open Settings",
    "View README",
  );
  if (selection === "Open Settings") {
    await vscode.commands.executeCommand("workbench.action.openSettings", "sqry.path");
  } else if (selection === "View README") {
    await vscode.env.openExternal(
      vscode.Uri.parse("https://github.com/verivus-oss/sqry/blob/master/README.md"),
    );
  }
  return true;
}

/** Handle timeout errors. Returns true if handled. */
async function handleTimeoutError(message: string): Promise<boolean> {
  if (!message.includes("timed out after") || !message.includes("ms")) {
    return false;
  }

  const settingName = message.includes("sqry.indexTimeoutMs") ? "sqry.indexTimeoutMs" : "sqry.timeoutMs";
  const selection = await vscode.window.showErrorMessage(
    `sqry error: ${message}`,
    "Increase Timeout",
    "Cancel",
  );
  if (selection === "Increase Timeout") {
    await vscode.commands.executeCommand("workbench.action.openSettings", settingName);
  }
  return true;
}

/** Handle query execution failures. Returns true if handled. */
async function handleQueryError(message: string): Promise<boolean> {
  if (!message.includes("failed to execute sqry query")) {
    return false;
  }

  const workspace = getActiveWorkspaceFolder();
  if (workspace) {
    const selection = await vscode.window.showErrorMessage(
      `sqry error: ${message}. This might be due to an outdated or corrupted index.`,
      "Rebuild Index",
      "Cancel",
    );
    if (selection === "Rebuild Index") {
      await indexWithProgress(workspace);
    }
  } else {
    await vscode.window.showErrorMessage(`sqry error: ${message}`);
  }
  return true;
}

async function handleError(error: unknown): Promise<void> {
  let message: string;
  if (error instanceof Error) {
    message = error.message;
  } else if (error !== null && error !== undefined) {
    message = typeof error === 'string' ? error : JSON.stringify(error);
  } else {
    message = "Unknown error";
  }

  if (await handleBinaryError(message)) return;
  if (await handleTimeoutError(message)) return;
  if (await handleQueryError(message)) return;

  await vscode.window.showErrorMessage(`sqry error: ${message}`);
}

async function refreshIndexStats(
    activeClient: SqryClient,
    workspace: vscode.WorkspaceFolder,
): Promise<void> {
    try {
        const status = await activeClient.getIndexStatus(workspace);
        searchPanel?.setIndexStatus(status);
        searchPanel?.setIndexStatusForRoot(workspace.uri.fsPath, status);

        // In multi-root, update status bar with worst-state-wins logic
        const folders = getAllWorkspaceFolders();
        if (folders.length > 1 && searchPanel) {
          statusBar?.updateMultiRoot(searchPanel.getIndexStatusMap());
        } else {
          statusBar?.update(status);
        }

        outputChannel?.appendLine(
            `[sqry] Index stats refreshed: ${status.symbol_count} symbols, ${status.file_count} files`,
        );

        // After index rebuild, clear stale diagnostics and refresh for open editors
        if (diagnosticsProvider) {
            diagnosticsProvider.clear();
            await diagnosticsProvider.refreshForOpenEditors(workspace);
        }
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        outputChannel?.appendLine(`[sqry] Failed to refresh index stats after rebuild: ${message}`);
    }
}

async function indexWithProgress(folder: vscode.WorkspaceFolder): Promise<void> {
  const activeClient = client;
  if (!activeClient) {
    return;
  }

  // Check if build is already in progress
  let force = false;
  try {
    const lockPath = vscode.Uri.joinPath(folder.uri, ".sqry-index.lock");
    try {
      const lockStat = await vscode.workspace.fs.stat(lockPath);
      const lockAge = Date.now() - lockStat.mtime;
      if (lockAge < 30 * 60 * 1000) {  // 30 minutes
        const ageMin = Math.floor(lockAge / 60000);
        const action = await vscode.window.showWarningMessage(
          `sqry: Index build already in progress for ${folder.name} (started ${ageMin} min ago)`,
          "Wait",
          "Force Rebuild"
        );
        if (action !== "Force Rebuild") {
          return;
        }
        // User chose Force Rebuild - pass force=true
        force = true;
      }
    } catch {
      // No lock file - OK to proceed
    }
  } catch (error) {
    outputChannel?.appendLine(`[sqry] Error checking lock file: ${error}`);
  }

  // Step 7: Graceful degradation - show fallback notification if LSP progress not received within 3s
  // This ensures users always get feedback even if LSP progress notifications fail silently.
  // The fallback is cancelled when actual progress notifications are received from the server.
  let progressReceived = false;
  const fallbackTimer = setTimeout(() => {
    if (!progressReceived) {
      void vscode.window.showInformationMessage(`sqry: Indexing ${folder.name}...`);
    }
  }, 3000);

  // Subscribe to progress notifications to cancel fallback timer when real progress is received
  const progressSubscription = activeClient.onIndexProgress(() => {
    if (!progressReceived) {
      progressReceived = true;
      clearTimeout(fallbackTimer);
      statusBar?.setBuilding();
    }
  });

  // Progress is handled by LSP WorkDoneProgress notifications from the server.
  // The vscode-languageclient automatically displays these in the notification area.
  try {
    await activeClient.runIndex(folder, force);
    // Success message is sent by LSP server via window/showMessage with symbol counts
    await refreshIndexStats(activeClient, folder);
  } catch (error) {
    await handleError(error);
  } finally {
    // Clean up: ensure timer is cleared and subscription is disposed
    progressReceived = true;
    clearTimeout(fallbackTimer);
    progressSubscription.dispose();
  }
}
