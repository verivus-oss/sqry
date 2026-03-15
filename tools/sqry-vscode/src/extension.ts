import * as vscode from "vscode";
import { readSettings } from "./config";
import { downloadBinary, findExistingBinary, getBinaryVersion, detectPlatform } from "./binaryDownloader";
import { SqryCodeLensProvider } from "./codeLens";
import { SearchPanel } from "./searchPanel";
import { SqryClient } from "./sqryClient";

let client: SqryClient | undefined;
let outputChannel: vscode.OutputChannel | undefined;
let searchPanel: SearchPanel | undefined;

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
      const workspace = getActiveWorkspaceFolder();
      if (!workspace) {
        void vscode.window.showWarningMessage(
          "sqry: No workspace folder detected. Open a folder before indexing.",
        );
        return;
      }

      // Progress is handled by LSP WorkDoneProgress notifications from the server.
      // The vscode-languageclient automatically displays these in the notification area.
      try {
        await activeClient.runIndex(workspace);
        // Success message is sent by LSP server via window/showMessage with symbol counts
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
        outputChannel?.appendLine(`[sqry] Refreshed index stats: ${status.symbol_count} symbols, ${status.file_count} files`);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        outputChannel?.appendLine(`[sqry] Failed to refresh stats: ${message}`);
        void vscode.window.showWarningMessage(`sqry: Failed to refresh stats: ${message}`);
      }
    }),
    vscode.commands.registerCommand("sqry.clearResults", () => {
      if (searchPanel) {
        searchPanel.clearResults();
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
    vscode.workspace.onDidChangeWorkspaceFolders(() => {
      void maybeAutoIndex();
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
      return true;
    }

    outputChannel?.appendLine(
      `[sqry] Index validation failed for ${folder.name}: exists=${status.exists}, symbols=${status.symbol_count}`,
    );
    return false;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    outputChannel?.appendLine(`[sqry] Index validation error for ${folder.name}: ${message}`);
    // Return undefined (not false) so the filesystem fallback runs.
    // The LSP may not be ready yet at startup.
    return undefined;
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
    outputChannel?.appendLine(`[sqry] Checking index for folder: ${folder.name} at ${folder.uri.fsPath}`);

    // Prefer LSP validation — it knows the actual index location (.sqry/graph/)
    const lspResult = await validateIndexViaLSP(folder);
    if (lspResult !== undefined) {
      return lspResult;
    }

    // Fallback: check filesystem when LSP is not available
    const manifestPath = vscode.Uri.joinPath(folder.uri, ".sqry", "graph", "manifest.json");
    const lockPath = vscode.Uri.joinPath(folder.uri, ".sqry-index.lock");

    try {
      const manifestStat = await vscode.workspace.fs.stat(manifestPath);
      outputChannel?.appendLine(`[sqry] Manifest exists at ${manifestPath.fsPath}`);
      return !isIndexStale(manifestStat.mtime, folder.name);
    } catch {
      outputChannel?.appendLine(`[sqry] Manifest not found at ${manifestPath.fsPath}`);
      const lockActive = await isLockFileActive(lockPath, folder.name);
      return lockActive; // Return true only if build is in progress
    }
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
    }
  });

  // Progress is handled by LSP WorkDoneProgress notifications from the server.
  // The vscode-languageclient automatically displays these in the notification area.
  try {
    await activeClient.runIndex(folder, force);
    // Success message is sent by LSP server via window/showMessage with symbol counts
  } catch (error) {
    await handleError(error);
  } finally {
    // Clean up: ensure timer is cleared and subscription is disposed
    progressReceived = true;
    clearTimeout(fallbackTimer);
    progressSubscription.dispose();
  }
}
