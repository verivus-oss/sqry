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

const SECTION = "sqry";

export function readSettings(): SqrySettings {
  const config = vscode.workspace.getConfiguration(SECTION);
  return {
    sqryPath: config.get<string>("path", "sqry"),
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

export async function resolveConfig(): Promise<ResolvedSqryConfig> {
  const settings = readSettings();
  const resolvedBinaryPath = await resolveBinary(settings.sqryPath);
  return {
    ...settings,
    resolvedBinaryPath,
  };
}

export async function resolveBinary(binary: string): Promise<string> {
  const expanded = expandTilde(binary.trim());
  if (!expanded) {
    throw new Error("sqry path is empty. Set `sqry.path` in settings.");
  }

  try {
    return await which(expanded);
  } catch {
    throw new Error(
      `Unable to locate sqry binary at "${binary}". Update \`sqry.path\` to a valid executable.`,
    );
  }
}

export function expandTilde(input: string): string {
  if (!input.startsWith("~")) {
    return input;
  }

  const home = process.env.HOME || process.env.USERPROFILE;
  if (!home) {
    return input;
  }
  return path.join(home, input.slice(1));
}
