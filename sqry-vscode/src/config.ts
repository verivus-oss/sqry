import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import * as vscode from "vscode";
import which from "which";

export interface SqrySettings {
  readonly sqryPath: string;
  readonly limit: number;
  readonly timeoutMs: number;
  readonly indexTimeoutMs: number;
  readonly autoIndexOnOpen: "always" | "prompt" | "never";
  readonly codeLensEnabled: boolean;
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
  };
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
