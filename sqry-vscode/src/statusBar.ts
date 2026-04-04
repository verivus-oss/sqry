import * as vscode from "vscode";
import { SqryIndexStatus } from "./lspProtocol";

const STALE_THRESHOLD_SECONDS = 86400; // 24 hours

export type IndexState = "ready" | "stale" | "building" | "noIndex" | "error";

export class SqryStatusBar implements vscode.Disposable {
  constructor(
    private readonly item: vscode.StatusBarItem,
    private readonly outputChannel: vscode.OutputChannel | null,
  ) {
    this.item.show();
    this.update(null);
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
   * Update status bar for multi-root workspaces.
   * Shows the worst state across all roots.
   * Priority: noIndex > stale > building > ready
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

  private worseState(a: IndexState, b: IndexState): IndexState {
    const priority: Record<IndexState, number> = {
      ready: 0,
      building: 1,
      stale: 2,
      error: 3,
      noIndex: 4,
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

  private setState(state: IndexState, status?: SqryIndexStatus): void {
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
    }
  }

  private buildTooltip(status?: SqryIndexStatus): string {
    if (!status) return "sqry";
    const parts = ["sqry"];
    if (status.symbol_count !== undefined) parts.push(`${status.symbol_count} symbols`);
    if (status.file_count !== undefined) parts.push(`${status.file_count} files`);
    if (status.age_seconds !== undefined) parts.push(`indexed ${this.formatAge(status.age_seconds)}`);
    return parts.join(" | ");
  }

  private formatAge(seconds: number): string {
    if (seconds < 60) return "just now";
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
