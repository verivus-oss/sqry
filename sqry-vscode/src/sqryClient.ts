import * as vscode from "vscode";
import { randomUUID } from "node:crypto";
import {
  CancellationTokenSource,
  ExecuteCommandRequest,
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";
import { handleExecuteCommandResult } from "./commandResults";
import { ResolvedSqryConfig, resolveConfig } from "./config";
import { IndexQueue } from "./indexQueue";
import { SqryResult, SqrySymbolResult } from "./types";
import {
  SortOrder,
  SqryAggregateIndexStatus,
  SqryIndexStatus,
  SqryIndexStatusParams,
  SqryIndexStatusResult,
  SqryListCrossLanguageRelationsParams,
  SqryListCrossLanguageRelationsResult,
  SqryListFilesByLanguageParams,
  SqryListFilesByLanguageResult,
  SqryListFilesParams,
  SqryListFilesResult,
  SqryListSymbolsParams,
  SqryListSymbolsResult,
  SqryRelationResult,
  SqrySearchParams,
  SqrySearchResult,
  SqryListDuplicateGroupsParams,
  SqryListDuplicateGroupsResult,
  SqryListCircularDependenciesParams,
  SqryListCircularDependenciesResult,
  SqryListUnusedSymbolsParams,
  SqryListUnusedSymbolsResult,
  SqryBatchCallerCalleeCountParams,
  SqryBatchCallerCalleeCountResult,
  SqryLogicalWorkspaceInfo,
  SqrySourceRootStatus,
  SqrySymbolRef,
  SqryWorkspaceStatus,
  SqryWorkspaceStatusParams,
} from "./lspProtocol";

const INITIALIZATION_TIMEOUT_MS = 10_000;

interface ActiveRequest {
  /** Cancellation source — disposed when the local request lifecycle settles. */
  readonly source: CancellationTokenSource;
  /** Settles the caller locally while cancelling the protocol request best effort. */
  readonly abort: (reason: Error) => void;
}

/**
 * Optional initialization-options payload sent to the LSP on start-up.
 *
 * STEP_5 (codex iter1 MAJOR fix) forwards two distinct shapes:
 *
 * - `workspace`: the parsed + classified `.code-workspace` payload
 *   produced by the extension's `workspaceClassifier` (folders +
 *   inline `sqry.workspace` block). The LSP attempts to deserialize
 *   this as a `LogicalWorkspace`; when the shape is the lightweight
 *   classification hint the extension produces, the LSP falls through
 *   to the path-based branch (see `sqry-lsp/src/session.rs`
 *   `resolve_step_1` + `resolve_step_4`). Always an OBJECT, never a
 *   path string.
 * - `workspaceFile`: absolute fsPath of the active `.code-workspace`,
 *   passed verbatim to the LSP's `resolve_step_4` branch which loads
 *   the file from disk and runs the heuristic classifier in-process.
 *   Always a PATH STRING, never an object.
 *
 * The LSP only reads `initializationOptions` once during the LSP
 * `initialize` handshake; callers MUST set this before
 * `SqryClient.initialize()`.
 */
export interface SqryInitializationOptions {
  /** Parsed/classified `.code-workspace` payload — see the JSDoc above. */
  readonly workspace?: SqryWorkspaceInitializationPayload;
  /** Absolute path to the `.code-workspace` file. */
  readonly workspaceFile?: string;
  /**
   * Absolute path forwarded from the `sqry.indexRoot` VS Code setting.
   *
   * STEP_10 iter3 wire-up. The extension reads the user's
   * `sqry.indexRoot` setting at activation and forwards the value here
   * so the LSP can use it as the canonical workspace identity (the
   * in-band replacement for the legacy `--index-root` CLI flag). The
   * LSP picks this up in `extract_sqry_init_options` and feeds it into
   * `WorkspaceResolutionInputs.index_root` when no explicit
   * `--index-root` flag was passed on the command line. Empty/absent
   * means "no override; auto-detect".
   */
  readonly indexRoot?: string;
}

/**
 * Lightweight classification hint produced by the extension and sent
 * under `initializationOptions.sqry.workspace`. Mirrors the
 * `ParsedWorkspaceFile` shape from `workspaceClassifier.ts` but is
 * declared here so `SqryClient` does not depend on a UI-side module.
 */
export interface SqryWorkspaceInitializationPayload {
  /** Verbatim `folders[]` array from the `.code-workspace`. */
  readonly folders: ReadonlyArray<{
    readonly path: string;
    readonly name?: string;
  }>;
  /**
   * The `sqry.workspace` block from the `.code-workspace`, when set by
   * the user. `null` means the file did not contain the block — the
   * LSP should fall through to the heuristic classifier on the file
   * itself (branch 4).
   */
  readonly classification: {
    readonly sourceRoots?: ReadonlyArray<string>;
    readonly exclusions?: ReadonlyArray<string>;
    readonly memberFolders?: ReadonlyArray<string>;
    readonly projectRootMode?: "gitRoot" | "folder" | "explicit";
  } | null;
}

// Progress notification handler type
type ProgressHandler = (token: string) => void;

export class SqryClient implements vscode.Disposable {
  private config: ResolvedSqryConfig | null = null;
  private readonly outputChannel: vscode.OutputChannel;
  private readonly indexQueue = new IndexQueue();
  private readonly onDidChangeConfigEmitter =
    new vscode.EventEmitter<ResolvedSqryConfig>();
  private readonly disposables: vscode.Disposable[] = [];

  private languageClient: LanguageClient | null = null;
  private currentBinaryPath: string | null = null;
  private downloadedBinaryPath: string | null = null;
  /** Once disposed, delayed request continuations must never restart or use the LSP. */
  private isDisposed = false;
  /** Active only while `initialize` owns configuration/client startup. */
  private initializationSource: CancellationTokenSource | undefined;
  /** Candidate-start owner, retained until the local client is assigned or stopped. */
  private activeStartSource: CancellationTokenSource | undefined;
  /** Serialize every stop/start transaction so local candidates cannot race. */
  private restartTail: Promise<void> = Promise.resolve();
  /**
   * In-flight request set. Each request owns its own cancellation
   * token + local settlement pair, so concurrent requests do NOT race on a
   * shared `currentRequest` slot. `dispose()` cancels every member.
   *
   * STEP_5 acceptance criterion 6 — concurrent status requests must
   * be safe to issue in parallel. `cancelActiveRequest` is reduced
   * to a `dispose()`-time helper that drains the set.
   */
  private readonly activeRequests = new Set<ActiveRequest>();
  private readonly progressHandlers: Map<string, ProgressHandler> = new Map();
  /** Initialization options forwarded to the LSP on each start. */
  private initializationOptions: SqryInitializationOptions = {};

  public readonly onDidChangeConfig = this.onDidChangeConfigEmitter.event;

  constructor(
    outputChannel: vscode.OutputChannel,
    private readonly initializationTimeoutMs = INITIALIZATION_TIMEOUT_MS,
  ) {
    this.outputChannel = outputChannel;

    const configListener = vscode.workspace.onDidChangeConfiguration(async (event) => {
      if (event.affectsConfiguration("sqry")) {
        this.outputChannel.appendLine("[sqry] Configuration changed, reloading...");
        try {
          await this.refreshConfig();
          this.outputChannel.appendLine("[sqry] Configuration reloaded successfully");
        } catch (error) {
          const message = error instanceof Error ? error.message : String(error);
          this.outputChannel.appendLine(`[sqry] Configuration reload failed: ${message}`);
          void vscode.window.showWarningMessage(
            `sqry configuration reload failed: ${message}`,
            "View Output"
          ).then((selection) => {
            if (selection === "View Output") {
              this.outputChannel.show();
            }
          });
        }
      }
    });

    this.disposables.push(configListener, this.onDidChangeConfigEmitter);
  }

  /**
   * Set the path to an auto-downloaded binary. This is used as a fallback
   * when the binary is not found on PATH.
   */
  public setDownloadedBinaryPath(binaryPath: string): void {
    this.downloadedBinaryPath = binaryPath;
  }

  /**
   * Initialize the language client. Must be called after construction and
   * before using any other methods.
   *
   * @throws {Error} If configuration cannot be resolved or language client fails to start
   */
  public async initialize(): Promise<void> {
    this.outputChannel.appendLine("[sqry] Initializing language client...");

    const initializationSource = new CancellationTokenSource();
    this.initializationSource = initializationSource;
    let initializationTimer: NodeJS.Timeout | undefined;
    try {
      // Resolve configuration with an owned timeout token. A late resolver or
      // LSP start must not outlive a failed activation and create a client
      // after the caller has already seen the initialization timeout.
      const configPromise = this.refreshConfig(initializationSource.token);
      const timeoutPromise = new Promise<never>((_, reject) =>
        initializationTimer = setTimeout(() => {
          initializationSource.cancel();
          reject(new Error("Configuration timeout after 10s"));
        }, this.initializationTimeoutMs),
      );

      await Promise.race([configPromise, timeoutPromise]);

      if (!this.config) {
        throw new Error(
          "Configuration failed to initialize. Ensure 'sqry' binary is in PATH or set 'sqry.path' in settings."
        );
      }

      if (!this.languageClient) {
        throw new Error(
          "Language client failed to start. Check the Output panel (View > Output > Sqry) for details."
        );
      }

      this.outputChannel.appendLine("[sqry] Language client initialized successfully");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.outputChannel.appendLine(`[sqry] Initialization failed: ${message}`);
      throw error;
    } finally {
      if (initializationTimer) {
        clearTimeout(initializationTimer);
      }
      if (this.initializationSource === initializationSource) {
        this.initializationSource = undefined;
      }
      initializationSource.dispose();
    }
  }

  public dispose(): void {
    if (this.isDisposed) {
      return;
    }
    this.isDisposed = true;
    this.initializationSource?.cancel();
    this.activeStartSource?.cancel();
    this.cancelAllRequests();
    void this.stopLanguageClient();
    this.disposables.forEach((disposable) => disposable.dispose());
  }

  /**
   * Set the initialization options forwarded to the LSP. Must be
   * called before [`initialize`]; later changes require a restart
   * (the LSP only reads `initializationOptions` once).
   */
  public setInitializationOptions(options: SqryInitializationOptions): void {
    this.initializationOptions = { ...options };
  }

  public async refreshConfig(cancellationToken?: vscode.CancellationToken): Promise<void> {
    this.throwIfRequestCannotProceed(cancellationToken);
    const newConfig = await resolveConfig(this.downloadedBinaryPath ?? undefined);
    this.throwIfRequestCannotProceed(cancellationToken);
    const binaryChanged =
      this.config?.resolvedBinaryPath !== newConfig.resolvedBinaryPath;
    this.config = newConfig;
    this.onDidChangeConfigEmitter.fire(newConfig);

    if (binaryChanged || !this.languageClient) {
      this.outputChannel.appendLine(`[sqry] Binary resolved: ${newConfig.resolvedBinaryPath}`);
    }

    if (!this.languageClient || binaryChanged) {
      await this.restartLanguageClient(newConfig, cancellationToken);
    }
  }

  public async runIndex(workspace: vscode.WorkspaceFolder, force = false): Promise<void> {
    const cfg = await this.ensureConfig();
    const key = workspace.uri.fsPath;
    await this.indexQueue.run(key, async () => {
      await this.sendExecuteCommand("sqry.index", [workspace.uri.fsPath, force], cfg, cfg.indexTimeoutMs);
    });
  }

  /**
   * Subscribe to progress notifications for sqry index operations.
   * The handler is called when a progress notification with a sqry-index-* token is received.
   * Returns a disposable to unsubscribe.
   */
  public onIndexProgress(handler: ProgressHandler): vscode.Disposable {
    const id = `progress-${Date.now()}-${randomUUID()}`;
    this.progressHandlers.set(id, handler);
    return {
      dispose: () => {
        this.progressHandlers.delete(id);
      },
    };
  }

  /**
   * Fetch the aggregate workspace status — the SOLE status surface for
   * the extension UI (DAG STEP_5 criterion 5).
   *
   * Routes through `sqry/workspaceStatus`, the LSP surface that owns
   * logical workspace identity and source-root aggregation. `sqry/indexStatus`
   * remains available through `getIndexStatus()` for per-root rich stats.
   */
  public async getWorkspaceStatus(): Promise<SqryWorkspaceStatus> {
    return (await this.getLogicalWorkspaceInfo()).aggregate;
  }

  /**
   * Fetch the raw index status payload for UI surfaces that need the
   * complete stats projection (files, languages, relation summaries).
   */
  public async getIndexStatus(
    workspace?: vscode.WorkspaceFolder,
  ): Promise<SqryAggregateIndexStatus> {
    const cfg = await this.ensureConfig();
    const params: SqryIndexStatusParams = { path: workspace?.uri.fsPath };
    const result = await this.sendRequest<SqryIndexStatusResult>(
      "sqry/indexStatus",
      params,
      cfg,
    );
    return result.status;
  }

  /**
   * STEP_12 telemetry — fetch the logical-workspace identity + structure
   * projection for the active workspace. Returns the LSP's
   * `sqry/workspaceStatus` payload verbatim, including both the
   * scannable `workspace_id_short` (16 hex) and the machine-identity
   * `workspace_id_full` (64 hex). Callers (extension activation) use
   * this to emit ONE aggregate startup line per the DAG contract.
   */
  public async getLogicalWorkspaceInfo(): Promise<SqryLogicalWorkspaceInfo> {
    const cfg = await this.ensureConfig();
    const params: SqryWorkspaceStatusParams = {};
    return await this.sendRequest<SqryLogicalWorkspaceInfo>(
      "sqry/workspaceStatus",
      params,
      cfg,
    );
  }

  /**
   * Drill-down into a single source root. Used by per-root rebuild
   * operations and by the search/results panel when the user expands
   * one root inside a multi-root workspace.
   *
   * Returns the matching `SourceRootStatus` from the aggregate when
   * available (preserves freshness consistent with the rest of the
   * UI); falls back to a synthesized entry for the single-folder
   * branch where the LSP returned a per-folder `IndexStatus`.
   */
  public async getSourceRootStatus(
    folder: vscode.WorkspaceFolder,
  ): Promise<SqrySourceRootStatus> {
    const cfg = await this.ensureConfig();
    const params: SqryIndexStatusParams = { path: folder.uri.fsPath };
    const result = await this.sendRequest<SqryIndexStatusResult>(
      "sqry/indexStatus",
      params,
      cfg,
    );
    return this.extractSourceRootStatus(result.status, folder.uri.fsPath);
  }

  private extractSourceRootStatus(
    status: SqryAggregateIndexStatus,
    requestedPath: string,
  ): SqrySourceRootStatus {
    const aggregate = status.aggregate;
    if (aggregate) {
      const match = aggregate.source_root_statuses.find((s) => s.path === requestedPath);
      if (match) {
        return match;
      }
      // Member-folder branch with no exact match — synthesize a
      // composite status from the aggregate counts.
      if (aggregate.error_count > 0) {
        return { path: requestedPath, status: "error" };
      }
      if (aggregate.building_count > 0) {
        return { path: requestedPath, status: "building" };
      }
      if (aggregate.missing_count > 0) {
        return { path: requestedPath, status: "missing" };
      }
      return { path: requestedPath, status: "ok" };
    }
    return {
      path: requestedPath,
      status: this.classifyLegacyStatus(status),
      symbol_count: status.symbol_count,
    };
  }

  private classifyLegacyStatus(
    status: SqryIndexStatus,
  ): "ok" | "missing" | "building" | "error" {
    if (status.building) {
      return "building";
    }
    if (status.exists && (status.symbol_count ?? 0) > 0) {
      return "ok";
    }
    return "missing";
  }

  public async runQuery(
    query: string,
    workspace?: vscode.WorkspaceFolder,
  ): Promise<SqryResult> {
    const cfg = await this.ensureConfig();
    const params: SqrySearchParams = {
      query,
      path: workspace?.uri.fsPath,
      limit: cfg.limit,
    };

    const response = await this.sendRequest<SqrySearchResult>(
      "sqry/search",
      params,
      cfg,
    );
    return this.toSqryResult(response);
  }

  public async runSearch(
    query: string,
    workspace?: vscode.WorkspaceFolder,
  ): Promise<SqryResult> {
    // The current LSP surface exposes semantic search; reuse runQuery until
    // dedicated text search surfaces are added.
    return this.runQuery(query, workspace);
  }

  /**
   * List symbols in the index with pagination support.
   * Uses the dedicated sqry/listSymbols endpoint.
   * @param workspace - Optional workspace folder to scope the query
   * @param offset - Pagination offset
   * @param limit - Maximum number of symbols to return
   * @param kind - Optional filter by symbol kind (e.g., "function", "class")
   */
  public async listSymbols(
    workspace?: vscode.WorkspaceFolder,
    offset?: number,
    limit?: number,
    kind?: string,
  ): Promise<SqryListSymbolsResult> {
    const cfg = await this.ensureConfig();
    const params: SqryListSymbolsParams = {
      path: workspace?.uri.fsPath,
      offset,
      limit: limit ?? cfg.limit,
      kind,
    };

    this.outputChannel.appendLine(
      `[sqry] listSymbols (path=${params.path ?? "default"}, offset=${offset ?? 0}, limit=${params.limit ?? "default"}, kind=${kind ?? "all"})`,
    );

    return this.sendRequest<SqryListSymbolsResult>(
      "sqry/listSymbols",
      params,
      cfg,
    );
  }

  /**
   * List files in the index with pagination support.
   * Uses the dedicated sqry/listFiles endpoint.
   */
  public async listFiles(
    workspace?: vscode.WorkspaceFolder,
    offset?: number,
    limit?: number,
  ): Promise<SqryListFilesResult> {
    const cfg = await this.ensureConfig();
    const params: SqryListFilesParams = {
      path: workspace?.uri.fsPath,
      offset,
      limit: limit ?? cfg.limit,
    };

    this.outputChannel.appendLine(
      `[sqry] listFiles (path=${params.path ?? "default"}, offset=${offset ?? 0}, limit=${params.limit ?? "default"})`,
    );

    return this.sendRequest<SqryListFilesResult>(
      "sqry/listFiles",
      params,
      cfg,
    );
  }

  /**
   * @deprecated Use listSymbols() instead for pagination support
   */
  public async listAllSymbols(
    workspace?: vscode.WorkspaceFolder,
    limit?: number,
  ): Promise<SqrySymbolResult[]> {
    const result = await this.listSymbols(workspace, 0, limit);
    return result.symbols.map((item) => {
      const uri = vscode.Uri.parse(item.location.uri);
      const filePath = uri.scheme === "file" ? uri.fsPath : uri.toString();
      const startLine = item.location.range.start.line + 1;
      return {
        name: item.name,
        kind: item.kind ?? "",
        qualifiedName: item.qualified_name ?? "",
        language: item.language ?? "",
        filePath,
        startLine,
      };
    });
  }

  /**
   * @deprecated Use listFiles() instead for pagination support
   */
  public async listAllFiles(
    workspace?: vscode.WorkspaceFolder,
    limit?: number,
  ): Promise<string[]> {
    const result = await this.listFiles(workspace, 0, limit);
    return result.files;
  }

  /**
   * List files filtered by language with pagination support.
   * Uses the dedicated sqry/listFilesByLanguage endpoint.
   */
  public async listFilesByLanguage(
    language: string,
    workspace?: vscode.WorkspaceFolder,
    offset?: number,
    limit?: number,
  ): Promise<SqryListFilesByLanguageResult> {
    const cfg = await this.ensureConfig();
    const params: SqryListFilesByLanguageParams = {
      language,
      path: workspace?.uri.fsPath,
      offset,
      limit: limit ?? cfg.limit,
    };

    this.outputChannel.appendLine(
      `[sqry] listFilesByLanguage (language=${language}, path=${params.path ?? "default"}, offset=${offset ?? 0}, limit=${params.limit ?? "default"})`,
    );

    return this.sendRequest<SqryListFilesByLanguageResult>(
      "sqry/listFilesByLanguage",
      params,
      cfg,
    );
  }

  /**
   * List cross-language relations with pagination and sort support.
   * Uses the dedicated sqry/listCrossLanguageRelations endpoint.
   * @param workspace - Optional workspace folder to scope the query
   * @param offset - Pagination offset
   * @param limit - Maximum number of relations to return
   * @param sortOrder - Sort order for results
   * @param sourceLanguage - Optional filter by source language (e.g., "rust", "go")
   * @param targetLanguage - Optional filter by target language (e.g., "javascript", "python")
   */
  public async listCrossLanguageRelations(
    workspace?: vscode.WorkspaceFolder,
    offset?: number,
    limit?: number,
    sortOrder?: SortOrder,
    sourceLanguage?: string,
    targetLanguage?: string,
  ): Promise<SqryListCrossLanguageRelationsResult> {
    const cfg = await this.ensureConfig();
    const params: SqryListCrossLanguageRelationsParams = {
      path: workspace?.uri.fsPath,
      offset,
      limit: limit ?? cfg.limit,
      sort_order: sortOrder,
      source_language: sourceLanguage,
      target_language: targetLanguage,
    };

    const langFilter = sourceLanguage || targetLanguage
      ? `, filter=${sourceLanguage ?? "*"}→${targetLanguage ?? "*"}`
      : "";
    this.outputChannel.appendLine(
      `[sqry] listCrossLanguageRelations (path=${params.path ?? "default"}, offset=${offset ?? 0}, limit=${params.limit ?? "default"}, sort=${sortOrder ?? "default"}${langFilter})`,
    );

    return this.sendRequest<SqryListCrossLanguageRelationsResult>(
      "sqry/listCrossLanguageRelations",
      params,
      cfg,
    );
  }

  // ===== CD Predicate Client Methods =====

  /**
   * List duplicate symbol groups.
   * Uses the dedicated sqry/listDuplicateGroups endpoint.
   * @param workspace - Optional workspace folder to scope the query
   * @param duplicateType - Type of duplicate to detect: "body", "signature", or "struct"
   * @param limit - Maximum number of groups to return
   */
  public async listDuplicateGroups(
    workspace?: vscode.WorkspaceFolder,
    duplicateType?: string,
    limit?: number,
  ): Promise<SqryListDuplicateGroupsResult> {
    const cfg = await this.ensureConfig();
    const params: SqryListDuplicateGroupsParams = {
      path: workspace?.uri.fsPath,
      duplicate_type: duplicateType ?? "body",
      limit: limit ?? cfg.limit,
    };

    this.outputChannel.appendLine(
      `[sqry] listDuplicateGroups (path=${params.path ?? "default"}, type=${params.duplicate_type ?? "body"}, limit=${params.limit ?? "default"})`,
    );

    return this.sendRequest<SqryListDuplicateGroupsResult>(
      "sqry/listDuplicateGroups",
      params,
      cfg,
    );
  }

  /**
   * List circular dependencies.
   * Uses the dedicated sqry/listCircularDependencies endpoint.
   * @param workspace - Optional workspace folder to scope the query
   * @param circularType - Type of circular dependency: "calls", "imports", or "modules"
   * @param limit - Maximum number of cycles to return
   * @param shouldIncludeSelfLoops - Whether to include self-loops (A -> A)
   */
  public async listCircularDependencies(
    workspace?: vscode.WorkspaceFolder,
    circularType?: string,
    limit?: number,
    shouldIncludeSelfLoops?: boolean,
  ): Promise<SqryListCircularDependenciesResult> {
    const cfg = await this.ensureConfig();
    const params: SqryListCircularDependenciesParams = {
      path: workspace?.uri.fsPath,
      circular_type: circularType ?? "calls",
      limit: limit ?? cfg.limit,
      should_include_self_loops: shouldIncludeSelfLoops ?? false,
    };

    this.outputChannel.appendLine(
      `[sqry] listCircularDependencies (path=${params.path ?? "default"}, type=${params.circular_type ?? "calls"}, limit=${params.limit ?? "default"})`,
    );

    return this.sendRequest<SqryListCircularDependenciesResult>(
      "sqry/listCircularDependencies",
      params,
      cfg,
    );
  }

  /**
   * List unused symbols.
   * Uses the dedicated sqry/listUnusedSymbols endpoint.
   * @param workspace - Optional workspace folder to scope the query
   * @param scope - Scope of unused analysis: "public", "private", "function", "struct", or "all"
   * @param limit - Maximum number of symbols to return
   */
  public async listUnusedSymbols(
    workspace?: vscode.WorkspaceFolder,
    scope?: string,
    limit?: number,
  ): Promise<SqryListUnusedSymbolsResult> {
    const cfg = await this.ensureConfig();
    const params: SqryListUnusedSymbolsParams = {
      path: workspace?.uri.fsPath,
      scope: scope ?? "all",
      limit: limit ?? cfg.limit,
    };

    this.outputChannel.appendLine(
      `[sqry] listUnusedSymbols (path=${params.path ?? "default"}, scope=${params.scope ?? "all"}, limit=${params.limit ?? "default"})`,
    );

    return this.sendRequest<SqryListUnusedSymbolsResult>(
      "sqry/listUnusedSymbols",
      params,
      cfg,
    );
  }

  /**
   * Batch caller/callee count for multiple symbols in one request.
   * Returns counts for each symbol without full result objects, optimized for CodeLens.
   * @param symbols - Symbols to count callers/callees for
   * @param workspace - Optional workspace folder to scope the query
   */
  public async batchCallerCalleeCount(
    symbols: SqrySymbolRef[],
    workspace?: vscode.WorkspaceFolder,
  ): Promise<SqryBatchCallerCalleeCountResult> {
    const cfg = await this.ensureConfig();
    const params: SqryBatchCallerCalleeCountParams = {
      symbols,
      path: workspace?.uri.fsPath,
    };

    this.outputChannel.appendLine(
      `[sqry] batchCallerCalleeCount (symbols=${symbols.length}, path=${params.path ?? "default"})`,
    );

    return this.sendRequest<SqryBatchCallerCalleeCountResult>(
      "sqry/batchCallerCalleeCount",
      params,
      cfg,
    );
  }

  public async restart(): Promise<void> {
    this.throwIfRequestCannotProceed();
    const config = await resolveConfig(this.downloadedBinaryPath ?? undefined);
    this.throwIfRequestCannotProceed();
    this.config = config;
    await this.restartLanguageClient(config);
  }

  private restartLanguageClient(
    config: ResolvedSqryConfig,
    cancellationToken?: vscode.CancellationToken,
  ): Promise<void> {
    const restart = async (): Promise<void> => {
      this.throwIfRequestCannotProceed(cancellationToken);
      await this.stopLanguageClient();
      this.throwIfRequestCannotProceed(cancellationToken);
      await this.startLanguageClient(config, cancellationToken);
    };

    // A configuration update, explicit restart, initialization, and a
    // request-triggered recovery all converge here. Running the complete
    // stop/start transaction through one tail means a previous local
    // candidate is stopped before a later candidate can exist or assign.
    const queuedRestart = this.restartTail.then(restart, restart);
    this.restartTail = queuedRestart.catch(() => undefined);
    return queuedRestart;
  }

  private async startLanguageClient(
    config: ResolvedSqryConfig,
    cancellationToken?: vscode.CancellationToken,
  ): Promise<void> {
    this.throwIfRequestCannotProceed(cancellationToken);
    if (this.activeStartSource) {
      throw new Error("sqry language client start overlap violated the restart queue invariant.");
    }
    const serverOptions: ServerOptions = {
      run: {
        command: config.resolvedBinaryPath,
        args: ["lsp", "--stdio"],
        options: {
          env: {
            ...process.env,
          },
        },
      },
      debug: {
        command: config.resolvedBinaryPath,
        args: ["lsp", "--stdio"],
        options: {
          env: {
            ...process.env,
            RUST_LOG: process.env.RUST_LOG ?? "sqry_lsp=debug",
          },
        },
      },
    };

    // STEP_5 acceptance criterion 7: do NOT pin
    // `LanguageClientOptions.workspaceFolder` to `workspaceFolders[0]`.
    // The per-request `path` parameter is the routing key the LSP uses
    // to resolve which logical workspace + source root to serve from.
    // Pinning the first folder confuses the LSP in multi-root setups
    // (the pinned folder shadows every member folder for the lifetime
    // of the language client).
    const initializationOptions =
      Object.keys(this.initializationOptions).length > 0
        ? { sqry: { ...this.initializationOptions } }
        : undefined;
    const clientOptions: LanguageClientOptions = {
      documentSelector: [{ scheme: "file" }],
      outputChannel: this.outputChannel,
      synchronize: {
        configurationSection: "sqry",
      },
      // Render the results of the server-owned CodeLens/CodeAction commands
      // (callers/references/explain) that vscode-languageclient auto-registers.
      // Without this seam their server results are computed and discarded.
      middleware: {
        executeCommand: (command, args, next) =>
          handleExecuteCommandResult(command, args, next, this.outputChannel),
      },
      ...(initializationOptions ? { initializationOptions } : {}),
    };

    let startTimer: NodeJS.Timeout | undefined;
    const startCancellationDisposables: vscode.Disposable[] = [];
    const languageClient = new LanguageClient(
      "sqryLanguageServer",
      "Sqry Language Server",
      serverOptions,
      clientOptions,
    );
    const startSource = new CancellationTokenSource();
    this.activeStartSource = startSource;
    try {
      // Keep the candidate local until its startup owner is still live. A
      // request timeout, initialization timeout, or extension disposal must
      // stop a delayed candidate immediately instead of letting it run until
      // this independent 30s watchdog eventually fires.
      const startPromise = languageClient.start();
      const timeoutPromise = new Promise<never>((_, reject) =>
        startTimer = setTimeout(
          () => reject(new Error("Language server failed to start within 30 seconds")),
          30000
        ),
      );
      const pendingStarts: Array<Promise<void>> = [startPromise, timeoutPromise];
      const addCancellationRace = (
        token: vscode.CancellationToken,
        message: string,
      ): void => {
        const cancellationPromise = new Promise<never>((_resolve, reject) => {
          const rejectForCancellation = (): void => {
            reject(new Error(message));
          };
          if (token.isCancellationRequested) {
            rejectForCancellation();
            return;
          }
          const cancellationDisposable = token.onCancellationRequested(rejectForCancellation);
          startCancellationDisposables.push(cancellationDisposable);
          if (token.isCancellationRequested) {
            cancellationDisposable.dispose();
            rejectForCancellation();
          }
        });
        pendingStarts.push(cancellationPromise);
      };
      addCancellationRace(
        startSource.token,
        "sqry language client was cancelled because the extension is deactivating.",
      );
      if (cancellationToken) {
        addCancellationRace(
          cancellationToken,
          "sqry request was cancelled while starting the language client.",
        );
      }

      await Promise.race(pendingStarts);
      this.throwIfRequestCannotProceed(cancellationToken);
    } catch (error) {
      this.outputChannel.appendLine(
        `[sqry] Failed to start language server: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
      // Clean up the failed client
      try {
        await languageClient.stop();
      } catch {
        // Ignore errors during cleanup
      }
      throw error;
    } finally {
      if (startTimer) {
        clearTimeout(startTimer);
      }
      for (const cancellationDisposable of startCancellationDisposables) {
        cancellationDisposable.dispose();
      }
      if (this.activeStartSource === startSource) {
        this.activeStartSource = undefined;
      }
      startSource.dispose();
    }

    this.languageClient = languageClient;
    this.currentBinaryPath = config.resolvedBinaryPath;

    // Set up progress notification listener for sqry-index-* tokens
    languageClient.onNotification('$/progress', (params: { token: string | number }) => {
      const token = String(params.token);
      if (token.startsWith('sqry-index-')) {
        // Notify all registered progress handlers
        for (const handler of this.progressHandlers.values()) {
          try {
            handler(token);
          } catch (error) {
            this.outputChannel.appendLine(
              `[sqry] Progress handler error: ${error instanceof Error ? error.message : String(error)}`
            );
          }
        }
      }
    });

    this.outputChannel.appendLine(
      `[sqry] Language server ready: ${config.resolvedBinaryPath} lsp --stdio`,
    );
  }

  private async stopLanguageClient(): Promise<void> {
    if (this.languageClient) {
      try {
        await this.languageClient.stop();
      } catch (error) {
        this.outputChannel.appendLine(
          `[sqry] Failed to stop language server: ${
            error instanceof Error ? error.message : String(error)
          }`,
        );
      }
    }
    this.languageClient = null;
    this.currentBinaryPath = null;
  }

  private async getLanguageClient(
    cancellationToken?: vscode.CancellationToken,
  ): Promise<LanguageClient> {
    this.throwIfRequestCannotProceed(cancellationToken);
    const cfg = await this.ensureConfig(cancellationToken);
    this.throwIfRequestCannotProceed(cancellationToken);
    if (
      !this.languageClient ||
      this.currentBinaryPath !== cfg.resolvedBinaryPath
    ) {
      await this.restartLanguageClient(cfg, cancellationToken);
    }

    this.throwIfRequestCannotProceed(cancellationToken);
    if (!this.languageClient) {
      throw new Error("sqry language client is not available.");
    }
    return this.languageClient;
  }

  private async sendRequest<T>(
    method: string,
    params: unknown,
    cfg: ResolvedSqryConfig,
  ): Promise<T> {
    this.outputChannel.appendLine(
      `[sqry] Request ${method} (${this.describeParams(params)})`,
    );
    return this.withLocalDeadline(
      cfg.timeoutMs,
      () => new Error(
        `sqry request timed out after ${cfg.timeoutMs}ms. Increase \`sqry.timeoutMs\` in settings.`,
      ),
      async (token) => {
        const client = await this.getLanguageClient(token);
        this.throwIfRequestCannotProceed(token);
        return client.sendRequest<T>(method, params, token);
      },
    );
  }

  private async sendExecuteCommand(
    command: string,
    args: unknown[],
    cfg: ResolvedSqryConfig,
    timeoutMs?: number,
  ): Promise<void> {
    const timeout = timeoutMs ?? cfg.timeoutMs;
    this.outputChannel.appendLine(
      `[sqry] Command ${command} (${args.map(String).join(" ")})`,
    );
    const settingName = timeoutMs === cfg.indexTimeoutMs
      ? "sqry.indexTimeoutMs"
      : "sqry.timeoutMs";
    await this.withLocalDeadline(
      timeout,
      () => new Error(
        `sqry command timed out after ${timeout}ms. Increase \`${settingName}\` in settings.`,
      ),
      async (token) => {
        const client = await this.getLanguageClient(token);
        this.throwIfRequestCannotProceed(token);
        await client.sendRequest(
          ExecuteCommandRequest.type,
          {
            command,
            arguments: args,
          },
          token,
        );
      },
    );
  }

  /**
   * Give an LSP request a locally enforced lifetime.
   *
   * JSON-RPC cancellation is advisory: a peer may retain the original
   * request promise after receiving `$/cancelRequest`. The caller must still
   * settle on timeout and during extension disposal, so the local promise
   * races the transport work and owns its cleanup independently. The
   * transport promise is deliberately observed after local settlement to
   * avoid an unhandled late rejection from a stopped client.
   */
  private async withLocalDeadline<T>(
    timeoutMs: number,
    timeoutError: () => Error,
    operation: (token: vscode.CancellationToken) => Promise<T>,
  ): Promise<T> {
    const source = new CancellationTokenSource();
    let rejectLocally: (reason: Error) => void = () => undefined;
    let locallySettled = false;
    const localSettlement = new Promise<never>((_resolve, reject) => {
      rejectLocally = reject;
    });
    const abort = (reason: Error): void => {
      if (locallySettled) {
        return;
      }
      locallySettled = true;
      // Reject before signalling cancellation so the stable, actionable local
      // error wins even if the transport rejects synchronously in response.
      rejectLocally(reason);
      source.cancel();
    };
    const request: ActiveRequest = { source, abort };
    const timer = setTimeout(() => {
      abort(timeoutError());
    }, timeoutMs);
    this.activeRequests.add(request);

    try {
      const transport = operation(source.token);
      // Promise.race registers a rejection handler, but retaining an explicit
      // observer makes the late-settlement contract clear and robust if this
      // helper changes later.
      void transport.catch(() => undefined);
      return await Promise.race([transport, localSettlement]);
    } finally {
      locallySettled = true;
      clearTimeout(timer);
      this.activeRequests.delete(request);
      source.dispose();
    }
  }

  private toSqryResult(response: SqrySearchResult | SqryRelationResult): SqryResult {
    const symbols: SqrySymbolResult[] = response.results.map((item) => {
      const uri = vscode.Uri.parse(item.location.uri);
      const filePath = uri.scheme === "file" ? uri.fsPath : uri.toString();
      const startLine = item.location.range.start.line + 1;
      return {
        name: item.name,
        kind: item.kind,
        qualifiedName: item.qualified_name,
        language: item.language,
        filePath,
        startLine,
      };
    });

    return {
      symbols,
      textMatches: [],
      raw: response,
    };
  }

  private describeParams(params: unknown): string {
    if (!params || typeof params !== "object") {
      return "no-params";
    }
    const anyParams = params as Record<string, unknown>;
    const parts: string[] = [];
    if (typeof anyParams.query === "string" && anyParams.query.length) {
      parts.push(`query="${anyParams.query}"`);
    }
    if (typeof anyParams.target === "string" && anyParams.target.length) {
      parts.push(`target="${anyParams.target}"`);
    }
    if (typeof anyParams.path === "string" && anyParams.path.length) {
      parts.push(`path=${anyParams.path}`);
    }
    if (typeof anyParams.limit === "number") {
      parts.push(`limit=${anyParams.limit}`);
    }
    return parts.length ? parts.join(" ") : "no-params";
  }

  /**
   * Cancel every in-flight request. Used by `dispose()` only —
   * concurrent requests do NOT cancel each other.
   *
   * STEP_5 acceptance criterion 6 — the legacy `cancelActiveRequest`
   * was renamed to make the "cancel everything" semantics explicit.
   */
  private cancelAllRequests(): void {
    for (const request of [...this.activeRequests]) {
      request.abort(
        new Error("sqry request cancelled because the extension is deactivating."),
      );
    }
  }

  private throwIfRequestCannotProceed(
    cancellationToken?: vscode.CancellationToken,
  ): void {
    if (this.isDisposed) {
      throw new Error("sqry language client is disposed.");
    }
    if (cancellationToken?.isCancellationRequested) {
      throw new Error("sqry request was cancelled before it could be sent.");
    }
  }

  private async ensureConfig(
    cancellationToken?: vscode.CancellationToken,
  ): Promise<ResolvedSqryConfig> {
    this.throwIfRequestCannotProceed(cancellationToken);
    if (this.config) {
      return this.config;
    }
    await this.refreshConfig(cancellationToken);
    this.throwIfRequestCannotProceed(cancellationToken);
    if (!this.config) {
      throw new Error(
        "sqry configuration is unavailable. Ensure 'sqry' binary is in PATH or set 'sqry.path' in settings.",
      );
    }
    return this.config;
  }
}
