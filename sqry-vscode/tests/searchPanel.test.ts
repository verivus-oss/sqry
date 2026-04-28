import { expect } from "chai";

// eslint-disable-next-line @typescript-eslint/no-var-requires
const proxyquire = require("proxyquire").noCallThru();

class ThemeIcon {
  constructor(public id: string) {}
}

class TreeItem {
  public description?: string;
  public iconPath?: ThemeIcon;
  public contextValue?: string;
  public tooltip?: string;
  public command?: unknown;

  constructor(
    public label: string,
    public collapsibleState: number,
  ) {}
}

class EventEmitter<T> {
  public event = (_listener: (value: T) => void) => ({ dispose: () => {} });
  public fire(_value?: T): void {}
  public dispose(): void {}
}

const TreeItemCollapsibleState = {
  None: 0,
  Collapsed: 1,
  Expanded: 2,
};

const vscodeStub = {
  __esModule: true,
  ThemeIcon,
  TreeItem,
  TreeItemCollapsibleState,
  EventEmitter,
  workspace: {
    workspaceFolders: undefined,
    getWorkspaceFolder: () => undefined,
  },
  window: {
    createTreeView: () => ({
      dispose: () => {},
    }),
    showInformationMessage: () => Promise.resolve(),
    showErrorMessage: () => Promise.resolve(),
  },
  Uri: {
    file: (filePath: string) => ({ fsPath: filePath, scheme: "file" }),
    parse: (value: string) => ({ fsPath: value, scheme: "file", toString: () => value }),
  },
  Range: class Range {
    constructor(
      public startLine: number,
      public startChar: number,
      public endLine: number,
      public endChar: number,
    ) {}
  },
};

const { SearchPanel } = proxyquire("../src/searchPanel", {
  vscode: vscodeStub,
  "node:path": require("node:path"),
}) as { SearchPanel: typeof import("../src/searchPanel").SearchPanel };

function makePanel(): any {
  const context = { subscriptions: { push: () => {} } };
  const panel = new SearchPanel(context as any, null, null) as any;
  panel.setIndexStatus({
    exists: true,
    symbol_count: 1,
    file_count: 1,
    supports_fuzzy: true,
    supports_relations: true,
    languages: ["rust"],
  } as any);
  return panel;
}

function makeEmptyPanel(): any {
  const context = { subscriptions: { push: () => {} } };
  return new SearchPanel(context as any, null, null) as any;
}

function makeUnusedSymbol(name: string): any {
  return {
    name,
    kind: "function",
    language: "rust",
    location: {
      uri: "file:///workspace/src/lib.rs",
      range: {
        start: { line: 0, character: 0 },
        end: { line: 0, character: 1 },
      },
    },
  };
}

describe("searchPanel labels", () => {
  it("renders SqryCircularItem labels from displayed count when truncated", async () => {
    const panel = makePanel();
    const provider = panel.treeDataProvider as any;
    provider.cachedCircularResult.set("", {
      cycles: [
        { cycle_id: "1", depth: 2, members: ["a", "b"], cycle_type: "calls" },
        { cycle_id: "2", depth: 2, members: ["c", "d"], cycle_type: "calls" },
        { cycle_id: "3", depth: 2, members: ["e", "f"], cycle_type: "calls" },
      ],
      total_cycles: 10,
      truncated: true,
    });

    const rootItems = await Promise.resolve(provider.getChildren());
    const circularItem = rootItems.find((item: any) => item.contextValue === "sqry.circular");
    expect(circularItem.description).to.equal("3+ cycles");
  });

  it("renders SqryCircularItem labels as exact totals when not truncated", async () => {
    const panel = makePanel();
    const provider = panel.treeDataProvider as any;
    provider.cachedCircularResult.set("", {
      cycles: [
        { cycle_id: "1", depth: 2, members: ["a", "b"], cycle_type: "calls" },
        { cycle_id: "2", depth: 2, members: ["c", "d"], cycle_type: "calls" },
        { cycle_id: "3", depth: 2, members: ["e", "f"], cycle_type: "calls" },
      ],
      total_cycles: 3,
      truncated: false,
    });

    const rootItems = await Promise.resolve(provider.getChildren());
    const circularItem = rootItems.find((item: any) => item.contextValue === "sqry.circular");
    expect(circularItem.description).to.equal("3 cycles");
  });

  it("renders SqryUnusedItem labels as exact totals even when truncated", async () => {
    const panel = makePanel();
    const provider = panel.treeDataProvider as any;
    provider.cachedUnusedResult.set("", {
      symbols: [makeUnusedSymbol("one"), makeUnusedSymbol("two")],
      total: 500,
      truncated: true,
      scope: "all",
    });

    const rootItems = await Promise.resolve(provider.getChildren());
    const unusedItem = rootItems.find((item: any) => item.contextValue === "sqry.unused");
    expect(unusedItem.description).to.equal("500 symbols");
  });

  it("renders the unused truncation row with shown and total counts", async () => {
    const panel = makePanel();
    const provider = panel.treeDataProvider as any;
    provider.cachedUnusedResult.set("", {
      symbols: [makeUnusedSymbol("one"), makeUnusedSymbol("two")],
      total: 500,
      truncated: true,
      scope: "all",
    });

    const rootItems = await Promise.resolve(provider.getChildren());
    const unusedItem = rootItems.find((item: any) => item.contextValue === "sqry.unused");
    const children = await Promise.resolve(provider.getChildren(unusedItem));
    expect(children[children.length - 1].label).to.equal(
      "Showing 2 of 500 (results truncated)",
    );
  });

  it("omits the unused truncation row when results are exact", async () => {
    const panel = makePanel();
    const provider = panel.treeDataProvider as any;
    provider.cachedUnusedResult.set("", {
      symbols: [
        makeUnusedSymbol("one"),
        makeUnusedSymbol("two"),
        makeUnusedSymbol("three"),
      ],
      total: 3,
      truncated: false,
      scope: "all",
    });

    const rootItems = await Promise.resolve(provider.getChildren());
    const unusedItem = rootItems.find((item: any) => item.contextValue === "sqry.unused");
    expect(unusedItem.description).to.equal("3 symbols");
    const children = await Promise.resolve(provider.getChildren(unusedItem));
    expect(children.map((item: any) => item.label)).to.not.include(
      "Showing 3 of 3 (results truncated)",
    );
  });

  it("renders aggregate workspace status instead of falling through to the welcome view", async () => {
    const panel = makeEmptyPanel();
    const provider = panel.treeDataProvider as any;

    panel.setWorkspaceStatus({
      source_root_statuses: [
        {
          path: "/workspace/sqry",
          status: "ok",
          symbol_count: 42,
        },
      ],
      missing_count: 0,
      building_count: 0,
      ok_count: 1,
      error_count: 0,
      generated_at: "2026-04-28T00:00:00Z",
    });

    const rootItems = await Promise.resolve(provider.getChildren());
    expect(rootItems.map((item: any) => item.label)).to.include("Symbols");
    expect(rootItems[0].description).to.equal("42");
  });

  it("keeps rich single-folder index status ahead of aggregate fallback", async () => {
    const panel = makeEmptyPanel();
    const provider = panel.treeDataProvider as any;

    panel.setWorkspaceStatus({
      source_root_statuses: [
        { path: "/workspace/sqry", status: "ok", symbol_count: 42 },
      ],
      missing_count: 0,
      building_count: 0,
      ok_count: 1,
      error_count: 0,
      generated_at: "2026-04-28T00:00:00Z",
    });
    panel.hydrateIndexStatus({
      exists: true,
      symbol_count: 42,
      file_count: 7,
      languages: ["rust", "ts"],
      supports_fuzzy: true,
      supports_relations: true,
    });

    const rootItems = await Promise.resolve(provider.getChildren());
    expect(rootItems.map((item: any) => item.label)).to.include.members([
      "Symbols",
      "Files",
      "Languages",
    ]);
  });

  it("falls back to aggregate rendering when hydrated status is aggregate-only", async () => {
    const panel = makeEmptyPanel();
    const provider = panel.treeDataProvider as any;

    const aggregate = {
      source_root_statuses: [
        { path: "/workspace/sqry", status: "ok", symbol_count: 42 },
      ],
      missing_count: 0,
      building_count: 0,
      ok_count: 1,
      error_count: 0,
      generated_at: "2026-04-28T00:00:00Z",
    };
    panel.setWorkspaceStatus(aggregate);
    panel.hydrateIndexStatus({
      exists: true,
      supports_fuzzy: true,
      supports_relations: true,
      aggregate,
    });

    const rootItems = await Promise.resolve(provider.getChildren());
    expect(rootItems.map((item: any) => item.label)).to.include("Symbols");
    expect(rootItems[0].description).to.equal("42");
  });

  it("renders multiple source roots from aggregate workspace status", async () => {
    const previousFolders = vscodeStub.workspace.workspaceFolders;
    vscodeStub.workspace.workspaceFolders = [
      { uri: { fsPath: "/workspace/repo-a" }, name: "repo-a" },
      { uri: { fsPath: "/workspace/repo-b" }, name: "repo-b" },
    ] as any;
    try {
      const panel = makeEmptyPanel();
      const provider = panel.treeDataProvider as any;

      panel.setWorkspaceStatus({
        source_root_statuses: [
          { path: "/workspace/repo-a", status: "ok", symbol_count: 11 },
          { path: "/workspace/repo-b", status: "ok", symbol_count: 13 },
        ],
        missing_count: 0,
        building_count: 0,
        ok_count: 2,
        error_count: 0,
        generated_at: "2026-04-28T00:00:00Z",
      });

      const rootItems = await Promise.resolve(provider.getChildren());
      expect(rootItems).to.have.length(2);
      expect(rootItems.map((item: any) => item.contextValue)).to.deep.equal([
        "sqry.workspaceRoot",
        "sqry.workspaceRoot",
      ]);
      expect(rootItems.map((item: any) => item.label)).to.deep.equal(["repo-a", "repo-b"]);
      expect(rootItems.map((item: any) => item.rootPath)).to.deep.equal([
        "/workspace/repo-a",
        "/workspace/repo-b",
      ]);
      expect(rootItems.map((item: any) => item.description)).to.deep.equal([
        "11 symbols, 0 files",
        "13 symbols, 0 files",
      ]);
    } finally {
      vscodeStub.workspace.workspaceFolders = previousFolders;
    }
  });

  it("renders missing and error source-root statuses explicitly", async () => {
    const panel = makeEmptyPanel();
    const provider = panel.treeDataProvider as any;

    panel.setWorkspaceStatus({
      source_root_statuses: [
        { path: "/workspace/missing", status: "missing" },
      ],
      missing_count: 1,
      building_count: 0,
      ok_count: 0,
      error_count: 0,
      generated_at: "2026-04-28T00:00:00Z",
    });
    let rootItems = await Promise.resolve(provider.getChildren());
    expect(rootItems[0].label).to.equal("Status");
    expect(rootItems[0].description).to.equal("not indexed");

    panel.setWorkspaceStatus({
      source_root_statuses: [
        { path: "/workspace/error", status: "error" },
      ],
      missing_count: 0,
      building_count: 0,
      ok_count: 0,
      error_count: 1,
      generated_at: "2026-04-28T00:00:00Z",
    });
    rootItems = await Promise.resolve(provider.getChildren());
    expect(rootItems[0].label).to.equal("Status");
    expect(rootItems[0].description).to.equal("index error");
  });
});
