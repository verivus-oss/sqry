import * as vscode from "vscode";
import { resolveUriWithinWorkspace } from "./workspaceGuard";

// Command ids the LSP server surfaces through CodeLens (`codelens.rs`) and
// CodeActions (`code_action.rs`). The server advertises them in
// `executeCommandProvider`, so `vscode-languageclient` registers a client
// command for each and routes its invocation through `middleware.executeCommand`
// (see `node_modules/vscode-languageclient/lib/common/executeCommand.js`). The
// extension does NOT register these ids itself: doing so would collide with the
// language client's registration (the same crash that removing client-owned
// `sqry.index` from the advertisement fixed). Instead it renders their results
// from the middleware seam below.
export const SQRY_SHOW_CALLERS = "sqry.showCallers";
export const SQRY_SHOW_REFERENCES = "sqry.showReferences";
export const SQRY_EXPLAIN_SYMBOL = "sqry.explainSymbol";

const RENDERED_COMMANDS: ReadonlySet<string> = new Set([
  SQRY_SHOW_CALLERS,
  SQRY_SHOW_REFERENCES,
  SQRY_EXPLAIN_SYMBOL,
]);

/** True when `command` is one whose server result this module renders. */
export function isRenderedCommand(command: string): boolean {
  return RENDERED_COMMANDS.has(command);
}

/** Forwards a command to the server and renders its result when applicable. */
export type ExecuteCommandNext = (command: string, args: unknown[]) => Promise<unknown>;

/**
 * `middleware.executeCommand` seam. Passes non-rendered commands straight
 * through. For the three server-owned commands it awaits the server payload and
 * surfaces it in the editor (references peek for callers/references, a webview
 * for explain) instead of letting the result be silently discarded.
 */
export async function handleExecuteCommandResult(
  command: string,
  args: unknown[],
  next: ExecuteCommandNext,
  outputChannel: vscode.OutputChannel,
): Promise<unknown> {
  if (!isRenderedCommand(command)) {
    return next(command, args);
  }

  let result: unknown;
  try {
    result = await next(command, args);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    outputChannel.appendLine(`[sqry] ${command} failed: ${message}`);
    void vscode.window.showErrorMessage(`sqry: ${command} failed: ${message}`);
    return undefined;
  }

  try {
    if (command === SQRY_EXPLAIN_SYMBOL) {
      renderExplain(result);
    } else {
      await renderReferences(command, args, result);
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    outputChannel.appendLine(`[sqry] Failed to render ${command} result: ${message}`);
  }

  return result;
}

// ── references / callers ────────────────────────────────────────────────────

interface CommandContext {
  readonly uri: string;
  readonly position: { readonly line: number; readonly character: number };
}

interface LspPosition {
  readonly line: number;
  readonly character: number;
}

interface LspLocation {
  readonly uri: string;
  readonly range: { readonly start: LspPosition; readonly end: LspPosition };
}

/**
 * The CodeLens/CodeAction command argument the server emits: a single
 * `{ uri, position }` object. Returns it only when fully well-formed so a
 * malformed payload degrades to "no context" rather than a thrown peek.
 */
export function extractContext(args: unknown[]): CommandContext | undefined {
  const first = Array.isArray(args) ? args[0] : undefined;
  if (
    first &&
    typeof first === "object" &&
    typeof (first as CommandContext).uri === "string" &&
    isPosition((first as CommandContext).position)
  ) {
    return first as CommandContext;
  }
  return undefined;
}

function isPosition(value: unknown): value is LspPosition {
  return (
    !!value &&
    typeof value === "object" &&
    typeof (value as LspPosition).line === "number" &&
    typeof (value as LspPosition).character === "number"
  );
}

function isLspLocation(value: unknown): value is LspLocation {
  const loc = value as LspLocation | undefined;
  return (
    !!loc &&
    typeof loc === "object" &&
    typeof loc.uri === "string" &&
    !!loc.range &&
    isPosition(loc.range.start) &&
    isPosition(loc.range.end)
  );
}

/** Reads the well-formed `results.locations` array out of the server payload. */
export function extractLocations(result: unknown): LspLocation[] {
  const locations = (result as { results?: { locations?: unknown } } | undefined)?.results
    ?.locations;
  if (!Array.isArray(locations)) {
    return [];
  }
  return locations.filter(isLspLocation);
}

function symbolName(result: unknown): string | undefined {
  const name = (result as { symbol?: { name?: unknown } } | undefined)?.symbol?.name;
  return typeof name === "string" ? name : undefined;
}

/** Converts an LSP `Location` into the editor type the peek view consumes. */
export function toVscodeLocation(loc: LspLocation): vscode.Location {
  const range = new vscode.Range(
    loc.range.start.line,
    loc.range.start.character,
    loc.range.end.line,
    loc.range.end.character,
  );
  return new vscode.Location(vscode.Uri.parse(loc.uri), range);
}

async function renderReferences(
  command: string,
  args: unknown[],
  result: unknown,
): Promise<void> {
  const label = command === SQRY_SHOW_CALLERS ? "callers" : "references";
  const name = symbolName(result);
  // The peek view navigates to these result locations, so confine them to the
  // workspace and navigate using the guard's CANONICAL URI (dropping non-`file`
  // or out-of-workspace locations, and pinning symlinks to their real target so
  // a post-check retarget cannot redirect the peek outside).
  const guarded = extractLocations(result)
    .map((loc) => {
      const uri = resolveUriWithinWorkspace(vscode.Uri.parse(loc.uri));
      return uri ? { loc, uri } : undefined;
    })
    .filter((entry): entry is { loc: LspLocation; uri: vscode.Uri } => entry !== undefined);

  if (guarded.length === 0) {
    void vscode.window.showInformationMessage(
      `sqry: no ${label} found${name ? ` for ${name}` : ""}.`,
    );
    return;
  }

  const context = extractContext(args);
  const contextUri = context ? resolveUriWithinWorkspace(vscode.Uri.parse(context.uri)) : undefined;
  const anchorUri = contextUri ?? guarded[0].uri;
  const anchorPosition =
    context && contextUri
      ? new vscode.Position(context.position.line, context.position.character)
      : new vscode.Position(guarded[0].loc.range.start.line, guarded[0].loc.range.start.character);

  await vscode.commands.executeCommand(
    "editor.action.showReferences",
    anchorUri,
    anchorPosition,
    guarded.map(
      ({ loc, uri }) =>
        new vscode.Location(
          uri,
          new vscode.Range(
            loc.range.start.line,
            loc.range.start.character,
            loc.range.end.line,
            loc.range.end.character,
          ),
        ),
    ),
  );
}

// ── explain ─────────────────────────────────────────────────────────────────

interface ExplainPayload {
  readonly name?: string;
  readonly qualifiedName?: string;
  readonly language?: string;
  readonly signature?: string;
  readonly documentation?: string;
}

function renderExplain(result: unknown): void {
  if (!result || typeof result !== "object") {
    void vscode.window.showInformationMessage("sqry: no symbol at this position.");
    return;
  }
  SqryExplainPanel.show(result as ExplainPayload);
}

/** Escapes text for safe interpolation into the explain webview HTML. */
export function escapeHtml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/** Builds the static (script-free) HTML body for an explain payload. */
export function buildExplainHtml(payload: ExplainPayload): string {
  const title = payload.qualifiedName || payload.name || "Symbol";
  const rows: string[] = [];
  if (payload.language) {
    rows.push(`<div class="meta"><span class="key">Language</span> ${escapeHtml(payload.language)}</div>`);
  }
  const signature = payload.signature || payload.name || "";
  if (signature) {
    rows.push(`<pre class="signature"><code>${escapeHtml(signature)}</code></pre>`);
  }
  const documentation = payload.documentation?.trim();
  const docBlock = documentation
    ? `<div class="documentation">${escapeHtml(documentation)}</div>`
    : `<div class="documentation empty">No documentation available.</div>`;

  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline';">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <style>
    body { margin: 0; padding: 16px; background: var(--vscode-editor-background); color: var(--vscode-editor-foreground); font-family: var(--vscode-font-family); font-size: var(--vscode-font-size); }
    h1 { font-size: 1.2em; margin: 0 0 12px; word-break: break-all; }
    .meta { font-size: 0.9em; opacity: 0.85; margin-bottom: 8px; }
    .meta .key { font-weight: 600; margin-right: 6px; }
    .signature { background: var(--vscode-textCodeBlock-background); padding: 10px 12px; border-radius: 4px; overflow-x: auto; }
    .signature code { font-family: var(--vscode-editor-font-family, monospace); white-space: pre; }
    .documentation { margin-top: 12px; white-space: pre-wrap; line-height: 1.5; }
    .documentation.empty { opacity: 0.6; font-style: italic; }
  </style>
</head>
<body>
  <h1>${escapeHtml(title)}</h1>
  ${rows.join("\n  ")}
  ${docBlock}
</body>
</html>`;
}

/** Singleton webview that shows the most recent explain result. */
class SqryExplainPanel {
  private static current: SqryExplainPanel | undefined;
  private disposed = false;

  private constructor(private readonly panel: vscode.WebviewPanel) {
    this.panel.onDidDispose(() => {
      this.disposed = true;
      if (SqryExplainPanel.current === this) {
        SqryExplainPanel.current = undefined;
      }
    });
  }

  public static show(payload: ExplainPayload): void {
    const html = buildExplainHtml(payload);
    const existing = SqryExplainPanel.current;
    if (existing && !existing.disposed) {
      existing.panel.webview.html = html;
      existing.panel.title = SqryExplainPanel.titleFor(payload);
      existing.panel.reveal(vscode.ViewColumn.Beside, true);
      return;
    }

    const panel = vscode.window.createWebviewPanel(
      "sqry.explain",
      SqryExplainPanel.titleFor(payload),
      { viewColumn: vscode.ViewColumn.Beside, preserveFocus: true },
      { enableScripts: false, retainContextWhenHidden: false },
    );
    panel.webview.html = html;
    SqryExplainPanel.current = new SqryExplainPanel(panel);
  }

  private static titleFor(payload: ExplainPayload): string {
    return `sqry: Explain ${payload.name ?? "Symbol"}`;
  }
}
