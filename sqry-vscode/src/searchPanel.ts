import * as path from "node:path";
import * as vscode from "vscode";
import {
  SqryIndexStatus,
  SqryListCrossLanguageRelationsResult,
  SqryListFilesByLanguageResult,
  SqryListFilesResult,
  SqryListSymbolsResult,
  SqryListDuplicateGroupsResult,
  SqryDuplicateGroup,
  SqryListCircularDependenciesResult,
  SqryCycle,
  SqryListUnusedSymbolsResult,
  SqrySourceRootIndexState,
  SqryWorkspaceStatus,
} from "./lspProtocol";
import { SqryClient } from "./sqryClient";
import { SqryResult, SqrySymbolResult, SqryTextMatch } from "./types";
import {
  ActiveFilters,
  SortOrder,
  applyFiltersAndSort,
  buildFilterSummary,
  extractKinds,
  extractLanguages,
  makeDefaultFilters,
} from "./filterSort";

// Default limits for list operations
const DEFAULT_SYMBOL_LIMIT = 100;
const DEFAULT_FILE_LIMIT = 100;
const DEFAULT_LANGUAGE_FILE_LIMIT = 50;
const DEFAULT_CROSS_LANGUAGE_LIMIT = 50;

type RenderableIndexStatus = SqryIndexStatus & {
  readonly sourceRootStatus?: SqrySourceRootIndexState;
};

/**
 * Format a duration in seconds to a human-readable string.
 */
function formatAge(seconds: number): string {
  if (seconds < 60) {
    return "just now";
  }
  if (seconds < 3600) {
    const minuteCount = Math.floor(seconds / 60);
    return `${minuteCount} minute${minuteCount === 1 ? "" : "s"} ago`;
  }
  if (seconds < 86400) {
    const hourCount = Math.floor(seconds / 3600);
    return `${hourCount} hour${hourCount === 1 ? "" : "s"} ago`;
  }
  const dayCount = Math.floor(seconds / 86400);
  return `${dayCount} day${dayCount === 1 ? "" : "s"} ago`;
}

/**
 * Individual stat line item - non-expandable
 */
class SqryStatItem extends vscode.TreeItem {
  constructor(
    label: string,
    description: string,
    icon: string,
    tooltip?: string,
  ) {
    super(label, vscode.TreeItemCollapsibleState.None);
    this.description = description;
    this.iconPath = new vscode.ThemeIcon(icon);
    this.tooltip = tooltip ?? `${label}: ${description}`;
    this.contextValue = "sqry.stats.item";
  }
}

/**
 * Expandable stat item that loads children on expand.
 */
class SqryExpandableStatItem extends vscode.TreeItem {
  constructor(
    label: string,
    description: string,
    icon: string,
    public readonly statType: "symbols" | "files" | "languages",
    tooltip?: string,
    public readonly rootPath?: string,
  ) {
    super(label, vscode.TreeItemCollapsibleState.Collapsed);
    this.description = description;
    this.iconPath = new vscode.ThemeIcon(icon);
    this.tooltip = tooltip ?? `Click to expand ${label.toLowerCase()}`;
    this.contextValue = `sqry.stats.expandable.${statType}`;
  }
}

/**
 * Individual file item in the Files list
 */
class SqryFileItem extends vscode.TreeItem {
  constructor(filePath: string) {
    const fileName = path.basename(filePath);
    const dirPath = path.dirname(filePath);
    super(fileName, vscode.TreeItemCollapsibleState.None);
    this.description = dirPath;
    this.iconPath = new vscode.ThemeIcon("file-code");
    this.command = {
      title: "Open File",
      command: "sqry.openResultFile",
      arguments: [filePath],
    };
    this.tooltip = filePath;
    this.contextValue = "sqry.file";
  }
}

/**
 * Individual language item in the Languages list (expandable to show files)
 */
class SqryLanguageItem extends vscode.TreeItem {
  public readonly language: string;

  constructor(language: string, fileCount?: number, public readonly rootPath?: string) {
    const label = fileCount === undefined ? language : `${language} (${fileCount} files)`;
    super(label, vscode.TreeItemCollapsibleState.Collapsed);
    this.language = language;
    this.iconPath = new vscode.ThemeIcon("code");
    this.contextValue = "sqry.language";
    this.tooltip = `Click to expand and see ${language} files`;
  }
}

/**
 * Cross-language relations root item
 */
class SqryCrossLanguageItem extends vscode.TreeItem {
  constructor(count: number, public readonly rootPath?: string) {
    super(`Cross-Language Relations`, vscode.TreeItemCollapsibleState.Collapsed);
    this.description = count > 0 ? `${count}` : "none";
    this.iconPath = new vscode.ThemeIcon("link");
    this.contextValue = "sqry.crossLanguage";
    this.tooltip = "Cross-language imports and calls between different programming languages";
  }
}

/**
 * Individual cross-language relation item
 */
class SqryCrossLanguageRelationItem extends vscode.TreeItem {
  constructor(relation: { relation_type: string; from_symbol: string; from_language: string; from_file: string; to_symbol: string; to_language: string; to_file?: string }) {
    const label = `${relation.from_symbol} → ${relation.to_symbol}`;
    super(label, vscode.TreeItemCollapsibleState.None);
    this.description = `${relation.from_language} → ${relation.to_language}`;
    this.iconPath = new vscode.ThemeIcon(relation.relation_type === "import" ? "arrow-right" : "call-outgoing");
    this.contextValue = "sqry.crossLanguageRelation";
    this.tooltip = `${relation.relation_type}: ${relation.from_symbol} (${relation.from_language}) → ${relation.to_symbol} (${relation.to_language})`;

    // If we have the from_file, make it clickable
    if (relation.from_file) {
      const filePath = resolveFilePath(relation.from_file);
      this.command = {
        title: "Open File",
        command: "sqry.openResultFile",
        arguments: [filePath],
      };
    }
  }
}

// ===== CD Predicate Tree Items =====

/**
 * Root item for duplicate symbol groups
 */
class SqryDuplicatesItem extends vscode.TreeItem {
  constructor(groupCount: number | null, symbolCount: number | null, public readonly rootPath?: string) {
    super("Duplicate Code", vscode.TreeItemCollapsibleState.Collapsed);
    if (groupCount === null) {
      this.description = "expand to check";
    } else {
      this.description = groupCount > 0 ? `${groupCount} groups, ${symbolCount} symbols` : "none";
    }
    this.iconPath = new vscode.ThemeIcon("files");
    this.contextValue = "sqry.duplicates";
    this.tooltip = "Code duplication detected across the codebase";
  }
}

/**
 * Individual duplicate group item (expandable to show symbols)
 */
class SqryDuplicateGroupItem extends vscode.TreeItem {
  constructor(
    public readonly group: SqryDuplicateGroup,
  ) {
    super(group.representative_name, vscode.TreeItemCollapsibleState.Collapsed);
    this.description = `${group.count} duplicates`;
    this.iconPath = new vscode.ThemeIcon("copy");
    this.contextValue = "sqry.duplicateGroup";
    this.tooltip = `${group.count} symbols with identical code`;
  }
}

/**
 * Root item for circular dependencies
 */
class SqryCircularItem extends vscode.TreeItem {
  constructor(
    result: SqryListCircularDependenciesResult | null,
    public readonly rootPath?: string,
  ) {
    super("Circular Dependencies", vscode.TreeItemCollapsibleState.Collapsed);
    if (result === null) {
      this.description = "expand to check";
    } else if (result.total_cycles === 0) {
      this.description = "none";
    } else {
      this.description = result.truncated
        ? `${result.cycles.length}+ cycles`
        : `${result.total_cycles} cycles`;
    }
    this.iconPath = new vscode.ThemeIcon("sync");
    this.contextValue = "sqry.circular";
    this.tooltip = "Circular dependencies detected in call/import graphs";
  }
}

/**
 * Individual cycle item showing the cycle path
 */
class SqryCycleItem extends vscode.TreeItem {
  constructor(cycle: SqryCycle) {
    const label = cycle.members.length <= 3
      ? cycle.members.join(" → ")
      : `${cycle.members[0]} → ... → ${cycle.members.at(-1)}`;
    super(label, vscode.TreeItemCollapsibleState.None);
    this.description = `${cycle.depth} nodes (${cycle.cycle_type})`;
    this.iconPath = new vscode.ThemeIcon("sync");
    this.contextValue = "sqry.cycle";
    this.tooltip = `Cycle: ${cycle.members.join(" → ")}`;
  }
}

/**
 * Root item for unused symbols
 */
class SqryUnusedItem extends vscode.TreeItem {
  constructor(count: number | null, public readonly rootPath?: string) {
    super("Unused Code", vscode.TreeItemCollapsibleState.Collapsed);
    if (count === null) {
      this.description = "expand to check";
    } else if (count === 0) {
      this.description = "none";
    } else {
      this.description = `${count} symbols`;
    }
    this.iconPath = new vscode.ThemeIcon("trash");
    this.contextValue = "sqry.unused";
    this.tooltip = "Symbols that appear to be unused based on reachability analysis";
  }
}

/**
 * Workspace root item for multi-root workspaces.
 * Shows per-root index status and groups stats under each root.
 */
class SqryWorkspaceRootItem extends vscode.TreeItem {
  constructor(
    public readonly rootPath: string,
    label: string,
    status: SqryIndexStatus | undefined,
  ) {
    super(label, vscode.TreeItemCollapsibleState.Collapsed);
    this.description = status
      ? `${status.symbol_count ?? 0} symbols, ${status.file_count ?? 0} files`
      : "not indexed";
    this.iconPath = new vscode.ThemeIcon("folder");
    this.contextValue = "sqry.workspaceRoot";
  }
}

class SqryCategoryItem extends vscode.TreeItem {
  constructor(
    readonly label: string,
    readonly category: "symbols" | "textMatches",
  ) {
    super(label, vscode.TreeItemCollapsibleState.Expanded);
    this.contextValue = `sqry.${category}.category`;
  }
}

// ===== Tree View Grouping Helper Functions =====

function capitalizeFirst(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1).replaceAll("_", " ");
}

function getSymbolKindIcon(kind: string): vscode.ThemeIcon {
  const icons: Record<string, string> = {
    function: "symbol-method",
    method: "symbol-method",
    class: "symbol-class",
    struct: "symbol-structure",
    interface: "symbol-interface",
    enum: "symbol-enum",
    variable: "symbol-variable",
    constant: "symbol-constant",
    type: "symbol-type-parameter",
    module: "symbol-namespace",
    namespace: "symbol-namespace",
    property: "symbol-property",
    parameter: "symbol-parameter",
    import: "package",
    component: "symbol-misc",
    style_rule: "symbol-color",
    style_at_rule: "symbol-color",
    style_variable: "symbol-color",
  };
  return new vscode.ThemeIcon(icons[kind] ?? "symbol-misc");
}

function getLanguageIcon(_lang: string): vscode.ThemeIcon {
  // Use file-type icons where available
  return new vscode.ThemeIcon("file-code");
}

// ===== Tree View Grouping Item Classes =====

/**
 * Intermediate grouping item for symbol kinds
 */
class SqrySymbolKindItem extends vscode.TreeItem {
  constructor(
    public readonly kind: string,
    public readonly count: number,
    public readonly rootPath?: string,
  ) {
    super(capitalizeFirst(kind), vscode.TreeItemCollapsibleState.Collapsed);
    this.description = `${count.toLocaleString()}`;
    this.iconPath = getSymbolKindIcon(kind);
    this.contextValue = "sqry-symbol-kind";
  }
}

/**
 * Intermediate grouping item for file languages
 */
class SqryFileLanguageItem extends vscode.TreeItem {
  constructor(
    public readonly language: string,
    public readonly count: number,
    public readonly rootPath?: string,
  ) {
    super(language, vscode.TreeItemCollapsibleState.Collapsed);
    this.description = `${count.toLocaleString()} files`;
    this.iconPath = getLanguageIcon(language);
    this.contextValue = "sqry-file-language";
  }
}

/**
 * Intermediate grouping item for language pairs
 */
class SqryLanguagePairItem extends vscode.TreeItem {
  constructor(
    public readonly pair: string, // "go→javascript"
    public readonly count: number,
    public readonly rootPath?: string,
  ) {
    super(pair, vscode.TreeItemCollapsibleState.Collapsed);
    this.description = `${count.toLocaleString()}`;
    this.iconPath = new vscode.ThemeIcon("link");
    this.contextValue = "sqry-language-pair";
  }
}

class SqrySymbolItem extends vscode.TreeItem {
  constructor(symbol: SqrySymbolResult) {
    const label = symbol.qualifiedName ?? symbol.name;
    super(label ?? "Unnamed symbol", vscode.TreeItemCollapsibleState.None);

    const filePath = resolveFilePath(symbol.filePath);
    this.description = symbol.kind ?? symbol.language ?? "";
    this.iconPath = new vscode.ThemeIcon("symbol-method");
    this.command = {
      title: "Open Symbol",
      command: "sqry.openResultFile",
      arguments: [filePath, { startLine: Math.max((symbol.startLine ?? 1) - 1, 0) }],
    };
    this.tooltip = `${filePath}`;
    this.contextValue = "sqry.symbol";
  }
}

class SqryTextMatchItem extends vscode.TreeItem {
  constructor(match: SqryTextMatch) {
    const filePath = resolveFilePath(match.path);
    const label = `${path.basename(filePath)}:${match.line}`;
    super(label, vscode.TreeItemCollapsibleState.None);
    this.description = match.lineText?.trim() ?? "";
    this.iconPath = new vscode.ThemeIcon("search");
    this.command = {
      title: "Open Match",
      command: "sqry.openResultFile",
      arguments: [filePath, { startLine: Math.max(match.line - 1, 0) }],
    };
    this.tooltip = match.lineText ?? filePath;
    this.contextValue = "sqry.textMatch";
  }
}

/** Item types supported by pagination. */
type LoadMoreItemType = "symbols" | "files" | "languageFiles" | "crossLanguage";

/**
 * "Load More" tree item for pagination.
 * Supports symbols, files, language-specific files, and cross-language relations.
 */
class SqryLoadMoreItem extends vscode.TreeItem {
  constructor(
    public readonly itemType: LoadMoreItemType,
    public readonly nextOffset: number,
    public readonly total: number,
    currentlyShown: number,
    public readonly language?: string, // Only used for languageFiles
    public readonly rootPath?: string,
  ) {
    const remaining = total - currentlyShown;
    super(`Load More (${remaining} remaining)`, vscode.TreeItemCollapsibleState.None);
    this.iconPath = new vscode.ThemeIcon("ellipsis");
    this.command = {
      title: "Load More",
      command: "sqry.loadMore",
      arguments: [itemType, nextOffset, language, rootPath],
    };
    this.contextValue = "sqry.loadMore";
  }
}

/**
 * Truncation info item showing "showing N of M"
 */
class SqryTruncationItem extends vscode.TreeItem {
  constructor(shown: number, total: number) {
    super(`Showing ${shown} of ${total}`, vscode.TreeItemCollapsibleState.None);
    this.iconPath = new vscode.ThemeIcon("info");
    this.contextValue = "sqry.truncation";
  }
}

class SqryTreeDataProvider
  implements vscode.TreeDataProvider<vscode.TreeItem>
{
  private readonly _onDidChangeTreeData = new vscode.EventEmitter<
    vscode.TreeItem | undefined | null | void
  >();
  public readonly onDidChangeTreeData = this._onDidChangeTreeData.event;
  private readonly outputChannel: vscode.OutputChannel | null;

  private symbols: SqrySymbolResult[] = [];
  private textMatches: SqryTextMatch[] = [];
  private indexStatus: SqryIndexStatus | null = null;
  private readonly indexStatusMap = new Map<string, SqryIndexStatus>();
  private hasSearched = false;
  /**
   * Aggregate workspace status surface (STEP_5). When non-null this
   * supersedes `indexStatusMap` for tree rendering — there is exactly
   * one workspace, classification per source root lives inside the
   * aggregate. The tree shows a per-source-root row for each entry.
   */
  private workspaceStatus: SqryWorkspaceStatus | null = null;
  /**
   * Loading phase mirror — when truthy the tree renders a single
   * skeleton row in place of the normal content. Set via
   * [`setLoadingPhase`].
   */
  private loadingPhase: "loading" | "ready" | "failed" = "ready";
  private failedReason: string | null = null;

  // Filter / sort state
  private unfilteredSymbols: SqrySymbolResult[] = [];
  private activeFilters: ActiveFilters = makeDefaultFilters();
  private activeSortOrder: SortOrder = "default";

  // Per-root caches for lazy-loaded data with pagination metadata.
  // Keys are rootPath (or "" for single-root workspaces).
  private readonly cachedSymbolsResult = new Map<string, SqryListSymbolsResult>();
  private readonly cachedSymbolsByKind = new Map<string, SqryListSymbolsResult>();
  private readonly cachedFilesResult = new Map<string, SqryListFilesResult>();
  private readonly cachedLanguageFiles = new Map<string, SqryListFilesByLanguageResult>();
  private readonly cachedCrossLanguageResult = new Map<string, SqryListCrossLanguageRelationsResult>();
  private readonly cachedRelationsByPair = new Map<string, SqryListCrossLanguageRelationsResult>();
  // CD Predicate caches (per-root)
  private readonly cachedDuplicatesResult = new Map<string, SqryListDuplicateGroupsResult>();
  private readonly cachedCircularResult = new Map<string, SqryListCircularDependenciesResult>();
  private readonly cachedUnusedResult = new Map<string, SqryListUnusedSymbolsResult>();
  // Per-root loading guards
  private readonly loadingSymbols = new Set<string>();
  private readonly loadingSymbolsKind = new Set<string>();
  private readonly loadingFiles = new Set<string>();
  private readonly loadingLanguageFiles = new Set<string>();
  private readonly loadingCrossLanguage = new Set<string>();
  private readonly loadingRelationsPair = new Set<string>();
  private readonly loadingDuplicates = new Set<string>();
  private readonly loadingCircular = new Set<string>();
  private readonly loadingUnused = new Set<string>();

  constructor(
    private readonly client: SqryClient | null,
    outputChannel: vscode.OutputChannel | null = null,
  ) {
    this.outputChannel = outputChannel;
  }

  private log(message: string): void {
    this.outputChannel?.appendLine(`[sqry] ${message}`);
  }

  /** Resolve the workspace folder for a given root path, falling back to first folder. */
  private resolveWorkspaceForRoot(rootPath?: string): vscode.WorkspaceFolder | undefined {
    if (rootPath) {
      return vscode.workspace.workspaceFolders?.find(f => f.uri.fsPath === rootPath);
    }
    return vscode.workspace.workspaceFolders?.[0];
  }

  public setResults(symbols: SqrySymbolResult[], textMatches: SqryTextMatch[]): void {
    this.unfilteredSymbols = [...symbols];
    this.symbols = applyFiltersAndSort(symbols, this.activeFilters, this.activeSortOrder);
    this.textMatches = textMatches;
    this.hasSearched = true;
    this._onDidChangeTreeData.fire();
  }

  /**
   * Return the current (filtered/sorted) symbol results.
   */
  public getSymbols(): SqrySymbolResult[] {
    return this.symbols;
  }

  public setIndexStatus(status: SqryIndexStatus | null): void {
    this.indexStatus = status;
    // Clear caches when status changes (index may have been rebuilt)
    this.cachedSymbolsResult.clear();
    this.cachedSymbolsByKind.clear();
    this.cachedFilesResult.clear();
    this.cachedLanguageFiles.clear();
    this.cachedCrossLanguageResult.clear();
    this.cachedRelationsByPair.clear();
    // Clear CD predicate caches
    this.cachedDuplicatesResult.clear();
    this.cachedCircularResult.clear();
    this.cachedUnusedResult.clear();
    this._onDidChangeTreeData.fire();
  }

  public hydrateIndexStatus(status: SqryIndexStatus | null): void {
    this.indexStatus = status;
    this._onDidChangeTreeData.fire();
  }

  public setIndexStatusForRoot(rootPath: string, status: SqryIndexStatus): void {
    this.indexStatusMap.set(rootPath, status);
    // Also update the single-root status for backward compat (use latest)
    this.indexStatus = status;
    this._onDidChangeTreeData.fire();
  }

  /**
   * Atomically replace the entire index status map.
   * Clears stale entries for removed roots and fires a tree refresh.
   */
  public replaceIndexStatusMap(map: Map<string, SqryIndexStatus>): void {
    this.indexStatusMap.clear();
    for (const [k, v] of map) {
      this.indexStatusMap.set(k, v);
    }
    // Update single-root status for backward compat
    this.indexStatus = map.size === 1
      ? map.values().next().value ?? null
      : null;
    this._onDidChangeTreeData.fire();
  }

  public getIndexStatusMap(): Map<string, SqryIndexStatus> {
    return this.indexStatusMap;
  }

  public clearResults(): void {
    this.unfilteredSymbols = [];
    this.symbols = [];
    this.textMatches = [];
    this.hasSearched = false;
    this.activeFilters = makeDefaultFilters();
    this.activeSortOrder = "default";
    this._onDidChangeTreeData.fire();
  }

  // ---------------------------------------------------------------------------
  // Filter / sort public API
  // ---------------------------------------------------------------------------

  /**
   * Apply new filter criteria to the current result set.
   * Re-applies on the unfiltered original results so filters are composable.
   */
  public setFilters(filters: Partial<ActiveFilters>): void {
    if (filters.languages !== undefined) {
      this.activeFilters.languages = filters.languages;
    }
    if (filters.kinds !== undefined) {
      this.activeFilters.kinds = filters.kinds;
    }
    if (filters.pathGlob !== undefined) {
      this.activeFilters.pathGlob = filters.pathGlob;
    }
    this.symbols = applyFiltersAndSort(this.unfilteredSymbols, this.activeFilters, this.activeSortOrder);
    this._onDidChangeTreeData.fire();
  }

  /**
   * Change the sort order for the current result set.
   */
  public setSortOrder(order: SortOrder): void {
    this.activeSortOrder = order;
    this.symbols = applyFiltersAndSort(this.unfilteredSymbols, this.activeFilters, this.activeSortOrder);
    this._onDidChangeTreeData.fire();
  }

  /**
   * Clear all active filters and reset sort to default.
   */
  public clearFilters(): void {
    this.activeFilters = makeDefaultFilters();
    this.activeSortOrder = "default";
    this.symbols = [...this.unfilteredSymbols];
    this._onDidChangeTreeData.fire();
  }

  /**
   * Get a human-readable summary of the current filter state.
   * Returns empty string when no filters are active.
   */
  public getFilterSummary(): string {
    return buildFilterSummary(this.activeFilters);
  }

  /**
   * Get the distinct languages present in the unfiltered result set.
   */
  public getAvailableLanguages(): string[] {
    return extractLanguages(this.unfilteredSymbols);
  }

  /**
   * Get the distinct symbol kinds present in the unfiltered result set.
   */
  public getAvailableKinds(): string[] {
    return extractKinds(this.unfilteredSymbols);
  }

  // ---------------------------------------------------------------------------
  // Pagination
  // ---------------------------------------------------------------------------

  /**
   * Load more items for pagination.
   * Fetches the next page and merges with existing cached results.
   */
  public async loadMore(
    itemType: "symbols" | "files" | "languageFiles" | "crossLanguage",
    nextOffset: number,
    language?: string,
    rootPath?: string,
  ): Promise<void> {
    const activeClient = this.client;
    if (!activeClient) {
      this.log("Cannot load more: client not available");
      return;
    }

    const workspace = this.resolveWorkspaceForRoot(rootPath);

    if (itemType === "symbols") {
      await this.loadMoreSymbols(workspace, nextOffset, rootPath);
    } else if (itemType === "files") {
      await this.loadMoreFiles(workspace, nextOffset, rootPath);
    } else if (itemType === "languageFiles" && language) {
      await this.loadMoreLanguageFiles(workspace, language, nextOffset, rootPath);
    } else if (itemType === "crossLanguage") {
      await this.loadMoreCrossLanguage(workspace, nextOffset, rootPath);
    }
  }

  private async loadMoreSymbols(workspace: vscode.WorkspaceFolder | undefined, offset: number, rootPath: string = ""): Promise<void> {
    const rootKey = rootPath;
    const activeClient = this.client;
    if (this.loadingSymbols.has(rootKey) || !activeClient) {
      return;
    }

    this.loadingSymbols.add(rootKey);
    this.log(`Loading more symbols from offset ${offset}...`);
    try {
      const newResult = await activeClient.listSymbols(workspace, offset, DEFAULT_SYMBOL_LIMIT);

      // Merge with existing cache
      const existing = this.cachedSymbolsResult.get(rootKey);
      if (existing) {
        const merged = {
          symbols: [...existing.symbols, ...newResult.symbols],
          total: newResult.total,
          offset: newResult.offset,
          limit: newResult.limit,
          has_more: newResult.has_more,
        };
        this.cachedSymbolsResult.set(rootKey, merged);
      } else {
        this.cachedSymbolsResult.set(rootKey, newResult);
      }

      const updated = this.cachedSymbolsResult.get(rootKey)!;
      this.log(`Loaded ${newResult.symbols.length} more symbols (total shown: ${updated.symbols.length} of ${newResult.total})`);
      this._onDidChangeTreeData.fire();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading more symbols: ${message}`);
      void vscode.window.showErrorMessage(`sqry: Failed to load more symbols: ${message}`);
    } finally {
      this.loadingSymbols.delete(rootKey);
    }
  }

  private async loadMoreFiles(workspace: vscode.WorkspaceFolder | undefined, offset: number, rootPath: string = ""): Promise<void> {
    const rootKey = rootPath;
    const activeClient = this.client;
    if (this.loadingFiles.has(rootKey) || !activeClient) {
      return;
    }

    this.loadingFiles.add(rootKey);
    this.log(`Loading more files from offset ${offset}...`);
    try {
      const newResult = await activeClient.listFiles(workspace, offset, DEFAULT_FILE_LIMIT);

      // Merge with existing cache
      const existing = this.cachedFilesResult.get(rootKey);
      if (existing) {
        const merged = {
          files: [...existing.files, ...newResult.files],
          total: newResult.total,
          offset: newResult.offset,
          limit: newResult.limit,
          has_more: newResult.has_more,
        };
        this.cachedFilesResult.set(rootKey, merged);
      } else {
        this.cachedFilesResult.set(rootKey, newResult);
      }

      const updated = this.cachedFilesResult.get(rootKey)!;
      this.log(`Loaded ${newResult.files.length} more files (total shown: ${updated.files.length} of ${newResult.total})`);
      this._onDidChangeTreeData.fire();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading more files: ${message}`);
      void vscode.window.showErrorMessage(`sqry: Failed to load more files: ${message}`);
    } finally {
      this.loadingFiles.delete(rootKey);
    }
  }

  private async loadMoreLanguageFiles(
    workspace: vscode.WorkspaceFolder | undefined,
    language: string,
    offset: number,
    rootPath?: string,
  ): Promise<void> {
    const cacheKey = `${rootPath ?? ""}:${language}`;
    const activeClient = this.client;
    if (this.loadingLanguageFiles.has(cacheKey) || !activeClient) {
      return;
    }

    this.loadingLanguageFiles.add(cacheKey);
    this.log(`Loading more ${language} files from offset ${offset}...`);
    try {
      const newResult = await activeClient.listFilesByLanguage(language, workspace, offset, DEFAULT_LANGUAGE_FILE_LIMIT);

      // Merge with existing cache for this language+root
      const cached = this.cachedLanguageFiles.get(cacheKey);
      if (cached) {
        this.cachedLanguageFiles.set(cacheKey, {
          language: newResult.language,
          files: [...cached.files, ...newResult.files],
          total: newResult.total,
          offset: newResult.offset,
          limit: newResult.limit,
          has_more: newResult.has_more,
        });
      } else {
        this.cachedLanguageFiles.set(cacheKey, newResult);
      }

      const updated = this.cachedLanguageFiles.get(cacheKey)!;
      this.log(`Loaded ${newResult.files.length} more ${language} files (total shown: ${updated.files.length} of ${newResult.total})`);
      this._onDidChangeTreeData.fire();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading more ${language} files: ${message}`);
      void vscode.window.showErrorMessage(`sqry: Failed to load more ${language} files: ${message}`);
    } finally {
      this.loadingLanguageFiles.delete(cacheKey);
    }
  }

  private async loadMoreCrossLanguage(
    workspace: vscode.WorkspaceFolder | undefined,
    offset: number,
    rootPath: string = "",
  ): Promise<void> {
    const rootKey = rootPath;
    const activeClient = this.client;
    if (this.loadingCrossLanguage.has(rootKey) || !activeClient) {
      return;
    }

    this.loadingCrossLanguage.add(rootKey);
    this.log(`Loading more cross-language relations from offset ${offset}...`);
    try {
      const newResult = await activeClient.listCrossLanguageRelations(workspace, offset, DEFAULT_CROSS_LANGUAGE_LIMIT);

      // Merge with existing cache, preserving overflow from first page
      const existing = this.cachedCrossLanguageResult.get(rootKey);
      if (existing) {
        const merged = {
          relations: [...existing.relations, ...newResult.relations],
          total: newResult.total,
          offset: newResult.offset,
          limit: newResult.limit,
          has_more: newResult.has_more,
          // Preserve overflow from first page (static metadata that doesn't change with pagination)
          overflow: existing.overflow ?? newResult.overflow,
        };
        this.cachedCrossLanguageResult.set(rootKey, merged);
      } else {
        this.cachedCrossLanguageResult.set(rootKey, newResult);
      }

      const updated = this.cachedCrossLanguageResult.get(rootKey)!;
      this.log(`Loaded ${newResult.relations.length} more cross-language relations (total shown: ${updated.relations.length} of ${newResult.total})`);
      this._onDidChangeTreeData.fire();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading more cross-language relations: ${message}`);
      void vscode.window.showErrorMessage(`sqry: Failed to load more cross-language relations: ${message}`);
    } finally {
      this.loadingCrossLanguage.delete(rootKey);
    }
  }

  getTreeItem(element: vscode.TreeItem): vscode.TreeItem {
    return element;
  }

  getChildren(element?: vscode.TreeItem): vscode.ProviderResult<vscode.TreeItem[]> {
    if (!element) {
      return this.getRootChildren();
    }
    return this.getElementChildren(element);
  }

  /**
   * Update the loading-phase mirror (STEP_5 acceptance criterion 2).
   * `loading` collapses the tree to a single skeleton row regardless
   * of the underlying status / search state, so the user never sees
   * empty / "no results" flicker during workspace resolution.
   */
  public setLoadingPhase(phase: "loading" | "ready" | "failed", reason?: string): void {
    this.loadingPhase = phase;
    this.failedReason = reason ?? null;
    this._onDidChangeTreeData.fire();
  }

  /**
   * Replace the aggregate workspace status surface (STEP_5
   * acceptance criterion 5). Supersedes the legacy
   * `replaceIndexStatusMap()` for the new code path; both remain
   * present so the search-results panel can show drill-down detail
   * without a second LSP round-trip.
   */
  public setWorkspaceStatus(status: SqryWorkspaceStatus | null): void {
    this.workspaceStatus = status;
    this._onDidChangeTreeData.fire();
  }

  private sourceRootIndexStatus(rootPath: string): RenderableIndexStatus | null {
    const status = this.workspaceStatus?.source_root_statuses.find((s) => s.path === rootPath);
    if (!status) {
      return null;
    }
    const isIndexed = status.status === "ok" || status.status === "building";
    return {
      exists: isIndexed,
      path: status.path,
      sourceRootStatus: status.status,
      symbol_count: status.symbol_count ?? undefined,
      supports_fuzzy: isIndexed,
      supports_relations: isIndexed,
      building: status.status === "building",
    };
  }

  private hasRenderableIndexStatus(status: SqryIndexStatus): boolean {
    const aggregate = (status as { readonly aggregate?: unknown }).aggregate;
    if (!aggregate) {
      return true;
    }
    return status.symbol_count !== undefined
      || status.file_count !== undefined
      || (status.languages !== undefined && status.languages.length > 0);
  }

  private workspaceStatusRootItems(folders: readonly vscode.WorkspaceFolder[]): vscode.TreeItem[] {
    const sourceRoots = this.workspaceStatus?.source_root_statuses ?? [];
    if (sourceRoots.length === 0) {
      return [];
    }
    if (sourceRoots.length === 1) {
      const sourceRoot = sourceRoots[0];
      const status = this.sourceRootIndexStatus(sourceRoot.path);
      return status ? this.buildStatsItems(status, sourceRoot.path) : [];
    }
    return sourceRoots.map((sourceRoot) => {
      const folder = folders.find((f) => f.uri.fsPath === sourceRoot.path);
      return new SqryWorkspaceRootItem(
        sourceRoot.path,
        folder?.name ?? path.basename(sourceRoot.path),
        this.sourceRootIndexStatus(sourceRoot.path) ?? undefined,
      );
    });
  }

  /** Get children for root level (no parent element). */
  private getRootChildren(): vscode.TreeItem[] {
    // STEP_5 contract — single skeleton row during loading. Must
    // be checked BEFORE search results so a stale result from a
    // previous workspace doesn't leak through during reload.
    if (this.loadingPhase === "loading") {
      const skeleton = new vscode.TreeItem(
        "sqry: resolving workspace…",
        vscode.TreeItemCollapsibleState.None,
      );
      skeleton.iconPath = new vscode.ThemeIcon("loading~spin");
      skeleton.contextValue = "sqry.skeleton";
      return [skeleton];
    }
    if (this.loadingPhase === "failed") {
      const item = new vscode.TreeItem(
        this.failedReason ? `sqry: unavailable — ${this.failedReason}` : "sqry: unavailable",
        vscode.TreeItemCollapsibleState.None,
      );
      item.iconPath = new vscode.ThemeIcon("error");
      item.command = {
        command: "sqry.showOutput",
        title: "View Logs",
      };
      item.contextValue = "sqry.unavailable";
      return [item];
    }

    // Show search results if we have them
    if (this.symbols.length || this.textMatches.length) {
      const categories: vscode.TreeItem[] = [];
      if (this.symbols.length) {
        categories.push(new SqryCategoryItem("Semantic Symbols", "symbols"));
      }
      if (this.textMatches.length) {
        categories.push(new SqryCategoryItem("Text Matches", "textMatches"));
      }
      return categories;
    }

    // Show "no results" if user searched but got nothing
    if (this.hasSearched) {
      return [new vscode.TreeItem("No results found", vscode.TreeItemCollapsibleState.None)];
    }

    // Multi-root: show per-root grouping when >1 root has status.
    // routing-gate-allow:UI presentation only; indexStatusMap is hydrated from classifier-aware workspace status.
    const folders = vscode.workspace.workspaceFolders ?? [];
    if (folders.length > 1 && this.indexStatusMap.size > 0) {
      return folders.map(f => new SqryWorkspaceRootItem(
        f.uri.fsPath,
        f.name,
        this.indexStatusMap.get(f.uri.fsPath),
      ));
    }

    // Show index stats directly at root level (flat structure per UX guidelines)
    if (this.indexStatus && this.hasRenderableIndexStatus(this.indexStatus)) {
      return this.buildStatsItems(this.indexStatus);
    }

    const workspaceStatusItems = this.workspaceStatusRootItems(folders);
    if (workspaceStatusItems.length > 0) {
      return workspaceStatusItems;
    }

    // No stats available yet - welcome view will show via viewsWelcome
    return [];
  }

  /** Get children for a specific element based on its type. */
  private getElementChildren(element: vscode.TreeItem): vscode.ProviderResult<vscode.TreeItem[]> {
    if (element instanceof SqryWorkspaceRootItem) {
      const status = this.indexStatusMap.get(element.rootPath)
        ?? this.sourceRootIndexStatus(element.rootPath);
      if (status) {
        return this.buildStatsItems(status, element.rootPath);
      }
      return [new SqryStatItem("Status", "not indexed", "error")];
    }
    if (element instanceof SqryCategoryItem) {
      return this.getCategoryChildren(element);
    }
    if (element instanceof SqryExpandableStatItem) {
      return this.getExpandableChildren(element);
    }
    if (element instanceof SqryLanguageItem) {
      return this.getLanguageFileChildren(element.language, element.rootPath);
    }
    if (element instanceof SqryCrossLanguageItem) {
      return this.getCrossLanguageChildren(element.rootPath);
    }
    if (element instanceof SqrySymbolKindItem) {
      return this.getSymbolsByKind(element.kind, element.rootPath);
    }
    if (element instanceof SqryFileLanguageItem) {
      return this.getFilesByLanguage(element.language, element.rootPath);
    }
    if (element instanceof SqryLanguagePairItem) {
      const [source, target] = element.pair.split("→");
      return this.getCrossLanguageRelationsByPair(source, target, element.rootPath);
    }
    if (element instanceof SqryDuplicatesItem) {
      return this.getDuplicateGroupsChildren(element.rootPath);
    }
    if (element instanceof SqryDuplicateGroupItem) {
      return this.getDuplicateGroupSymbols(element.group);
    }
    if (element instanceof SqryCircularItem) {
      return this.getCircularDependenciesChildren(element.rootPath);
    }
    if (element instanceof SqryUnusedItem) {
      return this.getUnusedSymbolsChildren(element.rootPath);
    }
    return [];
  }

  /** Get children for category items (search results). */
  private getCategoryChildren(element: SqryCategoryItem): vscode.TreeItem[] {
    if (element.category === "symbols") {
      return this.symbols.map((symbol) => new SqrySymbolItem(symbol));
    }
    if (element.category === "textMatches") {
      return this.textMatches.map((match) => new SqryTextMatchItem(match));
    }
    return [];
  }

  private async getExpandableChildren(element: SqryExpandableStatItem): Promise<vscode.TreeItem[]> {
    // Resolve the index status for this root (per-root map), falling back to global
    const rootStatus = element.rootPath
      ? this.indexStatusMap.get(element.rootPath)
      : this.indexStatus;

    if (element.statType === "symbols") {
      // Check if grouped counts are available - show intermediate grouping layer
      const counts = rootStatus?.symbol_counts_by_kind;
      if (counts && Object.keys(counts).length > 0) {
        return Object.entries(counts)
          .sort((a, b) => b[1] - a[1]) // Sort by count descending
          .map(([kind, count]) => new SqrySymbolKindItem(kind, count, element.rootPath));
      }
      // Fallback to flat symbol list
      return this.getSymbolChildren(element.rootPath);
    }
    if (element.statType === "files") {
      // Check if file counts by language are available - show language grouping
      const counts = rootStatus?.file_counts_by_language;
      if (counts && Object.keys(counts).length > 0) {
        return Object.entries(counts)
          .sort((a, b) => b[1] - a[1]) // Sort by count descending
          .map(([lang, count]) => new SqryFileLanguageItem(lang, count, element.rootPath));
      }
      // Fallback to flat file list
      return this.getFileChildren(element.rootPath);
    }
    if (element.statType === "languages") {
      return this.getLanguageChildren(element.rootPath);
    }
    return [];
  }

  private async getSymbolChildren(rootPath?: string): Promise<vscode.TreeItem[]> {
    const rootKey = rootPath ?? "";

    // Return cached if available
    const cached = this.cachedSymbolsResult.get(rootKey);
    if (cached) {
      return this.buildSymbolItems(cached, rootPath);
    }

    // Avoid duplicate requests
    const activeClient = this.client;
    if (this.loadingSymbols.has(rootKey) || !activeClient) {
      const loadingItem = new vscode.TreeItem("Loading symbols...", vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingSymbols.add(rootKey);
    this.log("Fetching symbols...");
    try {
      const workspace = this.resolveWorkspaceForRoot(rootPath);
      const result = await activeClient.listSymbols(workspace, 0, DEFAULT_SYMBOL_LIMIT);
      this.cachedSymbolsResult.set(rootKey, result);
      this.log(`Loaded ${result.symbols.length} of ${result.total} symbols`);
      return this.buildSymbolItems(result, rootPath);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading symbols: ${message}`);
      const symbolsLoadErrorItem = new vscode.TreeItem(`Error: ${message}`, vscode.TreeItemCollapsibleState.None);
      symbolsLoadErrorItem.iconPath = new vscode.ThemeIcon("error");
      return [symbolsLoadErrorItem];
    } finally {
      this.loadingSymbols.delete(rootKey);
    }
  }

  private buildSymbolItems(result: SqryListSymbolsResult, rootPath?: string): vscode.TreeItem[] {
    const items: vscode.TreeItem[] = [];

    // Convert symbols to tree items
    for (const item of result.symbols) {
      const uri = vscode.Uri.parse(item.location.uri);
      const filePath = uri.scheme === "file" ? uri.fsPath : uri.toString();
      const startLine = item.location.range.start.line + 1;
      const symbolResult: SqrySymbolResult = {
        name: item.name,
        kind: item.kind ?? "",
        qualifiedName: item.qualified_name ?? "",
        language: item.language ?? "",
        filePath,
        startLine,
      };
      items.push(new SqrySymbolItem(symbolResult));
    }

    // Add truncation info if there are more items
    if (result.has_more) {
      // After merging pages, symbols.length is total items loaded so far
      const shown = result.symbols.length;
      // Next offset should be where we left off (total loaded)
      const nextOffset = shown;
      items.push(
        new SqryTruncationItem(shown, result.total),
        new SqryLoadMoreItem("symbols", nextOffset, result.total, shown, undefined, rootPath),
      );
    }

    return items;
  }

  private async getFileChildren(rootPath?: string): Promise<vscode.TreeItem[]> {
    const rootKey = rootPath ?? "";

    // Return cached if available
    const cached = this.cachedFilesResult.get(rootKey);
    if (cached) {
      return this.buildFileItems(cached, rootPath);
    }

    // Avoid duplicate requests
    const activeClient = this.client;
    if (this.loadingFiles.has(rootKey) || !activeClient) {
      const loadingItem = new vscode.TreeItem("Loading files...", vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingFiles.add(rootKey);
    this.log("Fetching files...");
    try {
      const workspace = this.resolveWorkspaceForRoot(rootPath);
      const result = await activeClient.listFiles(workspace, 0, DEFAULT_FILE_LIMIT);
      this.cachedFilesResult.set(rootKey, result);
      this.log(`Loaded ${result.files.length} of ${result.total} files`);
      return this.buildFileItems(result, rootPath);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading files: ${message}`);
      const filesLoadErrorItem = new vscode.TreeItem(`Error: ${message}`, vscode.TreeItemCollapsibleState.None);
      filesLoadErrorItem.iconPath = new vscode.ThemeIcon("error");
      return [filesLoadErrorItem];
    } finally {
      this.loadingFiles.delete(rootKey);
    }
  }

  private buildFileItems(result: SqryListFilesResult, rootPath?: string): vscode.TreeItem[] {
    const items: vscode.TreeItem[] = [];

    // Convert files to tree items
    for (const filePath of result.files) {
      items.push(new SqryFileItem(filePath));
    }

    // Add truncation info if there are more items
    if (result.has_more) {
      // After merging pages, files.length is total items loaded so far
      const shown = result.files.length;
      // Next offset should be where we left off (total loaded)
      const nextOffset = shown;
      items.push(
        new SqryTruncationItem(shown, result.total),
        new SqryLoadMoreItem("files", nextOffset, result.total, shown, undefined, rootPath),
      );
    }

    return items;
  }

  private getLanguageChildren(rootPath?: string): vscode.TreeItem[] {
    // Resolve the index status for this root
    const rootStatus = rootPath
      ? this.indexStatusMap.get(rootPath)
      : this.indexStatus;
    if (rootStatus?.languages) {
      return rootStatus.languages.map((lang) => new SqryLanguageItem(lang, undefined, rootPath));
    }
    return [];
  }

  private async getLanguageFileChildren(language: string, rootPath?: string): Promise<vscode.TreeItem[]> {
    const rootKey = rootPath ?? "";
    const cacheKey = `${rootKey}:${language}`;

    // Return cached if available
    const cached = this.cachedLanguageFiles.get(cacheKey);
    if (cached) {
      return this.buildLanguageFileItems(cached, rootPath);
    }

    // Avoid duplicate requests
    const activeClient = this.client;
    if (this.loadingLanguageFiles.has(cacheKey) || !activeClient) {
      const loadingItem = new vscode.TreeItem(`Loading ${language} files...`, vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingLanguageFiles.add(cacheKey);
    this.log(`Fetching files for language: ${language}`);
    try {
      const workspace = this.resolveWorkspaceForRoot(rootPath);
      const result = await activeClient.listFilesByLanguage(language, workspace, 0, DEFAULT_LANGUAGE_FILE_LIMIT);
      this.cachedLanguageFiles.set(cacheKey, result);
      this.log(`Loaded ${result.files.length} of ${result.total} ${language} files`);
      return this.buildLanguageFileItems(result, rootPath);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading ${language} files: ${message}`);
      const languageFilesLoadErrorItem = new vscode.TreeItem(`Error: ${message}`, vscode.TreeItemCollapsibleState.None);
      languageFilesLoadErrorItem.iconPath = new vscode.ThemeIcon("error");
      return [languageFilesLoadErrorItem];
    } finally {
      this.loadingLanguageFiles.delete(cacheKey);
    }
  }

  private buildLanguageFileItems(result: SqryListFilesByLanguageResult, rootPath?: string): vscode.TreeItem[] {
    const items: vscode.TreeItem[] = [];

    // Convert files to tree items
    for (const filePath of result.files) {
      items.push(new SqryFileItem(filePath));
    }

    // Add pagination controls if there are more items
    if (result.has_more) {
      const shown = result.files.length;
      const nextOffset = shown;
      items.push(
        new SqryTruncationItem(shown, result.total),
        new SqryLoadMoreItem("languageFiles", nextOffset, result.total, shown, result.language, rootPath),
      );
    }

    return items;
  }

  private async getCrossLanguageChildren(rootPath?: string): Promise<vscode.TreeItem[]> {
    const rootKey = rootPath ?? "";

    // Check if language pair counts are available - show intermediate grouping layer
    const rootStatus = rootPath
      ? this.indexStatusMap.get(rootPath)
      : this.indexStatus;
    const counts = rootStatus?.relation_counts_by_pair;
    if (counts && Object.keys(counts).length > 0) {
      return Object.entries(counts)
        .sort((a, b) => b[1] - a[1]) // Sort by count descending
        .map(([pair, count]) => new SqryLanguagePairItem(pair, count, rootPath));
    }

    // Fallback to flat list - Return cached if available
    const cached = this.cachedCrossLanguageResult.get(rootKey);
    if (cached) {
      return this.buildCrossLanguageItems(cached, rootPath);
    }

    // Avoid duplicate requests
    const activeClient = this.client;
    if (this.loadingCrossLanguage.has(rootKey) || !activeClient) {
      const loadingItem = new vscode.TreeItem("Loading cross-language relations...", vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingCrossLanguage.add(rootKey);
    this.log("Fetching cross-language relations...");
    try {
      const workspace = this.resolveWorkspaceForRoot(rootPath);
      const result = await activeClient.listCrossLanguageRelations(workspace, 0, DEFAULT_CROSS_LANGUAGE_LIMIT);
      this.cachedCrossLanguageResult.set(rootKey, result);
      this.log(`Loaded ${result.relations.length} of ${result.total} cross-language relations`);
      return this.buildCrossLanguageItems(result, rootPath);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading cross-language relations: ${message}`);
      const crossLanguageLoadErrorItem = new vscode.TreeItem(`Error: ${message}`, vscode.TreeItemCollapsibleState.None);
      crossLanguageLoadErrorItem.iconPath = new vscode.ThemeIcon("error");
      return [crossLanguageLoadErrorItem];
    } finally {
      this.loadingCrossLanguage.delete(rootKey);
    }
  }

  private buildCrossLanguageItems(result: SqryListCrossLanguageRelationsResult, rootPath?: string): vscode.TreeItem[] {
    const items: vscode.TreeItem[] = [];

    if (result.relations.length === 0) {
      const noRelationsItem = new vscode.TreeItem("No cross-language relations found", vscode.TreeItemCollapsibleState.None);
      noRelationsItem.iconPath = new vscode.ThemeIcon("info");
      return [noRelationsItem];
    }

    // Convert relations to tree items
    for (const relation of result.relations) {
      items.push(new SqryCrossLanguageRelationItem(relation));
    }

    // Add pagination controls if there are more items
    if (result.has_more) {
      const shown = result.relations.length;
      const nextOffset = shown;
      items.push(
        new SqryTruncationItem(shown, result.total),
        new SqryLoadMoreItem("crossLanguage", nextOffset, result.total, shown, undefined, rootPath),
      );
    }

    return items;
  }

  /**
   * Get symbols filtered by kind using LSP-level filtering.
   * Uses per-kind caching to avoid redundant requests.
   */
  private async getSymbolsByKind(kind: string, rootPath?: string): Promise<vscode.TreeItem[]> {
    const rootKey = rootPath ?? "";
    const cacheKey = `${rootKey}:${kind.toLowerCase()}`;

    // Return cached if available
    const cached = this.cachedSymbolsByKind.get(cacheKey);
    if (cached) {
      return this.buildSymbolItems(cached, rootPath);
    }

    // Avoid duplicate requests for the same kind
    const activeClient = this.client;
    if (this.loadingSymbolsKind.has(cacheKey) || !activeClient) {
      const loadingItem = new vscode.TreeItem(`Loading ${kind} symbols...`, vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingSymbolsKind.add(cacheKey);
    this.log(`Fetching ${kind} symbols via LSP...`);
    try {
      const workspace = this.resolveWorkspaceForRoot(rootPath);
      // Use LSP-level filtering by passing kind parameter
      const result = await activeClient.listSymbols(workspace, 0, DEFAULT_SYMBOL_LIMIT, kind);
      this.cachedSymbolsByKind.set(cacheKey, result);
      this.log(`Loaded ${result.symbols.length} of ${result.total} ${kind} symbols`);
      return this.buildSymbolItems(result, rootPath);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading ${kind} symbols: ${message}`);
      const symbolsByKindLoadErrorItem = new vscode.TreeItem(`Error: ${message}`, vscode.TreeItemCollapsibleState.None);
      symbolsByKindLoadErrorItem.iconPath = new vscode.ThemeIcon("error");
      return [symbolsByKindLoadErrorItem];
    } finally {
      this.loadingSymbolsKind.delete(cacheKey);
    }
  }

  /**
   * Get files filtered by language.
   * Delegates to existing getLanguageFileChildren which uses listFilesByLanguage LSP.
   */
  private async getFilesByLanguage(language: string, rootPath?: string): Promise<vscode.TreeItem[]> {
    return this.getLanguageFileChildren(language, rootPath);
  }

  /**
   * Get cross-language relations filtered by language pair using LSP-level filtering.
   * Uses per-pair caching to avoid redundant requests.
   */
  private async getCrossLanguageRelationsByPair(sourceLang: string, targetLang: string, rootPath?: string): Promise<vscode.TreeItem[]> {
    const rootKey = rootPath ?? "";
    const cacheKey = `${rootKey}:${sourceLang.toLowerCase()}→${targetLang.toLowerCase()}`;

    // Return cached if available
    const cached = this.cachedRelationsByPair.get(cacheKey);
    if (cached) {
      return this.buildCrossLanguageItems(cached, rootPath);
    }

    // Avoid duplicate requests for the same pair
    const activeClient = this.client;
    if (this.loadingRelationsPair.has(cacheKey) || !activeClient) {
      const loadingItem = new vscode.TreeItem(`Loading ${sourceLang}→${targetLang} relations...`, vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingRelationsPair.add(cacheKey);
    this.log(`Fetching ${sourceLang}→${targetLang} relations via LSP...`);
    try {
      const workspace = this.resolveWorkspaceForRoot(rootPath);
      // Use LSP-level filtering by passing source and target language parameters
      const result = await activeClient.listCrossLanguageRelations(
        workspace,
        0,
        DEFAULT_CROSS_LANGUAGE_LIMIT,
        undefined, // sortOrder
        sourceLang,
        targetLang
      );
      this.cachedRelationsByPair.set(cacheKey, result);
      this.log(`Loaded ${result.relations.length} of ${result.total} ${sourceLang}→${targetLang} relations`);
      return this.buildCrossLanguageItems(result, rootPath);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading ${sourceLang}→${targetLang} relations: ${message}`);
      const relationsByPairLoadErrorItem = new vscode.TreeItem(`Error: ${message}`, vscode.TreeItemCollapsibleState.None);
      relationsByPairLoadErrorItem.iconPath = new vscode.ThemeIcon("error");
      return [relationsByPairLoadErrorItem];
    } finally {
      this.loadingRelationsPair.delete(cacheKey);
    }
  }

  // ===== CD Predicate Children Methods =====

  /**
   * Get children for the Duplicate Code category.
   * Returns duplicate groups that can be expanded to show individual symbols.
   */
  private async getDuplicateGroupsChildren(rootPath?: string): Promise<vscode.TreeItem[]> {
    const rootKey = rootPath ?? "";

    // Return cached if available
    const cached = this.cachedDuplicatesResult.get(rootKey);
    if (cached) {
      return cached.groups.map(group => new SqryDuplicateGroupItem(group));
    }

    // Avoid duplicate requests
    const activeClient = this.client;
    if (this.loadingDuplicates.has(rootKey) || !activeClient) {
      const loadingItem = new vscode.TreeItem("Loading duplicate groups...", vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingDuplicates.add(rootKey);
    this.log("Fetching duplicate groups via LSP...");
    try {
      const workspace = this.resolveWorkspaceForRoot(rootPath);
      const result = await activeClient.listDuplicateGroups(workspace, "body", 100);
      this.cachedDuplicatesResult.set(rootKey, result);
      this.log(`Loaded ${result.groups.length} duplicate groups with ${result.total_symbols} total symbols`);
      this._onDidChangeTreeData.fire(); // Refresh to show actual data
      return result.groups.map(group => new SqryDuplicateGroupItem(group));
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading duplicate groups: ${message}`);
      const duplicatesLoadErrorItem = new vscode.TreeItem(`Error: ${message}`, vscode.TreeItemCollapsibleState.None);
      duplicatesLoadErrorItem.iconPath = new vscode.ThemeIcon("error");
      return [duplicatesLoadErrorItem];
    } finally {
      this.loadingDuplicates.delete(rootKey);
    }
  }

  /**
   * Get symbols within a duplicate group.
   * These are clickable to navigate to the source location.
   */
  private getDuplicateGroupSymbols(group: SqryDuplicateGroup): vscode.TreeItem[] {
    return group.symbols.map(symbol => {
      const item = new vscode.TreeItem(symbol.name, vscode.TreeItemCollapsibleState.None);
      item.description = symbol.kind ?? symbol.language;
      item.iconPath = getSymbolKindIcon(symbol.kind ?? "symbol");

      const filePath = resolveFilePath(symbol.location.uri.replace("file://", ""));
      item.command = {
        title: "Open Symbol",
        command: "sqry.openResultFile",
        arguments: [
          filePath,
          {
            startLine: symbol.location.range.start.line,
            startCharacter: symbol.location.range.start.character,
            endLine: symbol.location.range.end.line,
            endCharacter: symbol.location.range.end.character,
          },
        ],
      };
      item.tooltip = `${filePath}:${symbol.location.range.start.line + 1}`;
      item.contextValue = "sqry.duplicateSymbol";
      return item;
    });
  }

  /**
   * Get children for the Circular Dependencies category.
   * Returns cycles detected in call/import graphs.
   */
  private async getCircularDependenciesChildren(rootPath?: string): Promise<vscode.TreeItem[]> {
    const rootKey = rootPath ?? "";

    // Return cached if available
    const cached = this.cachedCircularResult.get(rootKey);
    if (cached) {
      return cached.cycles.map(cycle => new SqryCycleItem(cycle));
    }

    // Avoid duplicate requests
    const activeClient = this.client;
    if (this.loadingCircular.has(rootKey) || !activeClient) {
      const loadingItem = new vscode.TreeItem("Loading circular dependencies...", vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingCircular.add(rootKey);
    this.log("Fetching circular dependencies via LSP...");
    try {
      const workspace = this.resolveWorkspaceForRoot(rootPath);
      const result = await activeClient.listCircularDependencies(workspace, "calls", 100, false);
      this.cachedCircularResult.set(rootKey, result);
      this.log(`Loaded ${result.cycles.length} circular dependencies`);
      this._onDidChangeTreeData.fire(); // Refresh to show actual data
      if (result.cycles.length === 0) {
        const noneItem = new vscode.TreeItem("No circular dependencies found", vscode.TreeItemCollapsibleState.None);
        noneItem.iconPath = new vscode.ThemeIcon("check");
        return [noneItem];
      }
      return result.cycles.map(cycle => new SqryCycleItem(cycle));
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading circular dependencies: ${message}`);
      const circularDepsLoadErrorItem = new vscode.TreeItem(`Error: ${message}`, vscode.TreeItemCollapsibleState.None);
      circularDepsLoadErrorItem.iconPath = new vscode.ThemeIcon("error");
      return [circularDepsLoadErrorItem];
    } finally {
      this.loadingCircular.delete(rootKey);
    }
  }

  /**
   * Get children for the Unused Code category.
   * Returns symbols that appear to be unused.
   */
  private async getUnusedSymbolsChildren(rootPath?: string): Promise<vscode.TreeItem[]> {
    const rootKey = rootPath ?? "";

    // Return cached if available
    const cached = this.cachedUnusedResult.get(rootKey);
    if (cached) {
      return this.buildUnusedSymbolItems(cached);
    }

    // Avoid duplicate requests
    const activeClient = this.client;
    if (this.loadingUnused.has(rootKey) || !activeClient) {
      const loadingItem = new vscode.TreeItem("Loading unused symbols...", vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingUnused.add(rootKey);
    this.log("Fetching unused symbols via LSP...");
    try {
      const workspace = this.resolveWorkspaceForRoot(rootPath);
      const result = await activeClient.listUnusedSymbols(workspace, "all", 100);
      this.cachedUnusedResult.set(rootKey, result);
      this.log(`Loaded ${result.symbols.length} unused symbols`);
      this._onDidChangeTreeData.fire(); // Refresh to show actual data
      return this.buildUnusedSymbolItems(result);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading unused symbols: ${message}`);
      const unusedSymbolsLoadErrorItem = new vscode.TreeItem(`Error: ${message}`, vscode.TreeItemCollapsibleState.None);
      unusedSymbolsLoadErrorItem.iconPath = new vscode.ThemeIcon("error");
      return [unusedSymbolsLoadErrorItem];
    } finally {
      this.loadingUnused.delete(rootKey);
    }
  }

  private buildUnusedSymbolItems(result: SqryListUnusedSymbolsResult): vscode.TreeItem[] {
    if (result.symbols.length === 0) {
      const noneItem = new vscode.TreeItem("No unused symbols found", vscode.TreeItemCollapsibleState.None);
      noneItem.iconPath = new vscode.ThemeIcon("check");
      return [noneItem];
    }

    const items: vscode.TreeItem[] = result.symbols.map(symbol => {
      const item = new vscode.TreeItem(symbol.name, vscode.TreeItemCollapsibleState.None);
      item.description = symbol.kind ?? symbol.language;
      item.iconPath = new vscode.ThemeIcon("trash");

      const filePath = resolveFilePath(symbol.location.uri.replace("file://", ""));
      item.command = {
        title: "Open Symbol",
        command: "sqry.openResultFile",
        arguments: [
          filePath,
          {
            startLine: symbol.location.range.start.line,
            startCharacter: symbol.location.range.start.character,
            endLine: symbol.location.range.end.line,
            endCharacter: symbol.location.range.end.character,
          },
        ],
      };
      item.tooltip = `${filePath}:${symbol.location.range.start.line + 1} (${result.scope} scope)`;
      item.contextValue = "sqry.unusedSymbol";
      return item;
    });

    // Add truncation info if results were limited
    if (result.truncated) {
      const truncationItem = new vscode.TreeItem(
        `Showing ${result.symbols.length} of ${result.total} (results truncated)`,
        vscode.TreeItemCollapsibleState.None,
      );
      truncationItem.iconPath = new vscode.ThemeIcon("info");
      truncationItem.contextValue = "sqry.truncation";
      items.push(truncationItem);
    }

    return items;
  }

  private buildStatsItems(status: SqryIndexStatus, rootPath?: string): vscode.TreeItem[] {
    const items: vscode.TreeItem[] = [];

    this.addCoreStats(items, status, rootPath);
    this.addCDPredicates(items, status, rootPath);
    this.addStatusIndicators(items, status);

    return items;
  }

  /** Add core index statistics (symbols, files, languages, cross-language). */
  private addCoreStats(items: vscode.TreeItem[], status: SqryIndexStatus, rootPath?: string): void {
    if (status.symbol_count !== undefined) {
      items.push(new SqryExpandableStatItem(
        "Symbols",
        status.symbol_count.toLocaleString(),
        "symbol-class",
        "symbols",
        `Click to browse ${status.symbol_count.toLocaleString()} indexed symbols`,
        rootPath,
      ));
    }

    if (status.file_count !== undefined) {
      items.push(new SqryExpandableStatItem(
        "Files",
        status.file_count.toLocaleString(),
        "files",
        "files",
        `Click to browse ${status.file_count.toLocaleString()} indexed files`,
        rootPath,
      ));
    }

    if (status.languages && status.languages.length > 0) {
      const langList = status.languages.slice(0, 5).join(", ");
      const suffix = status.languages.length > 5 ? ` +${status.languages.length - 5} more` : "";
      items.push(new SqryExpandableStatItem(
        "Languages",
        langList + suffix,
        "code",
        "languages",
        `Click to see all ${status.languages.length} languages`,
        rootPath,
      ));
    }

    if (status.supports_relations && status.languages && status.languages.length > 1) {
      const crossLangCount = status.cross_language_relation_count ?? 0;
      items.push(new SqryCrossLanguageItem(crossLangCount, rootPath));
    }
  }

  /** Add CD predicates (duplicates, circular dependencies, unused code). */
  private addCDPredicates(items: vscode.TreeItem[], status: SqryIndexStatus, rootPath?: string): void {
    if (!status.supports_relations) {
      return;
    }

    const rootKey = rootPath ?? "";
    const duplicatesCount = this.cachedDuplicatesResult.get(rootKey)?.total_groups ?? null;
    const symbolsCount = this.cachedDuplicatesResult.get(rootKey)?.total_symbols ?? null;
    items.push(new SqryDuplicatesItem(duplicatesCount, symbolsCount, rootPath));

    const circularResult = this.cachedCircularResult.get(rootKey) ?? null;
    items.push(new SqryCircularItem(circularResult, rootPath));

    const unusedCount = this.cachedUnusedResult.get(rootKey)?.total ?? null;
    items.push(new SqryUnusedItem(unusedCount, rootPath));
  }

  /** Add status indicators (index age, stale warning, building). */
  private addStatusIndicators(items: vscode.TreeItem[], status: SqryIndexStatus): void {
    const sourceRootStatus = (status as RenderableIndexStatus).sourceRootStatus;

    if (status.age_seconds !== undefined) {
      const indexedItem = new SqryStatItem(
        "Indexed",
        formatAge(status.age_seconds),
        "history",
        status.created_at ? `Indexed: ${status.created_at}\nRight-click to rebuild index` : "Right-click to rebuild index",
      );
      indexedItem.contextValue = "sqry.stats.indexed";
      items.push(indexedItem);
    }

    if (status.stale) {
      const staleItem = new SqryStatItem(
        "Status",
        "Index may be stale",
        "warning",
        "Right-click to rebuild index",
      );
      staleItem.contextValue = "sqry.stats.indexed";
      items.push(staleItem);
    }

    if (status.building) {
      const buildAge = status.build_age_seconds ? ` (${Math.floor(status.build_age_seconds / 60)} min)` : "";
      items.push(new SqryStatItem(
        "Status",
        `Building${buildAge}`,
        "sync~spin",
        "Index build is in progress",
      ));
    }

    if (sourceRootStatus === "error") {
      items.push(new SqryStatItem(
        "Status",
        "index error",
        "error",
        "The source root index is unavailable. Rebuild the index or check the sqry output.",
      ));
    } else if (sourceRootStatus === "missing" || (!status.exists && !status.building)) {
      const missingItem = new SqryStatItem(
        "Status",
        "not indexed",
        "warning",
        "Run sqry: Index Workspace to build the index",
      );
      missingItem.contextValue = "sqry.stats.indexed";
      items.push(missingItem);
    }
  }
}

export class SearchPanel implements vscode.Disposable {
  private readonly treeDataProvider: SqryTreeDataProvider;
  private readonly treeView: vscode.TreeView<vscode.TreeItem>;

  constructor(
    context: vscode.ExtensionContext,
    client: SqryClient | null = null,
    outputChannel: vscode.OutputChannel | null = null,
  ) {
    this.treeDataProvider = new SqryTreeDataProvider(client, outputChannel);
    this.treeView = vscode.window.createTreeView("sqry.searchResults", {
      treeDataProvider: this.treeDataProvider,
      showCollapseAll: true,  // Enable collapse all for browsing
    });
    context.subscriptions.push(this.treeView);
  }

  public update(result: SqryResult): void {
    const symbolCount = result.symbols.length;
    const textCount = result.textMatches.length;
    if (!symbolCount && !textCount) {
      void vscode.window.showInformationMessage(
        "sqry: No results found for the query.",
      );
    }
    this.treeDataProvider.setResults(result.symbols, result.textMatches);
  }

  /**
   * Update the panel to show index statistics.
   * Called when no search is active to show useful info about the index.
   */
  public setIndexStatus(status: SqryIndexStatus | null): void {
    this.treeDataProvider.setIndexStatus(status);
  }

  public hydrateIndexStatus(status: SqryIndexStatus | null): void {
    this.treeDataProvider.hydrateIndexStatus(status);
  }

  /**
   * Update index status for a specific workspace root.
   * Used in multi-root workspaces to track per-root status.
   */
  public setIndexStatusForRoot(rootPath: string, status: SqryIndexStatus): void {
    this.treeDataProvider.setIndexStatusForRoot(rootPath, status);
  }

  /**
   * Atomically replace the entire index status map.
   * Clears stale entries for removed roots and fires a tree refresh.
   */
  public replaceIndexStatusMap(map: Map<string, SqryIndexStatus>): void {
    this.treeDataProvider.replaceIndexStatusMap(map);
  }

  /**
   * Get the per-root index status map.
   */
  public getIndexStatusMap(): Map<string, SqryIndexStatus> {
    return this.treeDataProvider.getIndexStatusMap();
  }

  /**
   * Forward STEP_5 loading-phase transitions to the tree provider.
   * `loading` collapses the tree to a skeleton row.
   */
  public setLoadingPhase(phase: "loading" | "ready" | "failed", reason?: string): void {
    this.treeDataProvider.setLoadingPhase(phase, reason);
  }

  /** Replace the aggregate workspace status surface (STEP_5). */
  public setWorkspaceStatus(status: SqryWorkspaceStatus | null): void {
    this.treeDataProvider.setWorkspaceStatus(status);
  }

  /**
   * Clear search results and return to showing index stats.
   */
  public clearResults(): void {
    this.treeDataProvider.clearResults();
  }

  /**
   * Load more items for pagination.
   * Called when user clicks "Load More" button.
   */
  public async loadMore(
    itemType: "symbols" | "files" | "languageFiles" | "crossLanguage",
    nextOffset: number,
    language?: string,
    rootPath?: string,
  ): Promise<void> {
    await this.treeDataProvider.loadMore(itemType, nextOffset, language, rootPath);
  }

  // ---------------------------------------------------------------------------
  // Filter / sort delegation
  // ---------------------------------------------------------------------------

  /**
   * Apply new filter criteria to the current result set.
   */
  public setFilters(filters: Partial<ActiveFilters>): void {
    this.treeDataProvider.setFilters(filters);
  }

  /**
   * Change the active sort order.
   */
  public setSortOrder(order: SortOrder): void {
    this.treeDataProvider.setSortOrder(order);
  }

  /**
   * Clear all filters and reset sort order to default.
   */
  public clearFilters(): void {
    this.treeDataProvider.clearFilters();
  }

  /**
   * Return a human-readable filter summary string, or empty string.
   */
  public getFilterSummary(): string {
    return this.treeDataProvider.getFilterSummary();
  }

  /**
   * Return the distinct languages available in the unfiltered result set.
   */
  public getAvailableLanguages(): string[] {
    return this.treeDataProvider.getAvailableLanguages();
  }

  /**
   * Return the distinct symbol kinds available in the unfiltered result set.
   */
  public getAvailableKinds(): string[] {
    return this.treeDataProvider.getAvailableKinds();
  }

  /**
   * Return the current (filtered/sorted) symbol results.
   * Used by the export command to obtain the symbols displayed in the panel.
   */
  public getSymbols(): SqrySymbolResult[] {
    return this.treeDataProvider.getSymbols();
  }

  public dispose(): void {
    this.treeView.dispose();
  }
}

function resolveFilePath(filePath: string): string {
  if (path.isAbsolute(filePath)) {
    return filePath;
  }
  const workspace = vscode.workspace.workspaceFolders?.[0];
  if (workspace) {
    return path.join(workspace.uri.fsPath, filePath);
  }
  return filePath;
}
