import * as vscode from "vscode";
import { randomUUID } from "node:crypto";
import {
  CancellationTokenSource,
  ExecuteCommandRequest,
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";
import { ResolvedSqryConfig, resolveConfig } from "./config";
import { IndexQueue } from "./indexQueue";
import { SqryResult, SqrySymbolResult } from "./types";
import {
  SortOrder,
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
} from "./lspProtocol";

type ActiveRequest = {
  cancel: () => void;
  timer: NodeJS.Timeout | null;
};

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
  private currentRequest: ActiveRequest | null = null;
  private readonly progressHandlers: Map<string, ProgressHandler> = new Map();

  public readonly onDidChangeConfig = this.onDidChangeConfigEmitter.event;

  constructor(outputChannel: vscode.OutputChannel) {
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

    try {
      // Resolve configuration with explicit timeout
      const configPromise = this.refreshConfig();
      const timeoutPromise = new Promise<never>((_, reject) =>
        setTimeout(() => reject(new Error("Configuration timeout after 10s")), 10000)
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
    }
  }

  public dispose(): void {
    this.cancelActiveRequest();
    void this.stopLanguageClient();
    this.disposables.forEach((disposable) => disposable.dispose());
  }

  public async refreshConfig(): Promise<void> {
    const newConfig = await resolveConfig(this.downloadedBinaryPath ?? undefined);
    const binaryChanged =
      this.config?.resolvedBinaryPath !== newConfig.resolvedBinaryPath;
    this.config = newConfig;
    this.onDidChangeConfigEmitter.fire(newConfig);

    if (!this.languageClient || binaryChanged) {
      await this.restartLanguageClient(newConfig);
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

  public async getIndexStatus(workspace: vscode.WorkspaceFolder): Promise<SqryIndexStatus> {
    const cfg = await this.ensureConfig();
    const params: SqryIndexStatusParams = {
      path: workspace.uri.fsPath,
    };

    // LSP returns { status: IndexStatus }, so we need to unwrap
    const result = await this.sendRequest<SqryIndexStatusResult>(
      "sqry/indexStatus",
      params,
      cfg,
    );
    return result.status;
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

  private async restartLanguageClient(config: ResolvedSqryConfig): Promise<void> {
    await this.stopLanguageClient();
    await this.startLanguageClient(config);
  }

  private async startLanguageClient(
    config: ResolvedSqryConfig,
  ): Promise<void> {
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

    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    const clientOptions: LanguageClientOptions = {
      documentSelector: [{ scheme: "file" }],
      outputChannel: this.outputChannel,
      synchronize: {
        configurationSection: "sqry",
      },
      workspaceFolder,
    };

    const languageClient = new LanguageClient(
      "sqryLanguageServer",
      "Sqry Language Server",
      serverOptions,
      clientOptions,
    );

    try {
      // Add timeout to prevent indefinite hanging
      const startPromise = languageClient.start();
      const timeoutPromise = new Promise<never>((_, reject) =>
        setTimeout(
          () => reject(new Error("Language server failed to start within 30 seconds")),
          30000
        )
      );

      await Promise.race([startPromise, timeoutPromise]);
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

  private async getLanguageClient(): Promise<LanguageClient> {
    const cfg = await this.ensureConfig();
    if (
      !this.languageClient ||
      this.currentBinaryPath !== cfg.resolvedBinaryPath
    ) {
      await this.restartLanguageClient(cfg);
    }

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
    const client = await this.getLanguageClient();
    this.outputChannel.appendLine(
      `[sqry] Request ${method} (${this.describeParams(params)})`,
    );
    this.cancelActiveRequest();
    const source = new CancellationTokenSource();
    const timer = setTimeout(() => {
      source.cancel();
    }, cfg.timeoutMs);

    const request: ActiveRequest = {
      cancel: () => source.cancel(),
      timer,
    };
    this.currentRequest = request;

    try {
      const result = await client.sendRequest<T>(method, params, source.token);
      return result;
    } catch (error) {
      if (source.token.isCancellationRequested) {
        throw new Error(
          `sqry request timed out after ${cfg.timeoutMs}ms. Increase \`sqry.timeoutMs\` in settings.`,
        );
      }
      throw error;
    } finally {
      clearTimeout(timer);
      source.dispose();
      if (this.currentRequest === request) {
        this.currentRequest = null;
      }
    }
  }

  private async sendExecuteCommand(
    command: string,
    args: unknown[],
    cfg: ResolvedSqryConfig,
    timeoutMs?: number,
  ): Promise<void> {
    const client = await this.getLanguageClient();
    const timeout = timeoutMs ?? cfg.timeoutMs;
    this.outputChannel.appendLine(
      `[sqry] Command ${command} (${args.map(String).join(" ")})`,
    );
    this.cancelActiveRequest();
    const source = new CancellationTokenSource();
    const timer = setTimeout(() => {
      source.cancel();
    }, timeout);

    const request: ActiveRequest = {
      cancel: () => source.cancel(),
      timer,
    };
    this.currentRequest = request;

    try {
      await client.sendRequest(
        ExecuteCommandRequest.type,
        {
          command,
          arguments: args,
        },
        source.token,
      );
    } catch (error) {
      if (source.token.isCancellationRequested) {
        const settingName = timeoutMs === cfg.indexTimeoutMs
          ? 'sqry.indexTimeoutMs'
          : 'sqry.timeoutMs';
        throw new Error(
          `sqry command timed out after ${timeout}ms. Increase \`${settingName}\` in settings.`,
        );
      }
      throw error;
    } finally {
      clearTimeout(timer);
      source.dispose();
      if (this.currentRequest === request) {
        this.currentRequest = null;
      }
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

  private cancelActiveRequest(): void {
    if (this.currentRequest) {
      this.currentRequest.cancel();
      if (this.currentRequest.timer) {
        clearTimeout(this.currentRequest.timer);
      }
      this.currentRequest = null;
    }
  }

  private async ensureConfig(): Promise<ResolvedSqryConfig> {
    if (this.config) {
      return this.config;
    }
    await this.refreshConfig();
    if (!this.config) {
      throw new Error(
        "sqry configuration is unavailable. Ensure 'sqry' binary is in PATH or set 'sqry.path' in settings.",
      );
    }
    return this.config;
  }
}
