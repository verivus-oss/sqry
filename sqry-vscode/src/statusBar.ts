import * as vscode from "vscode";
import { LoadingPhase } from "./loadingState";
import { SqryIndexStatus, SqryWorkspaceStatus } from "./lspProtocol";

const STALE_THRESHOLD_SECONDS = 86400; // 24 hours

export type IndexState =
  | "ready"
  | "stale"
  | "building"
  | "noIndex"
  | "error"
  | "resolving"
  | "unavailable";

/**
 * Localised string keys consumed by the status bar. The DAG specifies
 * `sqry.statusBar.resolving`; we co-host `sqry.statusBar.unavailable`
 * for the terminal `Failed` phase since both are surfaced via the
 * same code path.
 *
 * VS Code's `l10n.t()` API picks up `package.nls.json` automatically.
 * The fallback string mirrors the English entry so unit tests (which
 * stub `l10n`) see deterministic text.
 */
export const STATUS_BAR_LOCALE_KEYS = {
  resolving: "sqry.statusBar.resolving",
  unavailable: "sqry.statusBar.unavailable",
} as const;

export const STATUS_BAR_LOCALE_FALLBACKS: Record<
  (typeof STATUS_BAR_LOCALE_KEYS)[keyof typeof STATUS_BAR_LOCALE_KEYS],
  string
> = {
  "sqry.statusBar.resolving": "sqry: resolving workspace…",
  "sqry.statusBar.unavailable": "sqry: unavailable",
};

/** Resolve a locale key with the package.nls fallback. */
function localized(key: keyof typeof STATUS_BAR_LOCALE_FALLBACKS): string {
  // `vscode.l10n` is undefined in unit tests; the fallback table is
  // the source of truth and matches `package.nls.json`.
  const l10n = (vscode as unknown as { l10n?: { t(key: string): string } }).l10n;
  if (l10n && typeof l10n.t === "function") {
    const translated = l10n.t(key);
    if (translated && translated !== key) {
      return translated;
    }
  }
  return STATUS_BAR_LOCALE_FALLBACKS[key];
}

export class SqryStatusBar implements vscode.Disposable {
  constructor(
    private readonly item: vscode.StatusBarItem,
    private readonly outputChannel: vscode.OutputChannel | null,
  ) {
    this.item.show();
    // STEP_5 contract: the extension boots in `Activating`, so the
    // initial status-bar render shows the resolving locale string —
    // never "no index" (which would cause user-visible flicker).
    this.setLoadingPhase("Activating");
  }

  /**
   * Render the loading-state phase. Activating / LspStarting /
   * WorkspaceResolving collapse to the same `resolving` locale string
   * (DAG STEP_5 acceptance criterion 2). `Ready` is a no-op here —
   * callers must follow up with `update()` / `updateMultiRoot()`.
   * `Failed` flips the bar to `unavailable` with the View Logs action.
   */
  public setLoadingPhase(phase: LoadingPhase, failureReason?: string): void {
    if (phase === "Activating" || phase === "LspStarting" || phase === "WorkspaceResolving") {
      this.setState("resolving");
      return;
    }
    if (phase === "Failed") {
      this.setState("unavailable", undefined, failureReason);
      return;
    }
    // `Ready` — no-op, caller renders the actual aggregate status.
  }

  public update(status: SqryIndexStatus | null): void {
    if (status?.symbol_count === undefined) {
      this.setState("noIndex");
      return;
    }
    if (status.age_seconds !== undefined && status.age_seconds > STALE_THRESHOLD_SECONDS) {
      this.setState("stale", status);
      return;
    }
    this.setState("ready", status);
  }

  public setBuilding(): void {
    this.setState("building");
  }

  /**
   * Render the aggregate `WorkspaceStatus` returned by
   * `getWorkspaceStatus()`. The DAG mandates a single aggregate
   * surface — callers no longer fan-out per-folder probes here.
   */
  public updateWorkspace(status: SqryWorkspaceStatus): void {
    if (status.source_root_statuses.length === 0) {
      this.setState("noIndex");
      return;
    }
    let worst: IndexState = "ready";
    const summaries: string[] = [];
    for (const entry of status.source_root_statuses) {
      const phase = this.classifySourceRoot(entry.status);
      worst = this.worseState(worst, phase);
      const sym = entry.symbol_count ?? 0;
      summaries.push(`${entry.path}: ${phase} (${sym} symbols)`);
    }
    this.setState(worst);
    this.item.tooltip = `sqry (${status.source_root_statuses.length} source roots)\n${summaries.join("\n")}`;
  }

  /**
   * Update status bar for multi-root workspaces.
   * Shows the worst state across all roots.
   * Priority: noIndex > stale > building > ready
   *
   * @deprecated retained for the existing unit-test surface; new
   * code paths route through `updateWorkspace()` so the aggregate
   * surface is the only consumer.
   */
  public updateMultiRoot(statuses: Map<string, SqryIndexStatus>): void {
    if (statuses.size === 0) {
      this.setState("noIndex");
      return;
    }

    let worstState: IndexState = "ready";
    const rootSummaries: string[] = [];

    for (const [rootName, status] of statuses) {
      const state = this.classifyStatus(status);
      worstState = this.worseState(worstState, state);
      const symbolCount = status.symbol_count ?? 0;
      const fileCount = status.file_count ?? 0;
      rootSummaries.push(`${rootName}: ${state} (${symbolCount} symbols, ${fileCount} files)`);
    }

    // Use worst state but build a combined tooltip
    this.setState(worstState);
    this.item.tooltip = `sqry (${statuses.size} roots)\n${rootSummaries.join("\n")}`;
  }

  private classifyStatus(status: SqryIndexStatus): IndexState {
    if (status?.symbol_count === undefined) {
      return "noIndex";
    }
    if (status.building) {
      return "building";
    }
    if (status.age_seconds !== undefined && status.age_seconds > STALE_THRESHOLD_SECONDS) {
      return "stale";
    }
    return "ready";
  }

  private classifySourceRoot(state: "ok" | "missing" | "building" | "error"): IndexState {
    switch (state) {
      case "ok":
        return "ready";
      case "missing":
        return "noIndex";
      case "building":
        return "building";
      case "error":
        return "error";
      default:
        return "noIndex";
    }
  }

  private worseState(a: IndexState, b: IndexState): IndexState {
    const priority: Record<IndexState, number> = {
      ready: 0,
      building: 1,
      stale: 2,
      error: 3,
      noIndex: 4,
      resolving: 5,
      unavailable: 6,
    };
    return priority[a] >= priority[b] ? a : b;
  }

  public setError(message: string): void {
    this.item.text = "$(error) sqry: Error";
    this.item.tooltip = `sqry error: ${message}`;
    this.item.command = "sqry.showOutput";
    this.item.backgroundColor = new vscode.ThemeColor("statusBarItem.errorBackground");
  }

  public dispose(): void {
    this.item.dispose();
  }

  private setState(state: IndexState, status?: SqryIndexStatus, reason?: string): void {
    switch (state) {
      case "ready":
        this.item.text = "$(database) sqry: Ready";
        this.item.tooltip = this.buildTooltip(status);
        this.item.command = "sqry.refreshStats";
        this.item.backgroundColor = undefined;
        break;
      case "stale":
        this.item.text = "$(warning) sqry: Stale";
        this.item.tooltip = this.buildTooltip(status) + "\nIndex is older than 24 hours";
        this.item.command = "sqry.index";
        this.item.backgroundColor = new vscode.ThemeColor("statusBarItem.warningBackground");
        break;
      case "building":
        this.item.text = "$(sync~spin) sqry: Indexing...";
        this.item.tooltip = "Building sqry index...";
        this.item.command = "sqry.showOutput";
        this.item.backgroundColor = undefined;
        break;
      case "noIndex":
        this.item.text = "$(error) sqry: No Index";
        this.item.tooltip = "No sqry index found. Click to build one.";
        this.item.command = "sqry.index";
        this.item.backgroundColor = new vscode.ThemeColor("statusBarItem.errorBackground");
        break;
      case "error":
        this.item.text = "$(error) sqry: Error";
        this.item.tooltip = "sqry: source root reported an error";
        this.item.command = "sqry.showOutput";
        this.item.backgroundColor = new vscode.ThemeColor("statusBarItem.errorBackground");
        break;
      case "resolving":
        this.item.text = `$(sync~spin) ${localized("sqry.statusBar.resolving")}`;
        this.item.tooltip = localized("sqry.statusBar.resolving");
        this.item.command = "sqry.showOutput";
        this.item.backgroundColor = undefined;
        break;
      case "unavailable":
        this.item.text = `$(error) ${localized("sqry.statusBar.unavailable")}`;
        this.item.tooltip = reason
          ? `${localized("sqry.statusBar.unavailable")}\n${reason}`
          : localized("sqry.statusBar.unavailable");
        // `View Logs` action — wired via the status-bar command.
        this.item.command = "sqry.showOutput";
        this.item.backgroundColor = new vscode.ThemeColor("statusBarItem.errorBackground");
        break;
    }
  }

  private buildTooltip(status?: SqryIndexStatus): string {
    if (!status) {
      return "sqry";
    }
    const parts = ["sqry"];
    if (status.symbol_count !== undefined) {
      parts.push(`${status.symbol_count} symbols`);
    }
    if (status.file_count !== undefined) {
      parts.push(`${status.file_count} files`);
    }
    if (status.age_seconds !== undefined) {
      parts.push(`indexed ${this.formatAge(status.age_seconds)}`);
    }
    return parts.join(" | ");
  }

  private formatAge(seconds: number): string {
    if (seconds < 60) {
      return "just now";
    }
    if (seconds < 3600) {
      const m = Math.floor(seconds / 60);
      return `${m}m ago`;
    }
    if (seconds < 86400) {
      const h = Math.floor(seconds / 3600);
      return `${h}h ago`;
    }
    const d = Math.floor(seconds / 86400);
    return `${d}d ago`;
  }
}
