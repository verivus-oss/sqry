/**
 * Workspace classifier — pure helpers for resolving the active
 * `.code-workspace` file path, parsing the optional `sqry.workspace`
 * block from a workspace file, and producing the JSON scaffold that
 * `sqry.editWorkspaceClassification` writes when the block is absent.
 *
 * No VS Code APIs are touched here. The functions accept and return
 * plain shapes so they can be unit tested without an extension host.
 */

import * as fs from "node:fs";
import * as path from "node:path";

export type ProjectRootMode = "gitRoot" | "folder" | "explicit";

/**
 * The shape persisted under `sqry.workspace` inside a `.code-workspace`
 * file. All fields are optional — an empty `sqry.workspace: {}` is a
 * valid scaffold that just opts the workspace into explicit
 * classification.
 */
export interface WorkspaceClassification {
  /** Explicit list of source roots (overrides auto-discovery). */
  readonly sourceRoots?: ReadonlyArray<string>;
  /** Glob patterns for folders that must NOT be treated as source roots. */
  readonly exclusions?: ReadonlyArray<string>;
  /** Folders that participate in the workspace but are not source roots. */
  readonly memberFolders?: ReadonlyArray<string>;
  /** How to detect implicit source roots. */
  readonly projectRootMode?: ProjectRootMode;
}

export interface ParsedWorkspaceFile {
  /** Folders array entries (raw `path` strings). */
  readonly folders: ReadonlyArray<{ readonly path: string; readonly name?: string }>;
  /** The `sqry.workspace` block, if present. */
  readonly classification: WorkspaceClassification | null;
}

/** Default scaffold inserted by `sqry.editWorkspaceClassification`. */
export const DEFAULT_CLASSIFICATION_SCAFFOLD: WorkspaceClassification = {
  sourceRoots: [],
  exclusions: [],
  memberFolders: [],
  projectRootMode: "gitRoot",
};

/**
 * Resolve the absolute path to the `.code-workspace` file currently
 * open in VS Code, or `null` when the user opened a single folder
 * (no workspace file).
 *
 * Accepts the `workspaceFile` URI returned by VS Code's workspace API
 * (callers pass `vscode.workspace.workspaceFile?.fsPath`). The helper
 * is split out so tests can exercise the classification logic without
 * requiring a real VS Code instance.
 */
export function resolveWorkspaceFilePath(workspaceFileFsPath: string | undefined): string | null {
  if (!workspaceFileFsPath) {
    return null;
  }
  // Untitled workspaces (in-memory, no on-disk file yet) carry an
  // `untitled:` URI scheme. VS Code surfaces those with no `fsPath`,
  // but we guard explicitly because the editWorkspaceClassification
  // command must refuse to scaffold untitled files.
  if (workspaceFileFsPath.startsWith("untitled:")) {
    return null;
  }
  return path.resolve(workspaceFileFsPath);
}

/**
 * Parse a `.code-workspace` file from disk.
 *
 * - Returns `null` when the file does not exist.
 * - Throws on JSON parse errors so the caller can surface a useful
 *   message; we deliberately don't swallow malformed input because
 *   silently treating it as empty would let the scaffold overwrite
 *   user content.
 */
export function readWorkspaceFile(workspaceFilePath: string): ParsedWorkspaceFile | null {
  let raw: string;
  try {
    raw = fs.readFileSync(workspaceFilePath, "utf-8");
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === "ENOENT") {
      return null;
    }
    throw err;
  }
  return parseWorkspaceFile(raw);
}

/**
 * Parse a raw `.code-workspace` JSON string.
 *
 * Tolerates trailing commas — `.code-workspace` is documented as
 * "JSON with comments" by VS Code, and many users hand-edit the file.
 */
export function parseWorkspaceFile(raw: string): ParsedWorkspaceFile {
  const stripped = stripJsonComments(raw);
  const parsed: unknown = JSON.parse(stripped);
  if (!parsed || typeof parsed !== "object") {
    return { folders: [], classification: null };
  }
  const obj = parsed as Record<string, unknown>;
  const folders = Array.isArray(obj.folders)
    ? obj.folders
        .filter((f): f is Record<string, unknown> => !!f && typeof f === "object")
        .map((f) => ({
          path: typeof f.path === "string" ? f.path : "",
          name: typeof f.name === "string" ? f.name : undefined,
        }))
        .filter((f) => f.path.length > 0)
    : [];
  const classificationRaw = obj["sqry.workspace"];
  const classification = parseClassification(classificationRaw);
  return { folders, classification };
}

/** Coerce an arbitrary JSON value into a `WorkspaceClassification`. */
function parseClassification(value: unknown): WorkspaceClassification | null {
  if (!value || typeof value !== "object") {
    return null;
  }
  const obj = value as Record<string, unknown>;
  const sourceRoots = parseStringArray(obj.sourceRoots);
  const exclusions = parseStringArray(obj.exclusions);
  const memberFolders = parseStringArray(obj.memberFolders);
  const projectRootMode = parseProjectRootMode(obj.projectRootMode);
  return {
    ...(sourceRoots !== undefined ? { sourceRoots } : {}),
    ...(exclusions !== undefined ? { exclusions } : {}),
    ...(memberFolders !== undefined ? { memberFolders } : {}),
    ...(projectRootMode !== undefined ? { projectRootMode } : {}),
  };
}

function parseStringArray(value: unknown): ReadonlyArray<string> | undefined {
  if (!Array.isArray(value)) {
    return undefined;
  }
  return value.filter((v): v is string => typeof v === "string");
}

function parseProjectRootMode(value: unknown): ProjectRootMode | undefined {
  if (value === "gitRoot" || value === "folder" || value === "explicit") {
    return value;
  }
  return undefined;
}

/**
 * Strip `// line` and `/* block *\/` comments from a JSON string.
 * Mirrors the behaviour VS Code applies when reading
 * `.code-workspace` files.
 */
export function stripJsonComments(input: string): string {
  let result = "";
  let i = 0;
  let inString: '"' | "'" | null = null;
  let inLineComment = false;
  let inBlockComment = false;
  while (i < input.length) {
    const c = input[i];
    const next = input[i + 1];
    if (inLineComment) {
      if (c === "\n") {
        inLineComment = false;
        result += c;
      }
      i += 1;
      continue;
    }
    if (inBlockComment) {
      if (c === "*" && next === "/") {
        inBlockComment = false;
        i += 2;
        continue;
      }
      i += 1;
      continue;
    }
    if (inString) {
      result += c;
      if (c === "\\" && i + 1 < input.length) {
        result += input[i + 1];
        i += 2;
        continue;
      }
      if (c === inString) {
        inString = null;
      }
      i += 1;
      continue;
    }
    if (c === '"' || c === "'") {
      inString = c as '"' | "'";
      result += c;
      i += 1;
      continue;
    }
    if (c === "/" && next === "/") {
      inLineComment = true;
      i += 2;
      continue;
    }
    if (c === "/" && next === "*") {
      inBlockComment = true;
      i += 2;
      continue;
    }
    result += c;
    i += 1;
  }
  return result;
}

/**
 * Build the JSON document the `sqry.editWorkspaceClassification`
 * command writes when the user invokes it.
 *
 * - Preserves the existing `folders` array verbatim.
 * - Adds `sqry.workspace` with the [`DEFAULT_CLASSIFICATION_SCAFFOLD`]
 *   shape only when the block is absent.
 * - Preserves any other top-level keys the user has already set
 *   (settings, launch, etc.) so the scaffold is non-destructive.
 *
 * Returns `{ content, alreadyHadBlock }`. When `alreadyHadBlock` is
 * `true` the caller should still open the file but skip the write.
 */
export function buildClassificationScaffold(rawWorkspaceFile: string | null): {
  readonly content: string;
  readonly alreadyHadBlock: boolean;
} {
  let parsed: Record<string, unknown>;
  if (rawWorkspaceFile === null || rawWorkspaceFile.trim().length === 0) {
    parsed = { folders: [] };
  } else {
    const stripped = stripJsonComments(rawWorkspaceFile);
    let candidate: unknown;
    try {
      candidate = JSON.parse(stripped);
    } catch {
      // Refuse to scaffold over malformed JSON — the caller surfaces
      // an error message instead. Returning the original content
      // unmodified preserves user data; the alreadyHadBlock flag is
      // forced to `true` so the caller does NOT overwrite the file.
      return { content: rawWorkspaceFile, alreadyHadBlock: true };
    }
    parsed =
      candidate && typeof candidate === "object"
        ? (candidate as Record<string, unknown>)
        : { folders: [] };
  }
  const alreadyHadBlock =
    parsed["sqry.workspace"] !== undefined && parsed["sqry.workspace"] !== null;
  if (!alreadyHadBlock) {
    parsed["sqry.workspace"] = {
      sourceRoots: [...(DEFAULT_CLASSIFICATION_SCAFFOLD.sourceRoots ?? [])],
      exclusions: [...(DEFAULT_CLASSIFICATION_SCAFFOLD.exclusions ?? [])],
      memberFolders: [...(DEFAULT_CLASSIFICATION_SCAFFOLD.memberFolders ?? [])],
      projectRootMode: DEFAULT_CLASSIFICATION_SCAFFOLD.projectRootMode,
    };
  }
  if (!Array.isArray(parsed.folders)) {
    parsed.folders = [];
  }
  return {
    content: JSON.stringify(parsed, null, 2) + "\n",
    alreadyHadBlock,
  };
}

/**
 * Test whether a folder path matches any of the user-configured
 * exclusion globs. Glob support is intentionally minimal: we honour
 * `*` (within a single segment), `**` (any number of segments), and
 * literal path matches. Anything else is matched as a literal
 * substring.
 */
export function isFolderExcluded(
  folderFsPath: string,
  excludes: ReadonlyArray<string>,
): boolean {
  if (!excludes.length) {
    return false;
  }
  const normalized = normalizePath(folderFsPath);
  for (const pattern of excludes) {
    if (matchesGlob(normalized, normalizePath(pattern))) {
      return true;
    }
  }
  return false;
}

function normalizePath(p: string): string {
  return p.replace(/\\/g, "/").replace(/\/+$/, "");
}

function matchesGlob(target: string, pattern: string): boolean {
  if (target === pattern) {
    return true;
  }
  if (target.endsWith(`/${pattern}`)) {
    return true;
  }
  // Exact basename matches the pattern.
  const basename = target.split("/").pop() ?? target;
  if (basename === pattern) {
    return true;
  }
  if (!pattern.includes("*")) {
    return target.includes(pattern);
  }
  const regex = globToRegex(pattern);
  if (regex.test(target) || regex.test(basename)) {
    return true;
  }
  // Apply the glob to every suffix of the path so patterns like
  // `docs/**` match `/repo/docs/api` (treat the leading workspace
  // segments as anonymous).
  const segments = target.split("/").filter((s) => s.length > 0);
  for (let i = 0; i < segments.length; i += 1) {
    const suffix = segments.slice(i).join("/");
    if (regex.test(suffix)) {
      return true;
    }
  }
  return false;
}

/**
 * Build the `initializationOptions.sqry.workspace` payload the
 * extension sends to the LSP at activation.
 *
 * STEP_5 codex iter1 MAJOR fix: the LSP wire contract is that
 * `initializationOptions.sqry.workspace` is the PARSED + CLASSIFIED
 * object (folders + sqry.workspace block), NOT a path string. The path
 * string is sent separately under `initializationOptions.sqry.workspaceFile`
 * so the LSP can fall back to in-process classification (branch 4 of
 * `resolve_logical_workspace`) when the parsed payload is the
 * extension-side classification hint.
 *
 * Returns `null` when the workspace file does not exist or the parser
 * returns a `null` (caller may decide whether to send a default empty
 * payload). The helper deliberately does not catch parse exceptions —
 * malformed JSON should bubble up so the activation path can log a
 * useful message and fall back gracefully.
 *
 * Pure helper: tests can import this without a VS Code extension host.
 */
export function buildWorkspaceInitializationPayload(
  workspaceFilePath: string,
): {
  readonly folders: ParsedWorkspaceFile["folders"];
  readonly classification: WorkspaceClassification | null;
} | null {
  const parsed = readWorkspaceFile(workspaceFilePath);
  if (!parsed) {
    return null;
  }
  return {
    folders: parsed.folders,
    classification: parsed.classification,
  };
}

function globToRegex(pattern: string): RegExp {
  // Escape regex specials, then translate `/**` -> `(?:/.*)?` so the
  // doublestar matches zero or more path segments, and `*` -> `[^/]*`.
  let r = "";
  let i = 0;
  while (i < pattern.length) {
    const c = pattern[i];
    if (c === "/" && pattern[i + 1] === "*" && pattern[i + 2] === "*") {
      r += "(?:/.*)?";
      i += 3;
      continue;
    }
    if (c === "*" && pattern[i + 1] === "*") {
      r += ".*";
      i += 2;
      continue;
    }
    if (c === "*") {
      r += "[^/]*";
      i += 1;
      continue;
    }
    if (c === "?") {
      r += "[^/]";
      i += 1;
      continue;
    }
    if (c === "." || c === "+" || c === "(" || c === ")" || c === "[" || c === "]" || c === "{" || c === "}" || c === "^" || c === "$" || c === "|" || c === "\\") {
      r += `\\${c}`;
      i += 1;
      continue;
    }
    r += c;
    i += 1;
  }
  return new RegExp(`^${r}$`);
}
