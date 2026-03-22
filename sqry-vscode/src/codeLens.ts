import * as vscode from "vscode";
import { SqryClient } from "./sqryClient";
import { readSettings } from "./config";

interface LensData {
  readonly query: string;
  readonly workspace: vscode.WorkspaceFolder | undefined;
}

export class SqryCodeLensProvider
  implements vscode.CodeLensProvider, vscode.Disposable
{
  private readonly cache = new Map<string, Promise<number>>();
  private readonly disposables: vscode.Disposable[] = [];
  private enabled = readSettings().codeLensEnabled;

  constructor(private readonly client: SqryClient) {
    this.disposables.push(
      vscode.workspace.onDidChangeConfiguration((event) => {
        if (event.affectsConfiguration("sqry.codeLens.enabled")) {
          this.enabled = readSettings().codeLensEnabled;
          this.cache.clear();
          vscode.commands.executeCommand(
            "editor.action.codeLensRefresh",
          );
        }
      }),
      client.onDidChangeConfig(() => {
        this.enabled = readSettings().codeLensEnabled;
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
    if (!this.enabled) {
      return [];
    }

    const symbols = (await vscode.commands.executeCommand<
      vscode.DocumentSymbol[]
    >("vscode.executeDocumentSymbolProvider", document.uri)) ?? [];

    const codeLenses: vscode.CodeLens[] = [];
    const workspace = vscode.workspace.getWorkspaceFolder(document.uri);

    const visit = (
      items: vscode.DocumentSymbol[],
      ancestors: vscode.DocumentSymbol[],
    ) => {
      for (const item of items) {
        if (token.isCancellationRequested) {
          return;
        }

        if (isEligibleSymbol(item)) {
          const qualified = buildQualifiedName(item, ancestors);
          const range = new vscode.Range(
            item.selectionRange.start,
            item.selectionRange.start,
          );
          const lens = new vscode.CodeLens(range, {
            title: "Sqry callers: …",
            command: "",
          });
          (lens as { data?: LensData }).data = {
            query: `callers:${qualified}`,
            workspace,
          };
          codeLenses.push(lens);
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

    const cacheKey = `${data.workspace?.uri.fsPath ?? "root"}|${data.query}`;
    if (!this.cache.has(cacheKey)) {
      this.cache.set(
        cacheKey,
        this.client
          .runQuery(data.query, data.workspace)
          .then((result) => result.symbols.length)
          .catch((error) => {
            vscode.window.showErrorMessage(
              `sqry CodeLens error: ${error instanceof Error ? error.message : String(error)}`,
            );
            return 0;
          }),
      );
    }

    const count = await this.cache.get(cacheKey)!;
    if (token.isCancellationRequested) {
      return codeLens;
    }

    codeLens.command = {
      title: `Sqry callers: ${count}`,
      command: "sqry.runQueryInternal",
      arguments: [data.query],
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
