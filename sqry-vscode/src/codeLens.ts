import * as vscode from "vscode";
import { SqryClient } from "./sqryClient";
import { readSettings } from "./config";

/** Segment type for CodeLens display. */
type Segment = "callers" | "callees";

interface LensData {
  readonly symbolName: string;
  readonly segment: Segment;
  readonly workspace: vscode.WorkspaceFolder | undefined;
}

/** Maximum number of symbols per document to avoid excessive requests. */
const MAX_SYMBOLS_PER_DOCUMENT = 100;

/** Sentinel value indicating a batch request error for this symbol. */
const ERROR_COUNT = -1;

export class SqryCodeLensProvider
  implements vscode.CodeLensProvider, vscode.Disposable
{
  private readonly cache = new Map<
    string,
    { callers: number; callees: number }
  >();
  private readonly disposables: vscode.Disposable[] = [];
  private enabled = readSettings().codeLensEnabled;
  private segments: Segment[] = vscode.workspace
    .getConfiguration("sqry")
    .get<string[]>("codeLens.segments", ["callers", "callees"])
    .filter((s): s is Segment => s === "callers" || s === "callees");

  constructor(private readonly client: SqryClient) {
    this.disposables.push(
      vscode.workspace.onDidChangeConfiguration((event) => {
        if (
          event.affectsConfiguration("sqry.codeLens.enabled") ||
          event.affectsConfiguration("sqry.codeLens.segments")
        ) {
          this.enabled = readSettings().codeLensEnabled;
          this.segments = vscode.workspace
            .getConfiguration("sqry")
            .get<string[]>("codeLens.segments", ["callers", "callees"])
            .filter(
              (s): s is Segment => s === "callers" || s === "callees",
            );
          this.cache.clear();
          vscode.commands.executeCommand("editor.action.codeLensRefresh");
        }
      }),
      client.onDidChangeConfig(() => {
        this.enabled = readSettings().codeLensEnabled;
        this.cache.clear();
      }),
    );
  }

  public dispose(): void {
    this.disposables.forEach((d) => d.dispose());
    this.cache.clear();
  }

  async provideCodeLenses(
    document: vscode.TextDocument,
    token: vscode.CancellationToken,
  ): Promise<vscode.CodeLens[]> {
    if (!this.enabled || this.segments.length === 0) {
      return [];
    }

    const symbols =
      (await vscode.commands.executeCommand<vscode.DocumentSymbol[]>(
        "vscode.executeDocumentSymbolProvider",
        document.uri,
      )) ?? [];

    const codeLenses: vscode.CodeLens[] = [];
    const workspace = vscode.workspace.getWorkspaceFolder(document.uri);
    let symbolCount = 0;

    const visit = (
      items: vscode.DocumentSymbol[],
      ancestors: vscode.DocumentSymbol[],
    ) => {
      for (const item of items) {
        if (token.isCancellationRequested) {
          return;
        }
        if (symbolCount >= MAX_SYMBOLS_PER_DOCUMENT) {
          return;
        }

        if (isEligibleSymbol(item)) {
          symbolCount++;
          const qualified = buildQualifiedName(item, ancestors);
          const range = new vscode.Range(
            item.selectionRange.start,
            item.selectionRange.start,
          );

          for (const segment of this.segments) {
            const lens = new vscode.CodeLens(range, {
              title: `Sqry ${segment}: …`,
              command: "",
            });
            (lens as { data?: LensData }).data = {
              symbolName: qualified,
              segment,
              workspace,
            };
            codeLenses.push(lens);
          }
        }

        if (item.children?.length) {
          visit(item.children, [...ancestors, item]);
        }
      }
    };

    visit(symbols, []);
    return codeLenses;
  }

  async resolveCodeLens(
    codeLens: vscode.CodeLens,
    token: vscode.CancellationToken,
  ): Promise<vscode.CodeLens> {
    if (!this.enabled) {
      return codeLens;
    }
    const data = (codeLens as vscode.CodeLens & { data?: LensData }).data;
    if (!data) {
      return codeLens;
    }

    const cacheKey = `${data.workspace?.uri.fsPath ?? "root"}|${data.symbolName}`;

    if (!this.cache.has(cacheKey)) {
      try {
        const result = await this.client.batchCallerCalleeCount(
          [{ name: data.symbolName }],
          data.workspace,
        );
        if (result.counts.length > 0) {
          this.cache.set(cacheKey, {
            callers: result.counts[0].callers,
            callees: result.counts[0].callees,
          });
        } else {
          this.cache.set(cacheKey, { callers: 0, callees: 0 });
        }
      } catch {
        this.cache.set(cacheKey, {
          callers: ERROR_COUNT,
          callees: ERROR_COUNT,
        });
      }
    }

    if (token.isCancellationRequested) {
      return codeLens;
    }

    const counts = this.cache.get(cacheKey)!;
    const count = counts[data.segment];
    const title =
      count === ERROR_COUNT
        ? `Sqry ${data.segment}: ?`
        : `Sqry ${data.segment}: ${count}`;

    codeLens.command = {
      title,
      command: "sqry.runQueryInternal",
      arguments: [`${data.segment}:${data.symbolName}`],
    };
    return codeLens;
  }
}

function isEligibleSymbol(symbol: vscode.DocumentSymbol): boolean {
  return (
    symbol.kind === vscode.SymbolKind.Function ||
    symbol.kind === vscode.SymbolKind.Method ||
    symbol.kind === vscode.SymbolKind.Constructor
  );
}

function buildQualifiedName(
  symbol: vscode.DocumentSymbol,
  ancestors: vscode.DocumentSymbol[],
): string {
  const names = ancestors
    .filter(
      (ancestor) =>
        ancestor.kind === vscode.SymbolKind.Class ||
        ancestor.kind === vscode.SymbolKind.Struct ||
        ancestor.kind === vscode.SymbolKind.Namespace,
    )
    .map((ancestor) => ancestor.name);
  names.push(symbol.name);
  return names.join(".");
}
