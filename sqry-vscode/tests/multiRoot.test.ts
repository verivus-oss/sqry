import { expect } from "chai";
import * as nodePath from "node:path";
import proxyquireModule from "proxyquire";

const proxyquire = proxyquireModule.noCallThru();

// ===== vscode stubs =====

class ThemeColor {
  constructor(public id: string) {}
}

class ThemeIcon {
  constructor(public id: string) {}
}

class TreeItem {
  public label?: string;
  public description?: string;
  public iconPath?: ThemeIcon;
  public collapsibleState?: number;
  public contextValue?: string;
  public command?: unknown;
  public tooltip?: string;
  public backgroundColor?: ThemeColor;

  constructor(label: string, collapsibleState?: number) {
    this.label = label;
    this.collapsibleState = collapsibleState;
  }
}

const TreeItemCollapsibleState = {
  None: 0,
  Collapsed: 1,
  Expanded: 2,
};

// ===== Status Bar Tests =====

const statusBarVscodeStub = {
  __esModule: true,
  ThemeColor,
};

const { SqryStatusBar } = proxyquire("../src/statusBar", {
  vscode: statusBarVscodeStub,
}) as { SqryStatusBar: typeof import("../src/statusBar").SqryStatusBar };

function createMockStatusBarItem() {
  return {
    text: "",
    tooltip: undefined as string | undefined,
    command: undefined as string | undefined,
    backgroundColor: undefined as unknown,
    show: () => {},
    hide: () => {},
    dispose: () => {},
  };
}

describe("Multi-Root Workspace Support", () => {
  // ===== Status Bar: updateMultiRoot =====

  describe("SqryStatusBar.updateMultiRoot", () => {
    it("shows noIndex when statuses map is empty", () => {
      const item = createMockStatusBarItem();
      const bar = new SqryStatusBar(item as any, null as any);
      bar.updateMultiRoot(new Map());
      expect(item.text).to.equal("$(error) sqry: No Index");
    });

    it("shows ready when all roots are ready", () => {
      const item = createMockStatusBarItem();
      const bar = new SqryStatusBar(item as any, null as any);
      const statuses = new Map<string, any>([
        ["/root/a", { symbol_count: 100, file_count: 10, age_seconds: 60 }],
        ["/root/b", { symbol_count: 200, file_count: 20, age_seconds: 120 }],
      ]);
      bar.updateMultiRoot(statuses);
      expect(item.text).to.equal("$(database) sqry: Ready");
    });

    it("shows stale when one root is ready and one is stale", () => {
      const item = createMockStatusBarItem();
      const bar = new SqryStatusBar(item as any, null as any);
      const statuses = new Map<string, any>([
        ["/root/a", { symbol_count: 100, file_count: 10, age_seconds: 60 }],
        ["/root/b", { symbol_count: 200, file_count: 20, age_seconds: 100000 }],
      ]);
      bar.updateMultiRoot(statuses);
      expect(item.text).to.equal("$(warning) sqry: Stale");
    });

    it("shows noIndex when any root has no index (worst state)", () => {
      const item = createMockStatusBarItem();
      const bar = new SqryStatusBar(item as any, null as any);
      const statuses = new Map<string, any>([
        ["/root/a", { symbol_count: 100, file_count: 10, age_seconds: 60 }],
        ["/root/b", { symbol_count: undefined }],
      ]);
      bar.updateMultiRoot(statuses);
      expect(item.text).to.equal("$(error) sqry: No Index");
    });

    it("shows building when any root is building (and none worse)", () => {
      const item = createMockStatusBarItem();
      const bar = new SqryStatusBar(item as any, null as any);
      const statuses = new Map<string, any>([
        ["/root/a", { symbol_count: 100, file_count: 10, age_seconds: 60 }],
        ["/root/b", { symbol_count: 200, file_count: 20, age_seconds: 60, building: true }],
      ]);
      bar.updateMultiRoot(statuses);
      expect(item.text).to.equal("$(sync~spin) sqry: Indexing...");
    });

    it("tooltip lists per-root summary", () => {
      const item = createMockStatusBarItem();
      const bar = new SqryStatusBar(item as any, null as any);
      const statuses = new Map<string, any>([
        ["project-a", { symbol_count: 100, file_count: 10, age_seconds: 60 }],
        ["project-b", { symbol_count: 200, file_count: 20, age_seconds: 100000 }],
      ]);
      bar.updateMultiRoot(statuses);
      expect(item.tooltip).to.include("2 roots");
      expect(item.tooltip).to.include("project-a");
      expect(item.tooltip).to.include("project-b");
    });
  });

  // ===== Search Panel: per-root tree view =====

  describe("SearchPanel tree view with multi-root", () => {
    // Build stubs for searchPanel module
    let workspaceFolders: Array<{ name: string; uri: { fsPath: string } }> | undefined;

    class EventEmitter {
      private listeners: Array<(...args: any[]) => void> = [];
      public event = (listener: (...args: any[]) => void) => {
        this.listeners.push(listener);
        return { dispose: () => {} };
      };
      public fire(..._args: any[]): void {
        // no-op in tests
      }
      public dispose(): void {
        this.listeners = [];
      }
    }

    const searchPanelVscodeStub = {
      __esModule: true,
      ThemeColor,
      ThemeIcon,
      TreeItem,
      TreeItemCollapsibleState,
      EventEmitter,
      workspace: {
        get workspaceFolders() {
          return workspaceFolders;
        },
        getWorkspaceFolder: () => undefined,
      },
      window: {
        createTreeView: () => ({
          dispose: () => {},
        }),
        showInformationMessage: () => Promise.resolve(),
      },
      Uri: {
        file: (p: string) => ({ fsPath: p, scheme: "file" }),
        parse: (s: string) => ({ fsPath: s, scheme: "file", toString: () => s }),
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
      vscode: searchPanelVscodeStub,
      "node:path": nodePath,
    }) as { SearchPanel: typeof import("../src/searchPanel").SearchPanel };

    const mockContext = {
      subscriptions: { push: () => {} },
    };

    afterEach(() => {
      workspaceFolders = undefined;
    });

    it("shows per-root grouping when >1 root has status", () => {
      workspaceFolders = [
        { name: "frontend", uri: { fsPath: "/workspace/frontend" } },
        { name: "backend", uri: { fsPath: "/workspace/backend" } },
      ];

      const panel = new SearchPanel(mockContext as any, null, null);
      panel.setIndexStatusForRoot("/workspace/frontend", {
        exists: true,
        symbol_count: 500,
        file_count: 25,
        supports_fuzzy: true,
        supports_relations: false,
      } as any);
      panel.setIndexStatusForRoot("/workspace/backend", {
        exists: true,
        symbol_count: 1000,
        file_count: 50,
        supports_fuzzy: true,
        supports_relations: false,
      } as any);

      // Access the tree data provider via getChildren on root
      // The panel exposes setIndexStatusForRoot which triggers tree refresh
      // Verify the status map has both entries
      const map = panel.getIndexStatusMap();
      expect(map.size).to.equal(2);
      expect(map.get("/workspace/frontend")?.symbol_count).to.equal(500);
      expect(map.get("/workspace/backend")?.symbol_count).to.equal(1000);
    });

    it("single workspace: no per-root grouping, flat stats view", () => {
      workspaceFolders = [
        { name: "myproject", uri: { fsPath: "/workspace/myproject" } },
      ];

      const panel = new SearchPanel(mockContext as any, null, null);
      panel.setIndexStatus({
        exists: true,
        symbol_count: 500,
        file_count: 25,
        supports_fuzzy: true,
        supports_relations: false,
      } as any);

      // Single root should not produce per-root items
      // The flat stats view is preserved
      const map = panel.getIndexStatusMap();
      // Single root: setIndexStatus does NOT populate the map
      // Only setIndexStatusForRoot does
      expect(map.size).to.equal(0);
    });
  });
});
