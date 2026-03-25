import * as vscode from "vscode";
import { SqryClient } from "./sqryClient";

const CACHE_TTL_MS = 10_000; // 10 seconds

interface CacheEntry {
  content: vscode.MarkdownString;
  timestamp: number;
}

export class SqryHoverProvider implements vscode.HoverProvider, vscode.Disposable {
  private readonly cache = new Map<string, CacheEntry>();
  private readonly disposables: vscode.Disposable[] = [];

  constructor(
    private readonly client: SqryClient,
    private readonly outputChannel: vscode.OutputChannel | null,
  ) {
    // Clear cache on config change (e.g. index rebuild)
    this.disposables.push(
      client.onDidChangeConfig(() => this.cache.clear()),
    );
  }

  async provideHover(
    document: vscode.TextDocument,
    position: vscode.Position,
    token: vscode.CancellationToken,
  ): Promise<vscode.Hover | null> {
    if (!vscode.workspace.getConfiguration("sqry").get<boolean>("hover.enabled", true)) {
      return null;
    }

    const symbolName = await this.getSymbolAtPosition(document, position);
    if (!symbolName || token.isCancellationRequested) {
      return null;
    }

    const cacheKey = `${document.uri.toString()}:${symbolName}`;
    const cached = this.cache.get(cacheKey);
    if (cached && (Date.now() - cached.timestamp) < CACHE_TTL_MS) {
      return new vscode.Hover(cached.content);
    }

    try {
      const workspace = vscode.workspace.getWorkspaceFolder(document.uri);
      if (!workspace || token.isCancellationRequested) return null;

      const [callerResult, calleeResult] = await Promise.all([
        this.client.runQuery(`callers:${symbolName}`, workspace),
        this.client.runQuery(`callees:${symbolName}`, workspace),
      ]);

      if (token.isCancellationRequested) return null;

      const callerCount = callerResult?.symbols?.length ?? 0;
      const calleeCount = calleeResult?.symbols?.length ?? 0;

      const md = new vscode.MarkdownString();
      md.appendMarkdown(`---\n**sqry** | ${callerCount} callers | ${calleeCount} callees`);

      this.cache.set(cacheKey, { content: md, timestamp: Date.now() });

      return new vscode.Hover(md);
    } catch {
      // Graceful degradation — never show errors in hover tooltips
      this.outputChannel?.appendLine(`[sqry] HoverProvider: error suppressed for symbol "${symbolName}"`);
      return null;
    }
  }

  private async getSymbolAtPosition(
    document: vscode.TextDocument,
    position: vscode.Position,
  ): Promise<string | null> {
    const wordRange = document.getWordRangeAtPosition(position);
    if (!wordRange) return null;
    return document.getText(wordRange);
  }

  public dispose(): void {
    this.cache.clear();
    this.disposables.forEach(d => d.dispose());
  }
}
