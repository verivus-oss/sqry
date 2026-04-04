import * as vscode from "vscode";

export interface GraphNode {
  id: string;
  label: string;
  kind?: string;
  file?: string;
  line?: number;
  language?: string;
}

export interface GraphEdge {
  source: string;
  target: string;
  label?: string;
}

const MAX_NODES = 500;
const MAX_EDGES = 2000;

export class SqryGraphPanel implements vscode.Disposable {
  private static readonly panels = new Map<string, SqryGraphPanel>();
  private readonly panel: vscode.WebviewPanel;
  private disposed = false;

  private constructor(
    panel: vscode.WebviewPanel,
    private readonly extensionUri: vscode.Uri,
    private readonly mode: string,
  ) {
    this.panel = panel;

    // Handle messages from webview
    this.panel.webview.onDidReceiveMessage(async (message) => {
      if (message.type === "navigateToFile" && message.file && message.line !== undefined) {
        const uri = vscode.Uri.file(message.file);
        const doc = await vscode.workspace.openTextDocument(uri);
        const editor = await vscode.window.showTextDocument(doc, vscode.ViewColumn.One);
        const pos = new vscode.Position(message.line, 0);
        editor.selection = new vscode.Selection(pos, pos);
        editor.revealRange(new vscode.Range(pos, pos));
      }
    });

    this.panel.onDidDispose(() => {
      this.disposed = true;
      SqryGraphPanel.panels.delete(this.mode);
    });
  }

  public static createOrShow(extensionUri: vscode.Uri, mode: string): SqryGraphPanel {
    const existing = SqryGraphPanel.panels.get(mode);
    if (existing && !existing.disposed) {
      existing.panel.reveal(vscode.ViewColumn.Beside);
      return existing;
    }

    const title = mode === "callGraph" ? "sqry: Call Graph" : "sqry: Dependencies";
    const panel = vscode.window.createWebviewPanel(
      `sqry.${mode}`,
      title,
      vscode.ViewColumn.Beside,
      {
        enableScripts: true,
        retainContextWhenHidden: false,
        localResourceRoots: [vscode.Uri.joinPath(extensionUri, "media")],
      },
    );

    const instance = new SqryGraphPanel(panel, extensionUri, mode);
    SqryGraphPanel.panels.set(mode, instance);

    // Set initial HTML
    panel.webview.html = instance.getHtml(panel.webview);

    return instance;
  }

  public sendGraphData(nodes: GraphNode[], edges: GraphEdge[]): void {
    // Truncate to caps
    const truncatedNodes = nodes.slice(0, MAX_NODES);
    const truncatedEdges = edges.slice(0, MAX_EDGES);
    const wasTruncated = nodes.length > MAX_NODES || edges.length > MAX_EDGES;

    this.panel.webview.postMessage({
      type: "graphData",
      nodes: truncatedNodes,
      edges: truncatedEdges,
      truncated: wasTruncated,
      totalNodes: nodes.length,
      totalEdges: edges.length,
    });
  }

  public sendError(message: string): void {
    this.panel.webview.postMessage({
      type: "error",
      message,
    });
  }

  public dispose(): void {
    this.panel.dispose();
  }

  private getHtml(webview: vscode.Webview): string {
    const nonce = getNonce();
    const scriptUri = webview.asWebviewUri(
      vscode.Uri.joinPath(this.extensionUri, "media", "graph.js"),
    );

    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'nonce-${nonce}'; style-src 'nonce-${nonce}';">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <style nonce="${nonce}">
    body { margin: 0; padding: 0; overflow: hidden; background: var(--vscode-editor-background); color: var(--vscode-editor-foreground); font-family: var(--vscode-font-family); }
    #graph-container { width: 100vw; height: 100vh; }
    #controls { position: absolute; top: 8px; right: 8px; display: flex; gap: 4px; z-index: 10; }
    #controls input { background: var(--vscode-input-background); color: var(--vscode-input-foreground); border: 1px solid var(--vscode-input-border); padding: 4px 8px; font-size: 12px; }
    #controls button { background: var(--vscode-button-background); color: var(--vscode-button-foreground); border: none; padding: 4px 8px; cursor: pointer; font-size: 12px; }
    #status { position: absolute; bottom: 8px; left: 8px; font-size: 11px; opacity: 0.7; }
    .node { cursor: pointer; }
    .node:focus { outline: 2px solid var(--vscode-focusBorder); outline-offset: 2px; }
    .node rect { fill: var(--vscode-badge-background); stroke: var(--vscode-badge-foreground); stroke-width: 1; }
    .node text { fill: var(--vscode-badge-foreground); font-size: 11px; }
    .node:hover rect { stroke-width: 2; }
    .edge path { stroke: var(--vscode-editorWidget-border); stroke-width: 1; fill: none; }
    .edge marker { fill: var(--vscode-editorWidget-border); }
    .error { padding: 20px; text-align: center; color: var(--vscode-errorForeground); }
    .truncated { padding: 8px; text-align: center; color: var(--vscode-editorWarning-foreground); font-size: 12px; }
  </style>
</head>
<body>
  <div id="controls">
    <input type="text" id="search" placeholder="Search nodes..." aria-label="Search nodes">
    <button id="export-btn" title="Export as SVG">Export SVG</button>
  </div>
  <div id="graph-container" role="application" aria-label="Code dependency graph"></div>
  <div id="status"></div>
  <script nonce="${nonce}" src="${scriptUri}"></script>
</body>
</html>`;
  }
}

function getNonce(): string {
  const chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  let result = "";
  for (let i = 0; i < 32; i++) {
    result += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return result;
}
