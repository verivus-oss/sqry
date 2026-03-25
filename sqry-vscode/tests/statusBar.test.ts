import { expect } from "chai";

// eslint-disable-next-line @typescript-eslint/no-var-requires
const proxyquire = require("proxyquire").noCallThru();

// Minimal vscode stub for statusBar module
const vscodeStub = {
  __esModule: true,
  ThemeColor: class ThemeColor {
    constructor(public id: string) {}
  },
};

// Load the module with the vscode stub
const { SqryStatusBar } = proxyquire("../src/statusBar", {
  vscode: vscodeStub,
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

describe("SqryStatusBar", () => {
  it("shows Ready state with database icon", () => {
    const item = createMockStatusBarItem();
    const bar = new SqryStatusBar(item as any, null as any);
    bar.update({ symbol_count: 1000, file_count: 50, age_seconds: 60 } as any);
    expect(item.text).to.equal("$(database) sqry: Ready");
    expect(item.command).to.equal("sqry.refreshStats");
    expect(item.backgroundColor).to.be.undefined;
  });

  it("shows Stale state when age > 24h", () => {
    const item = createMockStatusBarItem();
    const bar = new SqryStatusBar(item as any, null as any);
    bar.update({ symbol_count: 1000, file_count: 50, age_seconds: 90000 } as any);
    expect(item.text).to.equal("$(warning) sqry: Stale");
    expect(item.command).to.equal("sqry.index");
  });

  it("shows No Index state when status is null", () => {
    const item = createMockStatusBarItem();
    const bar = new SqryStatusBar(item as any, null as any);
    bar.update(null);
    expect(item.text).to.equal("$(error) sqry: No Index");
    expect(item.command).to.equal("sqry.index");
  });

  it("shows Building state", () => {
    const item = createMockStatusBarItem();
    const bar = new SqryStatusBar(item as any, null as any);
    bar.setBuilding();
    expect(item.text).to.equal("$(sync~spin) sqry: Indexing...");
  });

  it("shows Error state", () => {
    const item = createMockStatusBarItem();
    const bar = new SqryStatusBar(item as any, null as any);
    bar.setError("LSP crashed");
    expect(item.text).to.equal("$(error) sqry: Error");
    expect(item.tooltip).to.include("LSP crashed");
  });

  it("includes stats and age in tooltip when ready", () => {
    const item = createMockStatusBarItem();
    const bar = new SqryStatusBar(item as any, null as any);
    bar.update({ symbol_count: 1000, file_count: 50, age_seconds: 60 } as any);
    expect(item.tooltip).to.include("1000 symbols");
    expect(item.tooltip).to.include("50 files");
    expect(item.tooltip).to.include("indexed 1m ago");
  });

  it("formats age correctly in tooltip", () => {
    const item = createMockStatusBarItem();
    const bar = new SqryStatusBar(item as any, null as any);
    bar.update({ symbol_count: 100, file_count: 10, age_seconds: 7200 } as any);
    expect(item.tooltip).to.include("indexed 2h ago");
  });

  it("uses warning background for stale, none for building", () => {
    const item = createMockStatusBarItem();
    const bar = new SqryStatusBar(item as any, null as any);

    bar.update({ symbol_count: 1000, file_count: 50, age_seconds: 90000 } as any);
    expect(item.backgroundColor).to.not.be.undefined;

    bar.setBuilding();
    expect(item.backgroundColor).to.be.undefined;
  });

  it("uses error background for no index", () => {
    const item = createMockStatusBarItem();
    const bar = new SqryStatusBar(item as any, null as any);
    bar.update(null);
    expect(item.backgroundColor).to.not.be.undefined;
  });

  it("shows stale tooltip with warning text", () => {
    const item = createMockStatusBarItem();
    const bar = new SqryStatusBar(item as any, null as any);
    bar.update({ symbol_count: 1000, file_count: 50, age_seconds: 90000 } as any);
    expect(item.tooltip).to.include("older than 24 hours");
  });
});
