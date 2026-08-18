import * as fs from "node:fs";
import * as vscode from "vscode";
import {
  isWorkspaceFolderExcluded,
  nonExcludedFolders,
  readSettings,
} from "./config";
import { downloadBinary, findExistingBinary, getBinaryVersion, detectPlatform } from "./binaryDownloader";
import { SqryCodeLensProvider } from "./codeLens";
import { SearchPanel } from "./searchPanel";
import { SqryClient } from "./sqryClient";
import { addToHistory, clearHistory, formatRelativeTime, SearchHistoryEntry } from "./searchHistory";
import { SqryCodeActionProvider } from "./codeActions";
import { SqrySourceRootStatus, SqryWorkspaceStatus } from "./lspProtocol";
import { SqryDiagnosticsProvider } from "./diagnosticsProvider";
import { SqryHoverProvider } from "./hoverProvider";
import { SqryGraphPanel, GraphNode, GraphEdge } from "./graphPanel";
import { openFileWithinWorkspace, GuardSelection } from "./workspaceGuard";
import { SqryStatusBar } from "./statusBar";
import { exportAsJson, exportAsMarkdown, exportAsCsv } from "./exportResults";
import { AutoIndexManager } from "./autoIndex";
import { LoadingStateMachine, MANUAL_GATE_TIMEOUT_MS } from "./loadingState";
import {
  buildClassificationScaffold,
  buildWorkspaceInitializationPayload,
  resolveWorkspaceFilePath,
} from "./workspaceClassifier";
import {
  gatedManualRebuild,
  isManualRebuildGateTimeout,
} from "./manualRebuildGate";
import { emitWorkspaceResolutionTelemetry as sharedEmitWorkspaceResolutionTelemetry } from "./workspaceTelemetry";
import {
  bindSqryReadyContext,
  registerStartupCommands,
  type ReadinessContextBinding,
  type WorkspaceStatusRefreshResult,
} from "./startupCommands";
import {
  completeInitialWorkspaceResolution,
  deactivateStartupResources,
  disposeStartupClient,
} from "./startupLifecycle";

// STEP_12 — re-export the formatter + shared emitter so callers that
// imported them from `extension.ts` previously continue to compile.
// New callers SHOULD import directly from `./workspaceTelemetry`.
export {
  emitWorkspaceResolutionTelemetry as sharedEmitWorkspaceResolutionTelemetry,
  formatWorkspaceResolutionTelemetry,
} from "./workspaceTelemetry";

const HISTORY_STATE_KEY = "sqry.searchHistory";

let client: SqryClient | undefined;
let outputChannel: vscode.OutputChannel | undefined;
let searchPanel: SearchPanel | undefined;
let statusBar: SqryStatusBar | undefined;
let diagnosticsProvider: SqryDiagnosticsProvider | undefined;
let loadingState: LoadingStateMachine | undefined;
let readinessContextBinding: ReadinessContextBinding | undefined;
/**
 * Activation-scoped reference to the singleton [`AutoIndexManager`].
 *
 * STEP_5 codex iter1 MAJOR fix: `maybeAutoIndex` now delegates the
 * source-root filtering to `AutoIndexManager.enqueueFromWorkspaceStatus`,
 * so the helper needs the activation-scoped instance. The reference is
 * assigned during `activate` and cleared by `deactivate`.
 */
let autoIndexManagerRef: AutoIndexManager | undefined;

/**
 * Release the activation-owned client on every terminal startup path.
 *
 * Clearing the global first keeps later permanent command dispatchers and
 * delayed callbacks from observing a client that has already failed. Disposal
 * synchronously cancels LSP work and unregisters its configuration listener.
 */
function disposeTerminalStartupClient(): void {
  const failedClient = client;
  client = undefined;
  disposeStartupClient(failedClient);
}

export async function activate(
  context: vscode.ExtensionContext,
): Promise<void> {
  outputChannel = vscode.window.createOutputChannel("Sqry");
  context.subscriptions.push(outputChannel);

  // Register `sqry.showOutput` up front, before the LSP-start step below
  // can bail out. When binary resolution fails, `activate` returns early
  // (see the `if (!initialized) { ... return; }` block) and never reaches
  // the main command-registration block. But the failure UI wires its
  // "View Logs" affordance (status bar + search panel) to `sqry.showOutput`,
  // so it must be registered before that early return, otherwise clicking it
  // fails with `command 'sqry.showOutput' not found`.
  context.subscriptions.push(
    vscode.commands.registerCommand("sqry.showOutput", () => {
      outputChannel?.show(true);
    }),
  );

  // Result-driven file navigation (search results, symbol lists) routes through
  // this command so every open is confined to the workspace. The paths come from
  // indexed data, so opening them directly with `vscode.open` could reach a file
  // outside the workspace via an absolute path, a `..` sequence, or a symlink.
  context.subscriptions.push(
    vscode.commands.registerCommand(
      "sqry.openResultFile",
      (filePath: string, selection?: GuardSelection) =>
        openFileWithinWorkspace(filePath, { selection }),
    ),
  );

  // STEP_5 contract: state machine starts in `Activating`. Both UI
  // surfaces (status bar + tree view) are created BEFORE the LSP starts
  // so the user sees the resolving spinner during binary resolution
  // and the language-server boot — never an empty / "no index" view.
  loadingState = new LoadingStateMachine();
  context.subscriptions.push({ dispose: () => loadingState?.dispose() });

  // Every public manifest command is registered before anything can await.
  // The dispatcher is phase-safe even if a stale VS Code context key remains
  // visible after a host-side `setContext(false)` failure.
  const startupCommands = registerStartupCommands({
    getLoadingState: () => loadingState,
    getOutputChannel: () => outputChannel,
  });
  context.subscriptions.push(startupCommands);

  try {
    // The initial false context write is an activation barrier. Until it has
    // succeeded, no LSP/client work can start and no real public handler is
    // attached. Runtime write failures transition the existing state machine
    // to Failed, while permanent dispatchers remain locally actionable.
    readinessContextBinding = await bindSqryReadyContext(
      loadingState,
      outputChannel,
      (error) => {
        const activeLoadingState = loadingState;
        if (!activeLoadingState) {
          return;
        }
        if (!activeLoadingState.isFailed()) {
          const reason = `sqry command readiness update failed: ${error.message}`;
          outputChannel?.appendLine(`[sqry] ${reason}`);
          activeLoadingState.transition("Failed", { reason, viewLogsAction: true });
        }
        disposeTerminalStartupClient();
      },
    );
    context.subscriptions.push(readinessContextBinding);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    const reason = "sqry could not establish command readiness; see Sqry output";
    outputChannel.appendLine(`[sqry] ${reason}: ${message}`);
    loadingState.transition("Failed", { reason, viewLogsAction: true });
    void vscode.window.showErrorMessage(`${reason}: ${message}`, "View Logs").then((selection) => {
      if (selection === "View Logs") {
        outputChannel?.show(true);
      }
    });
    return;
  }

  client = new SqryClient(outputChannel);
  context.subscriptions.push(client);

  // Forward `.code-workspace` location to the LSP so the
  // LogicalWorkspaceRegistry can classify per-request paths. This MUST
  // be set before `initializeClient`, since the LSP only reads
  // initializationOptions during the initialize handshake.
  //
  // STEP_5 codex iter1 MAJOR fix: the contract is two distinct shapes —
  // `workspace` carries the PARSED + CLASSIFIED object (from the
  // extension-side `workspaceClassifier`); `workspaceFile` carries the
  // PATH STRING (the LSP loads + classifies in-process via branch 4 of
  // `resolve_logical_workspace`). We parse here at activation so the
  // classification is computed once; on a parse failure we still send
  // the path so the LSP can fall back to its in-process loader.
  const workspaceFilePath = resolveWorkspaceFilePath(
    vscode.workspace.workspaceFile?.fsPath,
  );
  // STEP_10 iter3 wire-up — read the `sqry.indexRoot` setting at
  // activation. When non-empty, forward as `initializationOptions.sqry.indexRoot`
  // so the LSP can use it as the canonical workspace identity (the
  // in-band replacement for the legacy `--index-root` CLI flag — see
  // `docs/cli/workspace-wrapper-migration.md`). We deliberately read
  // settings here (rather than calling `readSettings()` for the full
  // bag) so the wire-up stays self-contained: the `.code-workspace`
  // path and the `indexRoot` are independent inputs to the LSP
  // resolver, and either, both, or neither may be set.
  const indexRootSetting = vscode.workspace
    .getConfiguration("sqry")
    .get<string>("indexRoot", "")
    .trim();
  const indexRoot = indexRootSetting.length > 0 ? indexRootSetting : undefined;
  if (workspaceFilePath || indexRoot) {
    let payload: ReturnType<typeof buildWorkspaceInitializationPayload> = null;
    if (workspaceFilePath) {
      try {
        payload = buildWorkspaceInitializationPayload(workspaceFilePath);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        outputChannel.appendLine(
          `[sqry] Failed to parse .code-workspace at ${workspaceFilePath}: ${message}. ` +
            "LSP will fall back to in-process classification (branch 4).",
        );
      }
    }
    client.setInitializationOptions({
      // The parsed object — defaults to `{folders:[], classification:null}`
      // when the workspace file does not exist or could not be parsed;
      // the LSP detects this lightweight hint shape and falls through to
      // branch 4 (`workspaceFile`) which loads + classifies in-process.
      // Only emitted when a `.code-workspace` was open at activation.
      ...(workspaceFilePath
        ? {
            workspace: payload ?? { folders: [], classification: null },
            workspaceFile: workspaceFilePath,
          }
        : {}),
      // `sqry.indexRoot` — forwarded as a separate sibling field
      // (`initializationOptions.sqry.indexRoot`). The LSP feeds it
      // into `WorkspaceResolutionInputs.index_root` when no explicit
      // CLI `--index-root` flag was passed.
      ...(indexRoot ? { indexRoot } : {}),
    });
  }

  searchPanel = new SearchPanel(context, client, outputChannel);
  context.subscriptions.push(searchPanel);

  const statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
  statusBar = new SqryStatusBar(statusBarItem, outputChannel ?? null);
  context.subscriptions.push(statusBar);

  // Wire phase transitions to the visible surfaces. The state machine
  // owns the contract; the surfaces just mirror it.
  context.subscriptions.push(
    loadingState.onDidChangePhase((phase, failed) => {
      statusBar?.setLoadingPhase(phase, failed?.reason);
      if (phase === "Failed") {
        searchPanel?.setLoadingPhase("failed", failed?.reason);
      } else if (phase === "Ready") {
        searchPanel?.setLoadingPhase("ready");
      } else {
        searchPanel?.setLoadingPhase("loading");
      }
    }),
  );

  // Show the spinner immediately — covers the brief window before the
  // LSP can start, including binary download.
  searchPanel?.setLoadingPhase("loading");
  loadingState.transition("LspStarting");

  const initialized = await initializeClient(context, client, outputChannel);
  if (!initialized) {
    if (loadingState && !loadingState.isFailed()) {
      loadingState.transition("Failed", {
        reason: "sqry language server failed to start",
        viewLogsAction: true,
      });
    }
    disposeTerminalStartupClient();
    return;
  }
  loadingState.transition("WorkspaceResolving");

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

  // Diagnostics lifecycle + config change refresh
  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument((doc) => {
      const workspace = doc.uri.scheme === "file"
        ? vscode.workspace.getWorkspaceFolder(doc.uri)
        : undefined;
      if (workspace && diagnosticsProvider) {
        void diagnosticsProvider.refreshForFile(doc.uri, workspace);
      }
    }),
    vscode.workspace.onDidCloseTextDocument((doc) => {
      diagnosticsProvider?.clearFile(doc.uri);
    }),
    client.onDidChangeConfig(async () => {
      if (client) {
        try {
          await refreshWorkspaceStatus(client);
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
    startupCommands.registerHandler("sqry.query", (preset?: unknown) =>
      runQuery(typeof preset === "string" ? preset : undefined),
    ),
    vscode.commands.registerCommand("sqry.runQueryInternal", (preset: string) =>
      runQuery(preset),
    ),
    startupCommands.registerHandler("sqry.searchWorkspace", async () => {
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
    startupCommands.registerHandler("sqry.findReferences", async () => {
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
    startupCommands.registerHandler("sqry.index", () => handleIndexCommand()),
    startupCommands.registerHandler("sqry.refreshStats", async () => {
      const activeClient = client;
      if (!activeClient) {
        const message = "sqry is not ready to refresh index stats. Select View Logs for details.";
        outputChannel?.appendLine(`[sqry] ${message}`);
        void vscode.window.showWarningMessage(message, "View Logs").then((selection) => {
          if (selection === "View Logs") {
            outputChannel?.show(true);
          }
        });
        return;
      }

      const result = await refreshWorkspaceStatus(activeClient);
      if (result.ok) {
        outputChannel?.appendLine("[sqry] Refreshed index stats for all workspace roots");
        return;
      }

      const message = result.error.message;
      outputChannel?.appendLine(`[sqry] Failed to refresh stats: ${message}`);
      void vscode.window.showWarningMessage(`sqry: Failed to refresh stats: ${message}`);
    }),
    startupCommands.registerHandler("sqry.editWorkspaceClassification", () =>
      editWorkspaceClassification(),
    ),
    startupCommands.registerHandler("sqry.clearResults", () => {
      if (searchPanel) {
        searchPanel.clearResults();
      }
    }),
    startupCommands.registerHandler("sqry.searchHistory", async () => {
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
    startupCommands.registerHandler("sqry.scanWorkspace", async () => {
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
      const provider = diagnosticsProvider;
      const selectedWorkspace = workspace;
      await vscode.window.withProgress(
        {
          location: vscode.ProgressLocation.Notification,
          title: "sqry: Scanning workspace...",
        },
        () => provider.scanWorkspace(selectedWorkspace),
      );
    }),
    startupCommands.registerHandler("sqry.restartLsp", async () => {
      const activeClient = client;
      if (!activeClient) {
        return;
      }
      outputChannel?.appendLine("[sqry] Restarting language server...");
      try {
        await activeClient.restart();
        outputChannel?.appendLine("[sqry] Language server restarted successfully");
        void vscode.window.showInformationMessage("sqry: Language server restarted.");
        // Refresh stats for all workspace roots after restart
        await refreshWorkspaceStatus(activeClient);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        outputChannel?.appendLine(`[sqry] Failed to restart language server: ${message}`);
        void vscode.window.showErrorMessage(`sqry: Failed to restart language server: ${message}`);
      }
    }),
    startupCommands.registerHandler("sqry.rebuildIndex", async () => {
      const activeClient = client;
      if (!activeClient) {
        return;
      }
      const workspace = getActiveWorkspaceFolder();
      if (!workspace) {
        void vscode.window.showWarningMessage("sqry: No workspace folder detected.");
        return;
      }
      // STEP_5 codex iter1 MAJOR fix: manual rebuild commands MUST gate
      // on `Ready` (DAG mandate, 30s timeout). The legacy direct call
      // bypassed the gate and hit `runIndex` while the LSP was still
      // resolving the workspace, producing the bug the loading-state
      // contract was designed to prevent.
      try {
        await runGatedRebuild(async () => {
          await activeClient.runIndex(workspace);
          await refreshSourceRootStatus(activeClient, workspace);
          await refreshWorkspaceStatus(activeClient);
        });
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
        rootPath?: string,
      ) => {
        if (!searchPanel) {
          return;
        }
        const languageSuffix = language ? ` (${language})` : "";
        outputChannel?.appendLine(`[sqry] Load more ${itemType}${languageSuffix} from offset ${nextOffset}`);
        await searchPanel.loadMore(itemType, nextOffset, language, rootPath);
      },
    ),
    startupCommands.registerHandler("sqry.showCallGraph", async () => {
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
    startupCommands.registerHandler("sqry.showDependencies", async () => {
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
    startupCommands.registerHandler("sqry.filterResults", async () => {
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
    startupCommands.registerHandler("sqry.sortResults", async () => {
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
    startupCommands.registerHandler("sqry.exportResults", async () => {
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
      void (async () => {
        await maybeAutoIndex();
        if (client) {
          await refreshWorkspaceStatus(client);
        }
      })();
    }),
  );

  // ---------------------------------------------------------------------------
  // Auto-index on save (sqry.autoIndexOnSave)
  // ---------------------------------------------------------------------------
  const autoIndexManager = new AutoIndexManager();
  autoIndexManagerRef = autoIndexManager;
  context.subscriptions.push({
    dispose: () => {
      autoIndexManager.dispose();
      if (autoIndexManagerRef === autoIndexManager) {
        autoIndexManagerRef = undefined;
      }
    },
  });

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
          await refreshSourceRootStatus(activeClient, workspaceFolder);
          await refreshWorkspaceStatus(activeClient);
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

  // Hydrate the aggregate workspace status before we transition to
  // Ready — the UI is gated on at least one successful round-trip so
  // the tree view never flips to "no index" because of a transient
  // post-LSP-start race.
  const activeClient = client;
  const activeLoadingState = loadingState;
  if (!activeClient || !activeLoadingState) {
    const reason = "sqry language client is unavailable after initialization";
    outputChannel?.appendLine(`[sqry] ${reason}`);
    if (activeLoadingState && !activeLoadingState.isFailed()) {
      activeLoadingState.transition("Failed", { reason, viewLogsAction: true });
    }
    disposeTerminalStartupClient();
    return;
  }

  const completedInitialWorkspaceResolution = await completeInitialWorkspaceResolution({
    activeClient,
    loadingState: activeLoadingState,
    refreshWorkspaceStatus,
    // STEP_12 telemetry — emit ONE aggregate startup line only after Ready.
    // It is intentionally not awaited by the startup-resolution coordinator.
    emitTelemetry: () => outputChannel
      ? emitWorkspaceResolutionTelemetry(activeClient, outputChannel)
      : Promise.resolve(),
    // Auto-index runs only after Ready — by definition, the LSP has already
    // told us which source roots are missing.
    maybeAutoIndex,
    log: (line) => outputChannel?.appendLine(line),
  });
  if (!completedInitialWorkspaceResolution) {
    disposeTerminalStartupClient();
  }
}

async function handleIndexCommand(): Promise<void> {
  const activeClient = client;
  if (!activeClient) {
    return;
  }
  const folders = getAllWorkspaceFolders();
  if (folders.length === 0) {
    void vscode.window.showWarningMessage(
      "sqry: No workspace folder detected. Open a folder before indexing.",
    );
    return;
  }

  if (folders.length === 1) {
    await indexSingleRoot(activeClient);
    return;
  }

  await indexMultiRoot(activeClient, folders);
}

async function indexSingleRoot(activeClient: SqryClient): Promise<void> {
  const workspace = getActiveWorkspaceFolder();
  if (!workspace) {
    return;
  }
  // STEP_5 codex iter1 MAJOR fix: gate on Ready. See `sqry.rebuildIndex`.
  try {
    await runGatedRebuild(async () => {
      await activeClient.runIndex(workspace);
      await refreshSourceRootStatus(activeClient, workspace);
      await refreshWorkspaceStatus(activeClient);
    });
  } catch (error) {
    await handleError(error);
  }
}

async function indexMultiRoot(activeClient: SqryClient, folders: readonly vscode.WorkspaceFolder[]): Promise<void> {
  const items: Array<{ label: string; description: string; folder: vscode.WorkspaceFolder | undefined }> = [
    { label: "All Workspace Folders", description: `Index all ${folders.length} roots`, folder: undefined },
    ...folders.map(f => ({ label: f.name, description: f.uri.fsPath, folder: f })),
  ];
  const picked = await vscode.window.showQuickPick(items, {
    placeHolder: "Select workspace folder to index",
  });
  if (!picked) {
    return;
  }

  // STEP_5 codex iter1 MAJOR fix: every branch of the multi-root path
  // gates on Ready before issuing `runIndex`. We open the gate ONCE
  // around the whole operation so the user is not asked to wait twice
  // when “All Workspace Folders” is selected.
  if (!picked.folder) {
    // "All Workspace Folders" selected — index each sequentially
    try {
      await runGatedRebuild(async () => {
        for (const folder of folders) {
          try {
            await activeClient.runIndex(folder);
            await refreshSourceRootStatus(activeClient, folder);
          } catch (error) {
            const message = error instanceof Error ? error.message : String(error);
            outputChannel?.appendLine(`[sqry] Failed to index ${folder.name}: ${message}`);
          }
        }
        await refreshWorkspaceStatus(activeClient);
      });
    } catch (error) {
      await handleError(error);
    }
    return;
  }

  // Single folder selected
  try {
    await runGatedRebuild(async () => {
      await activeClient.runIndex(picked.folder!);
      await refreshSourceRootStatus(activeClient, picked.folder!);
      await refreshWorkspaceStatus(activeClient);
    });
  } catch (error) {
    await handleError(error);
  }
}

/**
 * Wrap a manual-rebuild operation in the loading-state gate.
 *
 * STEP_5 acceptance criterion (codex iter1 MAJOR fix): every command
 * that ends in `runIndex` from a USER-FACING command handler funnels
 * through this helper so the LSP is guaranteed to be `Ready` before
 * the rebuild is dispatched. The 30-second timeout is the DAG-mandated
 * `MANUAL_GATE_TIMEOUT_MS`.
 *
 * Errors:
 * - On gate timeout, surfaces a typed `ManualRebuildGateTimeoutError`
 *   to the user via `showWarningMessage` (and the output channel) and
 *   re-throws so callers can propagate. UI-side handlers (`handleError`)
 *   detect the gate-timeout shape and skip the generic error toast to
 *   avoid double notification.
 * - On terminal LSP failure, the `LoadingStateMachine.waitForReady`
 *   rejection propagates through unchanged — the user sees the actual
 *   failure cause via the existing `handleError` path.
 *
 * The helper is a thin VS Code-aware wrapper around the pure
 * `gatedManualRebuild` from `manualRebuildGate.ts`. The pure helper is
 * unit-tested (no extension host required); this wrapper exists only
 * to add the user-visible warning and the loadingState resolution.
 */
async function runGatedRebuild<T>(operation: () => Promise<T>): Promise<T> {
  const gate = loadingState;
  if (!gate) {
    // Activation has not finished yet — fail loudly rather than
    // silently bypass the gate.
    throw new Error(
      "sqry: loading-state machine is not initialized; refuse to dispatch manual rebuild",
    );
  }
  try {
    return await gatedManualRebuild(gate, operation, {
      timeoutMs: MANUAL_GATE_TIMEOUT_MS,
      log: (event) => {
        switch (event.kind) {
          case "gate-immediate":
            // No log — Ready at entry is the steady-state path.
            break;
          case "gate-waited":
            outputChannel?.appendLine(
              `[sqry] Manual rebuild gate cleared after ${event.waitedMs}ms`,
            );
            break;
          case "gate-timeout":
            outputChannel?.appendLine(
              `[sqry] Manual rebuild gate TIMED OUT after ${event.timeoutMs}ms` +
                (event.cause ? ` (cause: ${event.cause})` : ""),
            );
            void vscode.window.showWarningMessage(
              `sqry: cannot rebuild — language server did not reach Ready within ${event.timeoutMs}ms.`,
            );
            break;
          case "gate-failed":
            outputChannel?.appendLine(
              `[sqry] Manual rebuild gate FAILED — language server reached terminal Failed state: ${event.reason}`,
            );
            break;
        }
      },
    });
  } catch (err) {
    if (isManualRebuildGateTimeout(err)) {
      // Re-throw so the caller can decide whether to also surface a
      // toast — but mark the error so `handleError` skips its default
      // notification. We tag with the same code constant the gate uses.
      throw err;
    }
    throw err;
  }
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
    const existing = await findExistingBinary(context.globalStorageUri, version, context.extensionMode);
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

export async function deactivate(): Promise<void> {
  const binding = readinessContextBinding;
  readinessContextBinding = undefined;
  const activeClient = client;
  client = undefined;
  await deactivateStartupResources({
    activeClient,
    readinessContextBinding: binding,
  });
}

/**
 * STEP_5: auto-index walks the aggregate `WorkspaceStatus`. The LSP is
 * the source of truth for which source roots are missing — per-folder
 * filesystem stat probing is forbidden by acceptance criterion 4.
 *
 * STEP_5 codex iter1 MAJOR fix: the source-root filtering logic
 * (`status === "missing"` + exclude-predicate filter) lives in
 * [`AutoIndexManager.enqueueFromWorkspaceStatus`], NOT inline here.
 * This wiring layer is responsible only for resolving the `WorkspaceFolder`
 * for each enqueued root and dispatching the user-facing prompt /
 * background rebuild via `indexWithProgress`.
 */
async function maybeAutoIndex(): Promise<void> {
  const settings = readSettings();
  if (settings.autoIndexOnOpen === "never") {
    return;
  }
  if (!client || !loadingState?.isReady()) {
    return;
  }
  const status = await safeWorkspaceStatus();
  if (!status) {
    return;
  }
  const manager = autoIndexManagerRef;
  if (!manager) {
    // Activation has not finished yet. The same `manager` instance is
    // used by the on-save debouncer; if it is missing here we are in a
    // pathological state (e.g. tests calling maybeAutoIndex directly)
    // and there is no safe action other than to bail.
    return;
  }

  // The exclude predicate maps a source-root path to a boolean by
  // resolving the matching `WorkspaceFolder` and consulting
  // `isWorkspaceFolderExcluded`. The autoIndex helper does not know
  // about VS Code workspace folders by design — this lookup is the
  // wiring that bridges the two surfaces.
  const exclude = (rootPath: string): boolean => {
    const folder = resolveFolderForSourceRoot({
      path: rootPath,
      // The predicate only consults `path`; we synthesise the rest of
      // the SqrySourceRootStatus shape because the lookup function is
      // path-keyed.
      status: "missing",
    });
    if (!folder) {
      // Source roots that have no matching workspace folder cannot be
      // indexed via `indexWithProgress` (which requires a folder).
      // Treat them as excluded so the autoIndex helper does not try to
      // run them.
      return true;
    }
    return isWorkspaceFolderExcluded(folder, settings);
  };

  await manager.enqueueFromWorkspaceStatus(
    status,
    async (root) => {
      const folder = resolveFolderForSourceRoot(root);
      if (!folder) {
        return;
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
    },
    { exclude },
  );
}

/** Resolve a `WorkspaceFolder` for a given source-root path. */
function resolveFolderForSourceRoot(
  root: SqrySourceRootStatus,
): vscode.WorkspaceFolder | undefined {
  const folders = vscode.workspace.workspaceFolders ?? [];
  return folders.find((f) => f.uri.fsPath === root.path);
}

/**
 * Implementation of the `sqry.editWorkspaceClassification` command.
 *
 * Resolves the active `.code-workspace`, scaffolds the
 * `sqry.workspace` block when absent (without overwriting any other
 * keys), writes the file atomically, and opens it in an editor pane.
 *
 * When no `.code-workspace` is open we surface a `Save Workspace`
 * suggestion — VS Code's `workbench.action.saveWorkspaceAs` lets the
 * user materialise one. We do NOT silently scaffold against
 * `settings.json`; the DAG ties the command to the workspace file.
 */
async function editWorkspaceClassification(): Promise<void> {
  const workspaceFilePath = resolveWorkspaceFilePath(
    vscode.workspace.workspaceFile?.fsPath,
  );
  if (!workspaceFilePath) {
    const action = await vscode.window.showInformationMessage(
      "sqry: No `.code-workspace` file is currently open. Save the workspace first to scaffold a `sqry.workspace` block.",
      "Save Workspace…",
    );
    if (action === "Save Workspace…") {
      await vscode.commands.executeCommand("workbench.action.saveWorkspaceAs");
    }
    return;
  }
  let raw: string | null = null;
  try {
    raw = fs.readFileSync(workspaceFilePath, "utf-8");
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code !== "ENOENT") {
      const message = err instanceof Error ? err.message : String(err);
      void vscode.window.showErrorMessage(`sqry: cannot read workspace file: ${message}`);
      return;
    }
  }
  let scaffolded: { content: string; alreadyHadBlock: boolean };
  try {
    scaffolded = buildClassificationScaffold(raw);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    void vscode.window.showErrorMessage(
      `sqry: cannot scaffold sqry.workspace — workspace file is not valid JSON (${message}). Open the file manually and add the block.`,
    );
    const doc = await vscode.workspace.openTextDocument(workspaceFilePath);
    await vscode.window.showTextDocument(doc);
    return;
  }
  if (!scaffolded.alreadyHadBlock) {
    try {
      fs.writeFileSync(workspaceFilePath, scaffolded.content, "utf-8");
      outputChannel?.appendLine(
        `[sqry] Scaffolded sqry.workspace block in ${workspaceFilePath}`,
      );
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      void vscode.window.showErrorMessage(`sqry: cannot write workspace file: ${message}`);
      return;
    }
  }
  const doc = await vscode.workspace.openTextDocument(workspaceFilePath);
  await vscode.window.showTextDocument(doc);
}

/**
 * Fetch the logical-workspace info via `sqry/workspaceStatus` and emit
 * the STEP_12 single startup telemetry line.
 *
 * Delegates to the shared
 * [`emitWorkspaceResolutionTelemetry`](./workspaceTelemetry.ts) helper
 * so the activation path and the unit tests share a single
 * implementation. Failure is logged but never propagated — telemetry
 * is best-effort and must not block activation. STEP_12 codex iter1
 * MINOR fix: the shared helper guarantees `getLogicalWorkspaceInfo()`
 * is called exactly once and `appendLine` is called exactly once
 * (success or failure path), pinned by
 * `sqry-vscode/tests/telemetry.test.ts`.
 */
async function emitWorkspaceResolutionTelemetry(
  activeClient: SqryClient,
  channel: vscode.OutputChannel,
): Promise<void> {
  await sharedEmitWorkspaceResolutionTelemetry(activeClient, channel);
}

/** Wrap `getWorkspaceStatus` with logging — returns null on failure. */
async function safeWorkspaceStatus(): Promise<SqryWorkspaceStatus | null> {
  if (!client) {
    return null;
  }
  try {
    return await client.getWorkspaceStatus();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    outputChannel?.appendLine(`[sqry] getWorkspaceStatus failed: ${message}`);
    return null;
  }
}

function getActiveWorkspaceFolder():
  | vscode.WorkspaceFolder
  | undefined {
  const settings = readSettings();
  const editor = vscode.window.activeTextEditor;
  if (editor) {
    const candidate = vscode.workspace.getWorkspaceFolder(editor.document.uri);
    if (candidate && !isWorkspaceFolderExcluded(candidate, settings)) {
      return candidate;
    }
  }
  // Fall back to the first non-excluded folder (STEP_5 criterion 8).
  return nonExcludedFolders(vscode.workspace.workspaceFolders ?? [], settings)[0];
}

function getAllWorkspaceFolders(): readonly vscode.WorkspaceFolder[] {
  // STEP_5 acceptance criterion 8 — every enumeration loop filters
  // through `isWorkspaceFolderExcluded()`. Centralising the filter
  // here means the rest of the extension never has to remember.
  const all = vscode.workspace.workspaceFolders ?? [];
  return nonExcludedFolders(all);
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

  if (await handleBinaryError(message)) {
    return;
  }
  if (await handleTimeoutError(message)) {
    return;
  }
  if (await handleQueryError(message)) {
    return;
  }

  await vscode.window.showErrorMessage(`sqry error: ${message}`);
}

/**
 * Refresh the aggregate workspace status — the SOLE status surface
 * (DAG STEP_5 acceptance criterion 5). This is the single entrypoint
 * for status synchronization; every path that repopulates status
 * (`onDidChangeWorkspaceFolders`, `onDidChangeConfiguration("sqry")`,
 * `sqry.refreshStats`, post-rebuild) calls this exactly once.
 */
async function refreshWorkspaceStatus(
  activeClient: SqryClient,
): Promise<WorkspaceStatusRefreshResult> {
  try {
    const status = await activeClient.getWorkspaceStatus();
    searchPanel?.setWorkspaceStatus(status);
    statusBar?.updateWorkspace(status);

    const folders = getAllWorkspaceFolders();
    if (folders.length === 1) {
      try {
        const rawStatus = await activeClient.getIndexStatus(folders[0]);
        searchPanel?.hydrateIndexStatus(rawStatus);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        outputChannel?.appendLine(`[sqry] Failed to hydrate index stats: ${message}`);
      }
    }
    return { ok: true };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    outputChannel?.appendLine(`[sqry] Failed to refresh workspace status: ${message}`);
    statusBar?.update(null);
    return { ok: false, error: new Error(message) };
  }
}

/**
 * Drill-down refresh for a single source root (post-rebuild diagnostics
 * and tree drill-down updates). Does NOT replace the aggregate surface
 * — `refreshWorkspaceStatus` follows for that.
 */
async function refreshSourceRootStatus(
  activeClient: SqryClient,
  workspace: vscode.WorkspaceFolder,
): Promise<void> {
  try {
    const status = await activeClient.getSourceRootStatus(workspace);
    outputChannel?.appendLine(
      `[sqry] Source root ${workspace.name}: ${status.status} (symbols=${status.symbol_count ?? "?"})`,
    );

    // After index rebuild, clear stale diagnostics and refresh for open editors
    if (diagnosticsProvider) {
      diagnosticsProvider.clear();
      await diagnosticsProvider.refreshForOpenEditors(workspace);
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    outputChannel?.appendLine(
      `[sqry] Failed to refresh source-root status after rebuild: ${message}`,
    );
  }
}

/**
 * Gate a manual rebuild on `Ready`. Returns `true` when the gate is
 * open and the caller may proceed; throws/handles the timeout error
 * otherwise.
 */
async function awaitReadyOrFail(actionLabel: string): Promise<boolean> {
  if (!loadingState) {
    return true;
  }
  if (loadingState.isReady()) {
    return true;
  }
  if (loadingState.isFailed()) {
    void vscode.window.showErrorMessage(
      `sqry: cannot ${actionLabel} — extension is unavailable. Check the Output panel.`,
    );
    return false;
  }
  try {
    await loadingState.waitForReady(MANUAL_GATE_TIMEOUT_MS);
    return true;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    void vscode.window.showErrorMessage(`sqry: ${message}`);
    return false;
  }
}

async function indexWithProgress(folder: vscode.WorkspaceFolder): Promise<void> {
  const activeClient = client;
  if (!activeClient) {
    return;
  }

  // STEP_5: gate manual rebuild on Ready (DAG criterion 11 timeout
  // contract). The LSP enforces its own build-lock semantics; we no
  // longer probe `.sqry-index.lock` from the extension because that
  // would be per-folder filesystem stat probing — forbidden by
  // criterion 4.
  if (!(await awaitReadyOrFail(`rebuild ${folder.name}`))) {
    return;
  }
  let force = false;
  // Surface the in-progress build via the aggregate status: if the
  // matching source root reports `building`, prompt the user to
  // confirm a force rebuild. This replaces the legacy filesystem
  // stat probe.
  try {
    const status = await activeClient.getSourceRootStatus(folder);
    if (status.status === "building") {
      const action = await vscode.window.showWarningMessage(
        `sqry: Index build already in progress for ${folder.name}`,
        "Wait",
        "Force Rebuild",
      );
      if (action !== "Force Rebuild") {
        return;
      }
      force = true;
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    outputChannel?.appendLine(`[sqry] Pre-rebuild status check failed: ${message}`);
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
    await refreshSourceRootStatus(activeClient, folder);
    await refreshWorkspaceStatus(activeClient);
  } catch (error) {
    await handleError(error);
  } finally {
    // Clean up: ensure timer is cleared and subscription is disposed
    progressReceived = true;
    clearTimeout(fallbackTimer);
    progressSubscription.dispose();
  }
}
