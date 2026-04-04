import * as vscode from "vscode";
import { SqryClient } from "./sqryClient";
import {
  SqrySearchItem,
  SqryCycle,
  SqryDuplicateGroup,
} from "./lspProtocol";

const MAX_DIAGNOSTICS_PER_FILE = 500;
const MAX_DIAGNOSTICS_PER_WORKSPACE = 5000;

/**
 * Publishes sqry findings (unused symbols, circular dependencies, duplicate code)
 * to the native VS Code Problems panel as diagnostics.
 *
 * Unused code receives `DiagnosticTag.Unnecessary` so VS Code automatically
 * renders it with the "faded" style — delivering inline unused-code fading.
 */
export class SqryDiagnosticsProvider implements vscode.Disposable {
  constructor(
    private readonly collection: vscode.DiagnosticCollection,
    private readonly client: SqryClient,
    private readonly outputChannel: vscode.OutputChannel | null,
  ) {}

  /**
   * Refresh diagnostics for a single file (unused symbols only —
   * cycles/duplicates are workspace-level and require a full scan).
   */
  public async refreshForFile(
    uri: vscode.Uri,
    workspace: vscode.WorkspaceFolder,
  ): Promise<void> {
    if (!this.isEnabled()) {
      return;
    }

    try {
      const diagnostics: vscode.Diagnostic[] = [];

      if (this.isUnusedCodeEnabled()) {
        const result = await this.client.listUnusedSymbols(
          workspace,
          "all",
          MAX_DIAGNOSTICS_PER_WORKSPACE,
        );

        for (const symbol of result.symbols) {
          const symbolUri = vscode.Uri.parse(symbol.location.uri);
          if (symbolUri.toString() !== uri.toString()) {
            continue;
          }
          diagnostics.push(createUnusedDiagnostic(symbol));
        }
      }

      if (diagnostics.length > 0) {
        this.collection.set(uri, truncatePerFile(diagnostics, uri));
      }
    } catch (error) {
      this.log(
        `Failed to refresh diagnostics for ${uri.fsPath}: ${errorMessage(error)}`,
      );
    }
  }

  /**
   * Refresh diagnostics for all currently open editors.
   */
  public async refreshForOpenEditors(
    workspace: vscode.WorkspaceFolder,
  ): Promise<void> {
    if (!this.isEnabled()) {
      return;
    }

    const openUris = new Set(
      vscode.window.visibleTextEditors.map((e) => e.document.uri.toString()),
    );

    if (openUris.size === 0) {
      return;
    }

    try {
      if (this.isUnusedCodeEnabled()) {
        const result = await this.client.listUnusedSymbols(
          workspace,
          "all",
          MAX_DIAGNOSTICS_PER_WORKSPACE,
        );

        const byUri = groupByUri(
          result.symbols.map((s) => ({
            uri: s.location.uri,
            diagnostic: createUnusedDiagnostic(s),
          })),
        );

        for (const [uriStr, diags] of byUri) {
          if (openUris.has(uriStr)) {
            const parsedUri = vscode.Uri.parse(uriStr);
            this.collection.set(parsedUri, truncatePerFile(diags, parsedUri));
          }
        }
      }
    } catch (error) {
      this.log(
        `Failed to refresh diagnostics for open editors: ${errorMessage(error)}`,
      );
    }
  }

  /**
   * Full workspace scan: fetch all finding types (unused, cycles, duplicates)
   * and publish diagnostics for every file that has findings.
   */
  public async scanWorkspace(
    workspace: vscode.WorkspaceFolder,
  ): Promise<void> {
    if (!this.isEnabled()) {
      return;
    }

    this.collection.clear();

    const allEntries: Array<{ uri: string; diagnostic: vscode.Diagnostic }> = [];
    let totalCount = 0;

    try {
      totalCount = await this.collectUnusedSymbols(workspace, allEntries, totalCount);
      totalCount = await this.collectCircularDependencies(workspace, allEntries, totalCount);
      totalCount = await this.collectDuplicateGroups(workspace, allEntries, totalCount);

      // Group by URI and publish
      const byUri = groupByUri(allEntries);
      for (const [uriStr, diags] of byUri) {
        const parsedUri = vscode.Uri.parse(uriStr);
        this.collection.set(parsedUri, truncatePerFile(diags, parsedUri));
      }

      this.log(`Workspace scan complete: ${totalCount} diagnostics published`);
    } catch (error) {
      this.log(`Workspace scan failed: ${errorMessage(error)}`);
    }
  }

  private async collectUnusedSymbols(
    workspace: vscode.WorkspaceFolder,
    entries: Array<{ uri: string; diagnostic: vscode.Diagnostic }>,
    count: number,
  ): Promise<number> {
    if (!this.isUnusedCodeEnabled() || count >= MAX_DIAGNOSTICS_PER_WORKSPACE) {
      return count;
    }
    const unused = await this.client.listUnusedSymbols(workspace, "all", MAX_DIAGNOSTICS_PER_WORKSPACE);
    for (const symbol of unused.symbols) {
      if (count >= MAX_DIAGNOSTICS_PER_WORKSPACE) {
        break;
      }
      entries.push({ uri: symbol.location.uri, diagnostic: createUnusedDiagnostic(symbol) });
      count++;
    }
    this.log(`Workspace scan: ${unused.symbols.length} unused symbols (total=${unused.total})`);
    return count;
  }

  private async collectCircularDependencies(
    workspace: vscode.WorkspaceFolder,
    entries: Array<{ uri: string; diagnostic: vscode.Diagnostic }>,
    count: number,
  ): Promise<number> {
    if (count >= MAX_DIAGNOSTICS_PER_WORKSPACE) {
      return count;
    }
    const cycles = await this.client.listCircularDependencies(workspace, "calls", MAX_DIAGNOSTICS_PER_WORKSPACE);
    for (const cycle of cycles.cycles) {
      for (const entry of createCycleDiagnostics(cycle)) {
        if (count >= MAX_DIAGNOSTICS_PER_WORKSPACE) {
          break;
        }
        entries.push(entry);
        count++;
      }
    }
    this.log(`Workspace scan: ${cycles.cycles.length} cycles (total=${cycles.total_cycles})`);
    return count;
  }

  private async collectDuplicateGroups(
    workspace: vscode.WorkspaceFolder,
    entries: Array<{ uri: string; diagnostic: vscode.Diagnostic }>,
    count: number,
  ): Promise<number> {
    if (count >= MAX_DIAGNOSTICS_PER_WORKSPACE) {
      return count;
    }
    const duplicates = await this.client.listDuplicateGroups(workspace, "body", MAX_DIAGNOSTICS_PER_WORKSPACE);
    for (const group of duplicates.groups) {
      for (const entry of createDuplicateDiagnostics(group)) {
        if (count >= MAX_DIAGNOSTICS_PER_WORKSPACE) {
          break;
        }
        entries.push(entry);
        count++;
      }
    }
    this.log(`Workspace scan: ${duplicates.groups.length} duplicate groups (total=${duplicates.total_groups})`);
    return count;
  }

  /** Clear all diagnostics. */
  public clear(): void {
    this.collection.clear();
  }

  /** Clear diagnostics for a specific file. */
  public clearFile(uri: vscode.Uri): void {
    this.collection.delete(uri);
  }

  public dispose(): void {
    this.collection.dispose();
  }

  private isEnabled(): boolean {
    return vscode.workspace
      .getConfiguration("sqry")
      .get<boolean>("diagnostics.enabled", true);
  }

  private isUnusedCodeEnabled(): boolean {
    return vscode.workspace
      .getConfiguration("sqry")
      .get<boolean>("diagnostics.unusedCode", true);
  }

  private log(message: string): void {
    this.outputChannel?.appendLine(`[sqry-diag] ${message}`);
  }
}

// ===== Diagnostic Factories =====

function createUnusedDiagnostic(symbol: SqrySearchItem): vscode.Diagnostic {
  const range = toRange(symbol.location.range);
  const diag = new vscode.Diagnostic(
    range,
    `'${symbol.name}' appears to be unused`,
    vscode.DiagnosticSeverity.Hint,
  );
  diag.tags = [vscode.DiagnosticTag.Unnecessary];
  diag.source = "sqry";
  diag.code = "sqry:unused";
  return diag;
}

function createCycleDiagnostics(
  cycle: SqryCycle,
): Array<{ uri: string; diagnostic: vscode.Diagnostic }> {
  const results: Array<{ uri: string; diagnostic: vscode.Diagnostic }> = [];
  const locations = cycle.member_locations ?? [];

  // Build the cycle chain string: A -> B -> C -> A
  const chainParts = [...cycle.members];
  if (chainParts.length > 0) {
    chainParts.push(chainParts[0]);
  }
  const chainStr = chainParts.join(" -> ");

  // Collect resolved member locations for relatedInformation
  const resolvedLocations: Array<{
    name: string;
    uri: string;
    range: vscode.Range;
  }> = [];
  for (const loc of locations) {
    if (loc.file && loc.line !== undefined) {
      const uri = loc.file.startsWith("file://")
        ? loc.file
        : vscode.Uri.file(loc.file).toString();
      const line = Math.max(0, loc.line - 1); // 1-based to 0-based
      const col = loc.column === undefined ? 0 : Math.max(0, loc.column - 1);
      resolvedLocations.push({
        name: loc.name,
        uri,
        range: new vscode.Range(line, col, line, col),
      });
    }
  }

  // Create a diagnostic for each member with a resolved location
  for (let i = 0; i < resolvedLocations.length; i++) {
    const member = resolvedLocations[i];
    const diag = new vscode.Diagnostic(
      member.range,
      `Part of circular dependency: ${chainStr}`,
      vscode.DiagnosticSeverity.Information,
    );
    diag.source = "sqry";
    diag.code = "sqry:cycle";

    // relatedInformation: other members in the cycle
    diag.relatedInformation = resolvedLocations
      .filter((_, idx) => idx !== i)
      .map(
        (other) =>
          new vscode.DiagnosticRelatedInformation(
            new vscode.Location(vscode.Uri.parse(other.uri), other.range),
            `Cycle member: ${other.name}`,
          ),
      );

    results.push({ uri: member.uri, diagnostic: diag });
  }

  return results;
}

function createDuplicateDiagnostics(
  group: SqryDuplicateGroup,
): Array<{ uri: string; diagnostic: vscode.Diagnostic }> {
  const results: Array<{ uri: string; diagnostic: vscode.Diagnostic }> = [];
  const symbols = group.symbols;

  for (let i = 0; i < symbols.length; i++) {
    const symbol = symbols[i];
    const range = toRange(symbol.location.range);
    const diag = new vscode.Diagnostic(
      range,
      `Duplicate of '${group.representative_name}' (${group.count} copies)`,
      vscode.DiagnosticSeverity.Information,
    );
    diag.source = "sqry";
    diag.code = "sqry:duplicate";

    // relatedInformation: other symbols in the group
    diag.relatedInformation = symbols
      .filter((_, idx) => idx !== i)
      .map(
        (other) =>
          new vscode.DiagnosticRelatedInformation(
            new vscode.Location(
              vscode.Uri.parse(other.location.uri),
              toRange(other.location.range),
            ),
            `Duplicate: ${other.name}`,
          ),
      );

    results.push({ uri: symbol.location.uri, diagnostic: diag });
  }

  return results;
}

// ===== Helpers =====

function toRange(range: {
  start: { line: number; character: number };
  end: { line: number; character: number };
}): vscode.Range {
  return new vscode.Range(
    range.start.line,
    range.start.character,
    range.end.line,
    range.end.character,
  );
}

function groupByUri(
  entries: Array<{ uri: string; diagnostic: vscode.Diagnostic }>,
): Map<string, vscode.Diagnostic[]> {
  const map = new Map<string, vscode.Diagnostic[]>();
  for (const entry of entries) {
    let list = map.get(entry.uri);
    if (!list) {
      list = [];
      map.set(entry.uri, list);
    }
    list.push(entry.diagnostic);
  }
  return map;
}

function truncatePerFile(
  diagnostics: vscode.Diagnostic[],
  _uri: vscode.Uri,
): vscode.Diagnostic[] {
  if (diagnostics.length <= MAX_DIAGNOSTICS_PER_FILE) {
    return diagnostics;
  }
  const truncated = diagnostics.slice(0, MAX_DIAGNOSTICS_PER_FILE);
  const summary = new vscode.Diagnostic(
    new vscode.Range(0, 0, 0, 0),
    `sqry: showing ${MAX_DIAGNOSTICS_PER_FILE} of ${diagnostics.length} findings`,
    vscode.DiagnosticSeverity.Information,
  );
  summary.source = "sqry";
  truncated.push(summary);
  return truncated;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
