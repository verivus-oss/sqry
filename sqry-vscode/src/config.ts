import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import * as vscode from "vscode";
import which from "which";
import {
  isFolderExcluded,
  ProjectRootMode,
  WorkspaceClassification,
} from "./workspaceClassifier";

export interface SqrySettings {
  readonly sqryPath: string;
  readonly limit: number;
  readonly timeoutMs: number;
  readonly indexTimeoutMs: number;
  readonly autoIndexOnOpen: "always" | "prompt" | "never";
  readonly codeLensEnabled: boolean;
  /**
   * Optional canonical "index root" override. When set, sqry treats
   * this path as the workspace's logical root regardless of which
   * folder VS Code surfaced first. Empty string means "auto-detect".
   */
  readonly indexRoot: string;
  /** How sqry decides what counts as a project root inside a folder. */
  readonly projectRootMode: ProjectRootMode;
  /**
   * Glob patterns matched against `vscode.WorkspaceFolder.uri.fsPath`.
   * Folders matching any glob are excluded from every enumeration loop
   * (auto-index, status fan-out, manual rebuild) so users can keep
   * non-source folders (e.g. `docs`, `examples`) inside the workspace
   * without sqry trying to index them.
   */
  readonly workspaceFolderExcludes: ReadonlyArray<string>;
  /**
   * Inline classification block, parallel to the `sqry.workspace` entry
   * in a `.code-workspace` file. The settings.json copy is the
   * fallback used when no `.code-workspace` is open; the
   * `.code-workspace` block always wins when both are present.
   */
  readonly workspaceClassification: WorkspaceClassification | null;
}

export interface ResolvedSqryConfig extends SqrySettings {
  readonly resolvedBinaryPath: string;
}

/** How the binary was found — logged to the output channel for debuggability. */
export type ResolutionSource = "path" | "probe" | "fallback";

const SECTION = "sqry";

/** Default bare command name used when the user has not explicitly configured sqry.path. */
const DEFAULT_BINARY = "sqry";

export function readSettings(): SqrySettings {
  const config = vscode.workspace.getConfiguration(SECTION);
  const projectRootModeRaw = config.get<string>("projectRootMode", "gitRoot");
  const projectRootMode: ProjectRootMode =
    projectRootModeRaw === "folder" || projectRootModeRaw === "explicit"
      ? projectRootModeRaw
      : "gitRoot";
  const excludesRaw = config.get<unknown>("workspaceFolderExcludes", []);
  const workspaceFolderExcludes = Array.isArray(excludesRaw)
    ? excludesRaw.filter((v): v is string => typeof v === "string")
    : [];
  const classificationRaw = config.get<unknown>("workspaceClassification", null);
  const workspaceClassification = normalizeClassification(classificationRaw);
  return {
    sqryPath: config.get<string>("path", DEFAULT_BINARY),
    limit: config.get<number>("limit", 200),
    timeoutMs: config.get<number>("timeoutMs", 15_000),
    indexTimeoutMs: config.get<number>("indexTimeoutMs", 300_000),
    autoIndexOnOpen: config.get<"always" | "prompt" | "never">(
      "autoIndexOnOpen",
      "prompt",
    ),
    codeLensEnabled: config.get<boolean>("codeLens.enabled", true),
    indexRoot: config.get<string>("indexRoot", ""),
    projectRootMode,
    workspaceFolderExcludes,
    workspaceClassification,
  };
}

/**
 * Coerce the loosely-typed `sqry.workspaceClassification` setting into
 * a strongly-typed `WorkspaceClassification`. Returns `null` for
 * absent / unparseable values.
 */
function normalizeClassification(value: unknown): WorkspaceClassification | null {
  if (!value || typeof value !== "object") {
    return null;
  }
  const obj = value as Record<string, unknown>;
  const sourceRoots = Array.isArray(obj.sourceRoots)
    ? obj.sourceRoots.filter((v): v is string => typeof v === "string")
    : undefined;
  const exclusions = Array.isArray(obj.exclusions)
    ? obj.exclusions.filter((v): v is string => typeof v === "string")
    : undefined;
  const memberFolders = Array.isArray(obj.memberFolders)
    ? obj.memberFolders.filter((v): v is string => typeof v === "string")
    : undefined;
  const projectRootMode =
    obj.projectRootMode === "gitRoot" ||
    obj.projectRootMode === "folder" ||
    obj.projectRootMode === "explicit"
      ? (obj.projectRootMode as ProjectRootMode)
      : undefined;
  return {
    ...(sourceRoots !== undefined ? { sourceRoots } : {}),
    ...(exclusions !== undefined ? { exclusions } : {}),
    ...(memberFolders !== undefined ? { memberFolders } : {}),
    ...(projectRootMode !== undefined ? { projectRootMode } : {}),
  };
}

/**
 * Test whether a workspace folder is excluded by the user's
 * `sqry.workspaceFolderExcludes` setting. Every enumeration loop in
 * the extension consumes this (per DAG STEP_5 acceptance criterion 8).
 */
export function isWorkspaceFolderExcluded(
  folder: vscode.WorkspaceFolder,
  settings: SqrySettings = readSettings(),
): boolean {
  return isFolderExcluded(folder.uri.fsPath, settings.workspaceFolderExcludes);
}

/**
 * Filter a list of workspace folders down to those NOT excluded by
 * `sqry.workspaceFolderExcludes`. Convenience wrapper for call sites
 * that iterate `vscode.workspace.workspaceFolders`.
 */
export function nonExcludedFolders(
  folders: ReadonlyArray<vscode.WorkspaceFolder>,
  settings: SqrySettings = readSettings(),
): vscode.WorkspaceFolder[] {
  return folders.filter((f) => !isWorkspaceFolderExcluded(f, settings));
}

export async function resolveConfig(fallbackBinaryPath?: string): Promise<ResolvedSqryConfig> {
  const settings = readSettings();
  const resolvedBinaryPath = await resolveBinary(settings.sqryPath, fallbackBinaryPath);
  return {
    ...settings,
    resolvedBinaryPath,
  };
}

/**
 * Resolve the sqry binary path using a multi-step cascade:
 *
 * 1. If sqry.path is an explicit path (not the bare default), validate only that path.
 * 2. If sqry.path is the default bare command, try `which()` on PATH.
 * 3. If `which()` fails, probe common install locations (platform-aware).
 * 4. Fall back to a previously auto-downloaded binary.
 * 5. Throw so the caller can show the download prompt.
 */
export async function resolveBinary(binary: string, fallbackPath?: string): Promise<string> {
  const expanded = expandTilde(binary.trim());
  if (!expanded) {
    throw new Error("sqry path is empty. Set `sqry.path` in settings.");
  }

  // Step 1: Try which() — works for both explicit paths and bare command names.
  try {
    return await which(expanded);
  } catch {
    // which() failed — continue to next steps
  }

  // Step 2: Only probe common locations when using the default bare command.
  // When the user explicitly configured a path, don't second-guess it.
  if (isDefaultBinaryName(expanded)) {
    const probed = await probeCommonLocations();
    if (probed) {
      return probed;
    }
  }

  // Step 3: Try fallback path (e.g., auto-downloaded binary).
  if (fallbackPath) {
    if (await isExecutableFile(fallbackPath)) {
      return fallbackPath;
    }
  }

  // Step 4: Nothing found — throw.
  throw new Error(
    `Unable to locate sqry binary at "${binary}". Update \`sqry.path\` to a valid executable.`,
  );
}

/**
 * Check whether the configured binary name is the default bare command
 * (i.e. the user hasn't explicitly set sqry.path to a custom value).
 * A bare command has no directory separators — it's just "sqry" or "sqry.exe".
 */
function isDefaultBinaryName(expanded: string): boolean {
  return expanded === "sqry" || expanded === "sqry.exe";
}

/**
 * Build the platform-aware list of common install locations to probe.
 */
export function getCommonBinaryPaths(): string[] {
  const isWindows = process.platform === "win32";
  const home = os.homedir();

  if (isWindows) {
    const userProfile = process.env.USERPROFILE || home;
    const localAppData = process.env.LOCALAPPDATA || path.join(userProfile, "AppData", "Local");
    const cargoHome = process.env.CARGO_HOME || path.join(userProfile, ".cargo");
    return [
      path.join(cargoHome, "bin", "sqry.exe"),
      path.join(localAppData, "sqry", "sqry.exe"),
    ];
  }

  // POSIX (Linux, macOS)
  const cargoHome = process.env.CARGO_HOME || path.join(home, ".cargo");
  const candidates = [
    path.join(cargoHome, "bin", "sqry"),      // $CARGO_HOME/bin (or ~/.cargo/bin)
    path.join(home, ".local", "bin", "sqry"),  // ~/.local/bin (install.sh target)
    "/usr/local/bin/sqry",                     // system-wide install
  ];

  // Homebrew on Apple Silicon
  if (process.platform === "darwin") {
    candidates.push("/opt/homebrew/bin/sqry");
  }

  return candidates;
}

/**
 * Probe common install locations and return the first executable match.
 */
async function probeCommonLocations(): Promise<string | undefined> {
  for (const candidate of getCommonBinaryPaths()) {
    if (await isExecutableFile(candidate)) {
      return candidate;
    }
  }
  return undefined;
}

/**
 * Check that a path is a regular file with execute permission.
 * On Windows, checks existence and .exe suffix instead of POSIX X_OK.
 */
async function isExecutableFile(filePath: string): Promise<boolean> {
  try {
    const stat = await fs.promises.stat(filePath);
    if (!stat.isFile()) {
      return false;
    }
    if (process.platform === "win32") {
      return filePath.toLowerCase().endsWith(".exe");
    }
    await fs.promises.access(filePath, fs.constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

export function expandTilde(input: string): string {
  // Only expand ~/... (current user's home), not ~otheruser/...
  if (!input.startsWith("~/") && input !== "~") {
    return input;
  }

  const home = process.env.HOME || process.env.USERPROFILE;
  if (!home) {
    return input;
  }
  return path.join(home, input.slice(1));
}
