import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";

/**
 * Workspace-containment guard for file navigation.
 *
 * sqry opens files whose paths come from index results, graph nodes, webview
 * messages, and diagnostic related-information. Those paths are effectively data
 * derived from the indexed repository, so a crafted absolute path, a `..`
 * sequence, or a symlink could point the editor at a file outside the user's
 * workspace. Every result-driven open routes through {@link resolveWithinWorkspace}
 * so only files inside an open workspace folder can be opened.
 */

/** A selection to reveal after opening; zero-based. `end` defaults to `start`. */
export interface GuardSelection {
  startLine: number;
  startCharacter?: number;
  endLine?: number;
  endCharacter?: number;
}

/**
 * Resolve `filePath` to a canonical absolute path.
 *
 * We resolve the longest EXISTING ancestor with `realpathSync.native` (which
 * follows symlinks) and then re-append the non-existent tail. This is what makes
 * the guard sound against a symlinked parent whose leaf does not exist yet:
 * `path.resolve` alone would leave `/ws/link/missing.ts` lexically "inside" the
 * workspace even when `link -> /outside`, opening a TOCTOU escape once the leaf
 * appears. By realpath-ing the existing prefix, `/ws/link/missing.ts` resolves
 * to `/outside/missing.ts` and is correctly rejected.
 */
/**
 * A URI scheme is case-insensitive per RFC 3986, and `Uri.parse` keeps the case
 * it was given, so `FILE`, `File` and `file` all denote the same scheme.
 */
function isFileScheme(scheme: string | undefined): boolean {
  return typeof scheme === "string" && scheme.toLowerCase() === "file";
}

function canonicalize(filePath: string): string {
  const resolved = path.resolve(filePath);
  let prefix = resolved;
  const tail: string[] = [];
  for (;;) {
    try {
      const real = fs.realpathSync.native(prefix);
      return tail.length > 0 ? path.join(real, ...tail.reverse()) : real;
    } catch {
      const parent = path.dirname(prefix);
      if (parent === prefix) {
        // Reached the filesystem root without an existing ancestor; fall back to
        // the lexical resolution (already `..`-normalized).
        return resolved;
      }
      tail.push(path.basename(prefix));
      prefix = parent;
    }
  }
}

/**
 * Return true when `child` is the same path as, or nested inside, `root`.
 * Both inputs must already be canonical absolute paths.
 */
function isInside(root: string, child: string): boolean {
  const relative = path.relative(root, child);
  // `relative` is empty for the root itself, starts with `..` when it climbs
  // out, and is absolute (e.g. a different Windows drive) when unrelated.
  return (
    relative === "" ||
    (relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative))
  );
}

/**
 * Resolve `filePath` and confirm it lies within one of the open workspace
 * folders. Returns a `file:` {@link vscode.Uri} when contained, otherwise
 * `undefined` (including when no folder is open, so nothing is "within").
 */
export function resolveWithinWorkspace(filePath: string): vscode.Uri | undefined {
  if (!filePath) {
    return undefined;
  }
  const folders = vscode.workspace.workspaceFolders;
  if (!folders || folders.length === 0) {
    return undefined;
  }
  const candidate = canonicalize(filePath);
  for (const folder of folders) {
    if (!isFileScheme(folder.uri.scheme)) {
      continue;
    }
    const root = canonicalize(folder.uri.fsPath);
    if (isInside(root, candidate)) {
      return vscode.Uri.file(candidate);
    }
  }
  return undefined;
}

/**
 * Resolve a {@link vscode.Uri} within the workspace. Non-`file` schemes are
 * rejected outright; `file:` URIs are checked by their filesystem path.
 *
 * The scheme is compared case-insensitively: RFC 3986 defines it that way, and
 * `Uri.parse` preserves whatever case it was handed, so a valid `FILE:///...`
 * would otherwise be dropped as though it were a foreign scheme.
 */
export function resolveUriWithinWorkspace(uri: vscode.Uri): vscode.Uri | undefined {
  if (!isFileScheme(uri.scheme)) {
    return undefined;
  }
  return resolveWithinWorkspace(uri.fsPath);
}

function applySelection(editor: vscode.TextEditor, selection: GuardSelection): void {
  const start = new vscode.Position(selection.startLine, selection.startCharacter ?? 0);
  const end = new vscode.Position(
    selection.endLine ?? selection.startLine,
    selection.endCharacter ?? selection.startCharacter ?? 0,
  );
  editor.selection = new vscode.Selection(start, end);
  editor.revealRange(new vscode.Range(start, end));
}

/**
 * Open `filePath` in an editor only when it lies within the workspace. When it
 * does not, show a warning and return `undefined` rather than opening it.
 */
export async function openFileWithinWorkspace(
  filePath: string,
  options?: { selection?: GuardSelection; viewColumn?: vscode.ViewColumn },
): Promise<vscode.TextEditor | undefined> {
  const uri = resolveWithinWorkspace(filePath);
  if (!uri) {
    void vscode.window.showWarningMessage(
      `sqry did not open "${filePath}" because it is outside the current workspace.`,
    );
    return undefined;
  }
  const doc = await vscode.workspace.openTextDocument(uri);
  const editor = await vscode.window.showTextDocument(doc, options?.viewColumn);
  if (options?.selection) {
    applySelection(editor, options.selection);
  }
  return editor;
}
