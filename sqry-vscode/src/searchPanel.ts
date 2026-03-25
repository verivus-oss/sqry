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
      command: "vscode.open",
      arguments: [vscode.Uri.file(filePath)],
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

  constructor(language: string, fileCount?: number) {
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
  constructor(count: number) {
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
        command: "vscode.open",
        arguments: [vscode.Uri.file(filePath)],
      };
    }
  }
}

// ===== CD Predicate Tree Items =====

/**
 * Root item for duplicate symbol groups
 */
class SqryDuplicatesItem extends vscode.TreeItem {
  constructor(groupCount: number | null, symbolCount: number | null) {
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
  constructor(cycleCount: number | null) {
    super("Circular Dependencies", vscode.TreeItemCollapsibleState.Collapsed);
    if (cycleCount === null) {
      this.description = "expand to check";
    } else {
      this.description = cycleCount > 0 ? `${cycleCount} cycles` : "none";
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
  constructor(count: number | null, truncated?: boolean) {
    super("Unused Code", vscode.TreeItemCollapsibleState.Collapsed);
    if (count === null) {
      this.description = "expand to check";
    } else if (count === 0) {
      this.description = "none";
    } else {
      this.description = truncated ? `${count}+ symbols` : `${count} symbols`;
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
      command: "vscode.open",
      arguments: [
        vscode.Uri.file(filePath),
        {
          selection: new vscode.Range(
            Math.max((symbol.startLine ?? 1) - 1, 0),
            0,
            Math.max((symbol.startLine ?? 1) - 1, 0),
            0,
          ),
        },
      ],
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
      command: "vscode.open",
      arguments: [
        vscode.Uri.file(filePath),
        {
          selection: new vscode.Range(
            Math.max(match.line - 1, 0),
            0,
            Math.max(match.line - 1, 0),
            0,
          ),
        },
      ],
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
  ) {
    const remaining = total - currentlyShown;
    super(`Load More (${remaining} remaining)`, vscode.TreeItemCollapsibleState.None);
    this.iconPath = new vscode.ThemeIcon("ellipsis");
    this.command = {
      title: "Load More",
      command: "sqry.loadMore",
      arguments: [itemType, nextOffset, language],
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

  // Filter / sort state
  private unfilteredSymbols: SqrySymbolResult[] = [];
  private activeFilters: ActiveFilters = makeDefaultFilters();
  private activeSortOrder: SortOrder = "default";

  // Cache for lazy-loaded data with pagination metadata
  private cachedSymbolsResult: SqryListSymbolsResult | null = null;
  private readonly cachedSymbolsByKind: Map<string, SqryListSymbolsResult> = new Map();
  private cachedFilesResult: SqryListFilesResult | null = null;
  private readonly cachedLanguageFiles: Map<string, SqryListFilesByLanguageResult> = new Map();
  private cachedCrossLanguageResult: SqryListCrossLanguageRelationsResult | null = null;
  private readonly cachedRelationsByPair: Map<string, SqryListCrossLanguageRelationsResult> = new Map();
  // CD Predicate caches
  private cachedDuplicatesResult: SqryListDuplicateGroupsResult | null = null;
  private cachedCircularResult: SqryListCircularDependenciesResult | null = null;
  private cachedUnusedResult: SqryListUnusedSymbolsResult | null = null;
  private loadingSymbols = false;
  private readonly loadingSymbolsKind: Set<string> = new Set();
  private loadingFiles = false;
  private readonly loadingLanguageFiles: Set<string> = new Set();
  private loadingCrossLanguage = false;
  private readonly loadingRelationsPair: Set<string> = new Set();
  // CD Predicate loading states
  private loadingDuplicates = false;
  private loadingCircular = false;
  private loadingUnused = false;

  constructor(
    private readonly client: SqryClient | null,
    outputChannel: vscode.OutputChannel | null = null,
  ) {
    this.outputChannel = outputChannel;
  }

  private log(message: string): void {
    this.outputChannel?.appendLine(`[sqry] ${message}`);
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
    this.cachedSymbolsResult = null;
    this.cachedSymbolsByKind.clear();
    this.cachedFilesResult = null;
    this.cachedLanguageFiles.clear();
    this.cachedCrossLanguageResult = null;
    this.cachedRelationsByPair.clear();
    // Clear CD predicate caches
    this.cachedDuplicatesResult = null;
    this.cachedCircularResult = null;
    this.cachedUnusedResult = null;
    this._onDidChangeTreeData.fire();
  }

  public setIndexStatusForRoot(rootPath: string, status: SqryIndexStatus): void {
    this.indexStatusMap.set(rootPath, status);
    // Also update the single-root status for backward compat (use latest)
    this.indexStatus = status;
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
  ): Promise<void> {
    const activeClient = this.client;
    if (!activeClient) {
      this.log("Cannot load more: client not available");
      return;
    }

    const workspace = vscode.workspace.workspaceFolders?.[0];

    if (itemType === "symbols") {
      await this.loadMoreSymbols(workspace, nextOffset);
    } else if (itemType === "files") {
      await this.loadMoreFiles(workspace, nextOffset);
    } else if (itemType === "languageFiles" && language) {
      await this.loadMoreLanguageFiles(workspace, language, nextOffset);
    } else if (itemType === "crossLanguage") {
      await this.loadMoreCrossLanguage(workspace, nextOffset);
    }
  }

  private async loadMoreSymbols(workspace: vscode.WorkspaceFolder | undefined, offset: number): Promise<void> {
    const activeClient = this.client;
    if (this.loadingSymbols || !activeClient) {
      return;
    }

    this.loadingSymbols = true;
    this.log(`Loading more symbols from offset ${offset}...`);
    try {
      const newResult = await activeClient.listSymbols(workspace, offset, DEFAULT_SYMBOL_LIMIT);

      // Merge with existing cache
      if (this.cachedSymbolsResult) {
        this.cachedSymbolsResult = {
          symbols: [...this.cachedSymbolsResult.symbols, ...newResult.symbols],
          total: newResult.total,
          offset: newResult.offset,
          limit: newResult.limit,
          has_more: newResult.has_more,
        };
      } else {
        this.cachedSymbolsResult = newResult;
      }

      this.log(`Loaded ${newResult.symbols.length} more symbols (total shown: ${this.cachedSymbolsResult.symbols.length} of ${newResult.total})`);
      this._onDidChangeTreeData.fire();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading more symbols: ${message}`);
      void vscode.window.showErrorMessage(`sqry: Failed to load more symbols: ${message}`);
    } finally {
      this.loadingSymbols = false;
    }
  }

  private async loadMoreFiles(workspace: vscode.WorkspaceFolder | undefined, offset: number): Promise<void> {
    const activeClient = this.client;
    if (this.loadingFiles || !activeClient) {
      return;
    }

    this.loadingFiles = true;
    this.log(`Loading more files from offset ${offset}...`);
    try {
      const newResult = await activeClient.listFiles(workspace, offset, DEFAULT_FILE_LIMIT);

      // Merge with existing cache
      if (this.cachedFilesResult) {
        this.cachedFilesResult = {
          files: [...this.cachedFilesResult.files, ...newResult.files],
          total: newResult.total,
          offset: newResult.offset,
          limit: newResult.limit,
          has_more: newResult.has_more,
        };
      } else {
        this.cachedFilesResult = newResult;
      }

      this.log(`Loaded ${newResult.files.length} more files (total shown: ${this.cachedFilesResult.files.length} of ${newResult.total})`);
      this._onDidChangeTreeData.fire();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading more files: ${message}`);
      void vscode.window.showErrorMessage(`sqry: Failed to load more files: ${message}`);
    } finally {
      this.loadingFiles = false;
    }
  }

  private async loadMoreLanguageFiles(
    workspace: vscode.WorkspaceFolder | undefined,
    language: string,
    offset: number,
  ): Promise<void> {
    const activeClient = this.client;
    if (this.loadingLanguageFiles.has(language) || !activeClient) {
      return;
    }

    this.loadingLanguageFiles.add(language);
    this.log(`Loading more ${language} files from offset ${offset}...`);
    try {
      const newResult = await activeClient.listFilesByLanguage(language, workspace, offset, DEFAULT_LANGUAGE_FILE_LIMIT);

      // Merge with existing cache for this language
      const cached = this.cachedLanguageFiles.get(language);
      if (cached) {
        this.cachedLanguageFiles.set(language, {
          language: newResult.language,
          files: [...cached.files, ...newResult.files],
          total: newResult.total,
          offset: newResult.offset,
          limit: newResult.limit,
          has_more: newResult.has_more,
        });
      } else {
        this.cachedLanguageFiles.set(language, newResult);
      }

      const updated = this.cachedLanguageFiles.get(language)!;
      this.log(`Loaded ${newResult.files.length} more ${language} files (total shown: ${updated.files.length} of ${newResult.total})`);
      this._onDidChangeTreeData.fire();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading more ${language} files: ${message}`);
      void vscode.window.showErrorMessage(`sqry: Failed to load more ${language} files: ${message}`);
    } finally {
      this.loadingLanguageFiles.delete(language);
    }
  }

  private async loadMoreCrossLanguage(
    workspace: vscode.WorkspaceFolder | undefined,
    offset: number,
  ): Promise<void> {
    const activeClient = this.client;
    if (this.loadingCrossLanguage || !activeClient) {
      return;
    }

    this.loadingCrossLanguage = true;
    this.log(`Loading more cross-language relations from offset ${offset}...`);
    try {
      const newResult = await activeClient.listCrossLanguageRelations(workspace, offset, DEFAULT_CROSS_LANGUAGE_LIMIT);

      // Merge with existing cache, preserving overflow from first page
      if (this.cachedCrossLanguageResult) {
        this.cachedCrossLanguageResult = {
          relations: [...this.cachedCrossLanguageResult.relations, ...newResult.relations],
          total: newResult.total,
          offset: newResult.offset,
          limit: newResult.limit,
          has_more: newResult.has_more,
          // Preserve overflow from first page (static metadata that doesn't change with pagination)
          overflow: this.cachedCrossLanguageResult.overflow ?? newResult.overflow,
        };
      } else {
        this.cachedCrossLanguageResult = newResult;
      }

      this.log(`Loaded ${newResult.relations.length} more cross-language relations (total shown: ${this.cachedCrossLanguageResult.relations.length} of ${newResult.total})`);
      this._onDidChangeTreeData.fire();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading more cross-language relations: ${message}`);
      void vscode.window.showErrorMessage(`sqry: Failed to load more cross-language relations: ${message}`);
    } finally {
      this.loadingCrossLanguage = false;
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

  /** Get children for root level (no parent element). */
  private getRootChildren(): vscode.TreeItem[] {
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

    // Multi-root: show per-root grouping when >1 root has status
    const folders = vscode.workspace.workspaceFolders ?? [];
    if (folders.length > 1 && this.indexStatusMap.size > 0) {
      return folders.map(f => new SqryWorkspaceRootItem(
        f.uri.fsPath,
        f.name,
        this.indexStatusMap.get(f.uri.fsPath),
      ));
    }

    // Show index stats directly at root level (flat structure per UX guidelines)
    if (this.indexStatus) {
      return this.buildStatsItems(this.indexStatus);
    }

    // No stats available yet - welcome view will show via viewsWelcome
    return [];
  }

  /** Get children for a specific element based on its type. */
  private getElementChildren(element: vscode.TreeItem): vscode.ProviderResult<vscode.TreeItem[]> {
    if (element instanceof SqryWorkspaceRootItem) {
      const status = this.indexStatusMap.get(element.rootPath);
      if (status) {
        return this.buildStatsItems(status);
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
      return this.getLanguageFileChildren(element.language);
    }
    if (element instanceof SqryCrossLanguageItem) {
      return this.getCrossLanguageChildren();
    }
    if (element instanceof SqrySymbolKindItem) {
      return this.getSymbolsByKind(element.kind);
    }
    if (element instanceof SqryFileLanguageItem) {
      return this.getFilesByLanguage(element.language);
    }
    if (element instanceof SqryLanguagePairItem) {
      const [source, target] = element.pair.split("→");
      return this.getCrossLanguageRelationsByPair(source, target);
    }
    if (element instanceof SqryDuplicatesItem) {
      return this.getDuplicateGroupsChildren();
    }
    if (element instanceof SqryDuplicateGroupItem) {
      return this.getDuplicateGroupSymbols(element.group);
    }
    if (element instanceof SqryCircularItem) {
      return this.getCircularDependenciesChildren();
    }
    if (element instanceof SqryUnusedItem) {
      return this.getUnusedSymbolsChildren();
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
    if (element.statType === "symbols") {
      // Check if grouped counts are available - show intermediate grouping layer
      const counts = this.indexStatus?.symbol_counts_by_kind;
      if (counts && Object.keys(counts).length > 0) {
        return Object.entries(counts)
          .sort((a, b) => b[1] - a[1]) // Sort by count descending
          .map(([kind, count]) => new SqrySymbolKindItem(kind, count));
      }
      // Fallback to flat symbol list
      return this.getSymbolChildren();
    }
    if (element.statType === "files") {
      // Check if file counts by language are available - show language grouping
      const counts = this.indexStatus?.file_counts_by_language;
      if (counts && Object.keys(counts).length > 0) {
        return Object.entries(counts)
          .sort((a, b) => b[1] - a[1]) // Sort by count descending
          .map(([lang, count]) => new SqryFileLanguageItem(lang, count));
      }
      // Fallback to flat file list
      return this.getFileChildren();
    }
    if (element.statType === "languages") {
      return this.getLanguageChildren();
    }
    return [];
  }

  private async getSymbolChildren(): Promise<vscode.TreeItem[]> {
    // Return cached if available
    if (this.cachedSymbolsResult) {
      return this.buildSymbolItems(this.cachedSymbolsResult);
    }

    // Avoid duplicate requests
    const activeClient = this.client;
    if (this.loadingSymbols || !activeClient) {
      const loadingItem = new vscode.TreeItem("Loading symbols...", vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingSymbols = true;
    this.log("Fetching symbols...");
    try {
      const workspace = vscode.workspace.workspaceFolders?.[0];
      this.cachedSymbolsResult = await activeClient.listSymbols(workspace, 0, DEFAULT_SYMBOL_LIMIT);
      this.log(`Loaded ${this.cachedSymbolsResult.symbols.length} of ${this.cachedSymbolsResult.total} symbols`);
      return this.buildSymbolItems(this.cachedSymbolsResult);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading symbols: ${message}`);
      const symbolsLoadErrorItem = new vscode.TreeItem(`Error: ${message}`, vscode.TreeItemCollapsibleState.None);
      symbolsLoadErrorItem.iconPath = new vscode.ThemeIcon("error");
      return [symbolsLoadErrorItem];
    } finally {
      this.loadingSymbols = false;
    }
  }

  private buildSymbolItems(result: SqryListSymbolsResult): vscode.TreeItem[] {
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
        new SqryLoadMoreItem("symbols", nextOffset, result.total, shown),
      );
    }

    return items;
  }

  private async getFileChildren(): Promise<vscode.TreeItem[]> {
    // Return cached if available
    if (this.cachedFilesResult) {
      return this.buildFileItems(this.cachedFilesResult);
    }

    // Avoid duplicate requests
    const activeClient = this.client;
    if (this.loadingFiles || !activeClient) {
      const loadingItem = new vscode.TreeItem("Loading files...", vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingFiles = true;
    this.log("Fetching files...");
    try {
      const workspace = vscode.workspace.workspaceFolders?.[0];
      this.cachedFilesResult = await activeClient.listFiles(workspace, 0, DEFAULT_FILE_LIMIT);
      this.log(`Loaded ${this.cachedFilesResult.files.length} of ${this.cachedFilesResult.total} files`);
      return this.buildFileItems(this.cachedFilesResult);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading files: ${message}`);
      const filesLoadErrorItem = new vscode.TreeItem(`Error: ${message}`, vscode.TreeItemCollapsibleState.None);
      filesLoadErrorItem.iconPath = new vscode.ThemeIcon("error");
      return [filesLoadErrorItem];
    } finally {
      this.loadingFiles = false;
    }
  }

  private buildFileItems(result: SqryListFilesResult): vscode.TreeItem[] {
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
        new SqryLoadMoreItem("files", nextOffset, result.total, shown),
      );
    }

    return items;
  }

  private getLanguageChildren(): vscode.TreeItem[] {
    // Languages are already available in indexStatus
    if (this.indexStatus?.languages) {
      return this.indexStatus.languages.map((lang) => new SqryLanguageItem(lang));
    }
    return [];
  }

  private async getLanguageFileChildren(language: string): Promise<vscode.TreeItem[]> {
    // Return cached if available
    const cached = this.cachedLanguageFiles.get(language);
    if (cached) {
      return this.buildLanguageFileItems(cached);
    }

    // Avoid duplicate requests
    const activeClient = this.client;
    if (this.loadingLanguageFiles.has(language) || !activeClient) {
      const loadingItem = new vscode.TreeItem(`Loading ${language} files...`, vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingLanguageFiles.add(language);
    this.log(`Fetching files for language: ${language}`);
    try {
      const workspace = vscode.workspace.workspaceFolders?.[0];
      const result = await activeClient.listFilesByLanguage(language, workspace, 0, DEFAULT_LANGUAGE_FILE_LIMIT);
      this.cachedLanguageFiles.set(language, result);
      this.log(`Loaded ${result.files.length} of ${result.total} ${language} files`);
      return this.buildLanguageFileItems(result);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading ${language} files: ${message}`);
      const languageFilesLoadErrorItem = new vscode.TreeItem(`Error: ${message}`, vscode.TreeItemCollapsibleState.None);
      languageFilesLoadErrorItem.iconPath = new vscode.ThemeIcon("error");
      return [languageFilesLoadErrorItem];
    } finally {
      this.loadingLanguageFiles.delete(language);
    }
  }

  private buildLanguageFileItems(result: SqryListFilesByLanguageResult): vscode.TreeItem[] {
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
        new SqryLoadMoreItem("languageFiles", nextOffset, result.total, shown, result.language),
      );
    }

    return items;
  }

  private async getCrossLanguageChildren(): Promise<vscode.TreeItem[]> {
    // Check if language pair counts are available - show intermediate grouping layer
    const counts = this.indexStatus?.relation_counts_by_pair;
    if (counts && Object.keys(counts).length > 0) {
      return Object.entries(counts)
        .sort((a, b) => b[1] - a[1]) // Sort by count descending
        .map(([pair, count]) => new SqryLanguagePairItem(pair, count));
    }

    // Fallback to flat list - Return cached if available
    if (this.cachedCrossLanguageResult) {
      return this.buildCrossLanguageItems(this.cachedCrossLanguageResult);
    }

    // Avoid duplicate requests
    const activeClient = this.client;
    if (this.loadingCrossLanguage || !activeClient) {
      const loadingItem = new vscode.TreeItem("Loading cross-language relations...", vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingCrossLanguage = true;
    this.log("Fetching cross-language relations...");
    try {
      const workspace = vscode.workspace.workspaceFolders?.[0];
      this.cachedCrossLanguageResult = await activeClient.listCrossLanguageRelations(workspace, 0, DEFAULT_CROSS_LANGUAGE_LIMIT);
      this.log(`Loaded ${this.cachedCrossLanguageResult.relations.length} of ${this.cachedCrossLanguageResult.total} cross-language relations`);
      return this.buildCrossLanguageItems(this.cachedCrossLanguageResult);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log(`Error loading cross-language relations: ${message}`);
      const crossLanguageLoadErrorItem = new vscode.TreeItem(`Error: ${message}`, vscode.TreeItemCollapsibleState.None);
      crossLanguageLoadErrorItem.iconPath = new vscode.ThemeIcon("error");
      return [crossLanguageLoadErrorItem];
    } finally {
      this.loadingCrossLanguage = false;
    }
  }

  private buildCrossLanguageItems(result: SqryListCrossLanguageRelationsResult): vscode.TreeItem[] {
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
        new SqryLoadMoreItem("crossLanguage", nextOffset, result.total, shown),
      );
    }

    return items;
  }

  /**
   * Get symbols filtered by kind using LSP-level filtering.
   * Uses per-kind caching to avoid redundant requests.
   */
  private async getSymbolsByKind(kind: string): Promise<vscode.TreeItem[]> {
    const cacheKey = kind.toLowerCase();

    // Return cached if available
    const cached = this.cachedSymbolsByKind.get(cacheKey);
    if (cached) {
      return this.buildSymbolItems(cached);
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
      const workspace = vscode.workspace.workspaceFolders?.[0];
      // Use LSP-level filtering by passing kind parameter
      const result = await activeClient.listSymbols(workspace, 0, DEFAULT_SYMBOL_LIMIT, kind);
      this.cachedSymbolsByKind.set(cacheKey, result);
      this.log(`Loaded ${result.symbols.length} of ${result.total} ${kind} symbols`);
      return this.buildSymbolItems(result);
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
  private async getFilesByLanguage(language: string): Promise<vscode.TreeItem[]> {
    return this.getLanguageFileChildren(language);
  }

  /**
   * Get cross-language relations filtered by language pair using LSP-level filtering.
   * Uses per-pair caching to avoid redundant requests.
   */
  private async getCrossLanguageRelationsByPair(sourceLang: string, targetLang: string): Promise<vscode.TreeItem[]> {
    const cacheKey = `${sourceLang.toLowerCase()}→${targetLang.toLowerCase()}`;

    // Return cached if available
    const cached = this.cachedRelationsByPair.get(cacheKey);
    if (cached) {
      return this.buildCrossLanguageItems(cached);
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
      const workspace = vscode.workspace.workspaceFolders?.[0];
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
      return this.buildCrossLanguageItems(result);
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
  private async getDuplicateGroupsChildren(): Promise<vscode.TreeItem[]> {
    // Return cached if available
    if (this.cachedDuplicatesResult) {
      return this.cachedDuplicatesResult.groups.map(group => new SqryDuplicateGroupItem(group));
    }

    // Avoid duplicate requests
    const activeClient = this.client;
    if (this.loadingDuplicates || !activeClient) {
      const loadingItem = new vscode.TreeItem("Loading duplicate groups...", vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingDuplicates = true;
    this.log("Fetching duplicate groups via LSP...");
    try {
      const workspace = vscode.workspace.workspaceFolders?.[0];
      const result = await activeClient.listDuplicateGroups(workspace, "body", 100);
      this.cachedDuplicatesResult = result;
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
      this.loadingDuplicates = false;
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
        command: "vscode.open",
        arguments: [
          vscode.Uri.file(filePath),
          {
            selection: new vscode.Range(
              symbol.location.range.start.line,
              symbol.location.range.start.character,
              symbol.location.range.end.line,
              symbol.location.range.end.character,
            ),
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
  private async getCircularDependenciesChildren(): Promise<vscode.TreeItem[]> {
    // Return cached if available
    if (this.cachedCircularResult) {
      return this.cachedCircularResult.cycles.map(cycle => new SqryCycleItem(cycle));
    }

    // Avoid duplicate requests
    const activeClient = this.client;
    if (this.loadingCircular || !activeClient) {
      const loadingItem = new vscode.TreeItem("Loading circular dependencies...", vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingCircular = true;
    this.log("Fetching circular dependencies via LSP...");
    try {
      const workspace = vscode.workspace.workspaceFolders?.[0];
      const result = await activeClient.listCircularDependencies(workspace, "calls", 100, false);
      this.cachedCircularResult = result;
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
      this.loadingCircular = false;
    }
  }

  /**
   * Get children for the Unused Code category.
   * Returns symbols that appear to be unused.
   */
  private async getUnusedSymbolsChildren(): Promise<vscode.TreeItem[]> {
    // Return cached if available
    if (this.cachedUnusedResult) {
      return this.buildUnusedSymbolItems(this.cachedUnusedResult);
    }

    // Avoid duplicate requests
    const activeClient = this.client;
    if (this.loadingUnused || !activeClient) {
      const loadingItem = new vscode.TreeItem("Loading unused symbols...", vscode.TreeItemCollapsibleState.None);
      loadingItem.iconPath = new vscode.ThemeIcon("loading~spin");
      return [loadingItem];
    }

    this.loadingUnused = true;
    this.log("Fetching unused symbols via LSP...");
    try {
      const workspace = vscode.workspace.workspaceFolders?.[0];
      const result = await activeClient.listUnusedSymbols(workspace, "all", 100);
      this.cachedUnusedResult = result;
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
      this.loadingUnused = false;
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
        command: "vscode.open",
        arguments: [
          vscode.Uri.file(filePath),
          {
            selection: new vscode.Range(
              symbol.location.range.start.line,
              symbol.location.range.start.character,
              symbol.location.range.end.line,
              symbol.location.range.end.character,
            ),
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
        `Showing ${result.symbols.length}+ (results truncated)`,
        vscode.TreeItemCollapsibleState.None,
      );
      truncationItem.iconPath = new vscode.ThemeIcon("info");
      truncationItem.contextValue = "sqry.truncation";
      items.push(truncationItem);
    }

    return items;
  }

  private buildStatsItems(status: SqryIndexStatus): vscode.TreeItem[] {
    const items: vscode.TreeItem[] = [];

    this.addCoreStats(items, status);
    this.addCDPredicates(items, status);
    this.addStatusIndicators(items, status);

    return items;
  }

  /** Add core index statistics (symbols, files, languages, cross-language). */
  private addCoreStats(items: vscode.TreeItem[], status: SqryIndexStatus): void {
    if (status.symbol_count !== undefined) {
      items.push(new SqryExpandableStatItem(
        "Symbols",
        status.symbol_count.toLocaleString(),
        "symbol-class",
        "symbols",
        `Click to browse ${status.symbol_count.toLocaleString()} indexed symbols`,
      ));
    }

    if (status.file_count !== undefined) {
      items.push(new SqryExpandableStatItem(
        "Files",
        status.file_count.toLocaleString(),
        "files",
        "files",
        `Click to browse ${status.file_count.toLocaleString()} indexed files`,
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
      ));
    }

    if (status.supports_relations && status.languages && status.languages.length > 1) {
      const crossLangCount = status.cross_language_relation_count ?? 0;
      items.push(new SqryCrossLanguageItem(crossLangCount));
    }
  }

  /** Add CD predicates (duplicates, circular dependencies, unused code). */
  private addCDPredicates(items: vscode.TreeItem[], status: SqryIndexStatus): void {
    if (!status.supports_relations) return;

    const duplicatesCount = this.cachedDuplicatesResult?.total_groups ?? null;
    const symbolsCount = this.cachedDuplicatesResult?.total_symbols ?? null;
    items.push(new SqryDuplicatesItem(duplicatesCount, symbolsCount));

    const cyclesCount = this.cachedCircularResult?.total_cycles ?? null;
    items.push(new SqryCircularItem(cyclesCount));

    const unusedCount = this.cachedUnusedResult?.total ?? null;
    const unusedTruncated = this.cachedUnusedResult?.truncated ?? false;
    items.push(new SqryUnusedItem(unusedCount, unusedTruncated));
  }

  /** Add status indicators (index age, stale warning, building). */
  private addStatusIndicators(items: vscode.TreeItem[], status: SqryIndexStatus): void {
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

  /**
   * Update index status for a specific workspace root.
   * Used in multi-root workspaces to track per-root status.
   */
  public setIndexStatusForRoot(rootPath: string, status: SqryIndexStatus): void {
    this.treeDataProvider.setIndexStatusForRoot(rootPath, status);
  }

  /**
   * Get the per-root index status map.
   */
  public getIndexStatusMap(): Map<string, SqryIndexStatus> {
    return this.treeDataProvider.getIndexStatusMap();
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
  ): Promise<void> {
    await this.treeDataProvider.loadMore(itemType, nextOffset, language);
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
