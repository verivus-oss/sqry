import { expect } from "chai";
import proxyquireModule from "proxyquire";

const proxyquire = proxyquireModule.noCallThru();

// Track posted messages and dispose calls
let postedMessages: unknown[] = [];
let disposeCallCount = 0;
let revealCallCount = 0;
let onDidReceiveMessageHandler: ((message: unknown) => void) | null = null;
let onDidDisposeHandler: (() => void) | null = null;
let executedCommands: Array<{ command: string; args: unknown[] }> = [];

function createMockPanel() {
  return {
    webview: {
      html: "",
      postMessage: (msg: unknown) => {
        postedMessages.push(msg);
        return Promise.resolve(true);
      },
      onDidReceiveMessage: (handler: (message: unknown) => void) => {
        onDidReceiveMessageHandler = handler;
        return { dispose: () => {} };
      },
      asWebviewUri: (uri: unknown) => uri,
    },
    onDidDispose: (handler: () => void) => {
      onDidDisposeHandler = handler;
      return { dispose: () => {} };
    },
    reveal: () => {
      revealCallCount++;
    },
    dispose: () => {
      disposeCallCount++;
      if (onDidDisposeHandler) {
        onDidDisposeHandler();
      }
    },
  };
}

// Build vscode stub
const vscodeStub = {
  __esModule: true,
  Uri: {
    file: (path: string) => ({ scheme: "file", fsPath: path, path }),
    parse: (str: string) => ({ scheme: "file", fsPath: str, path: str }),
    joinPath: (base: unknown, ...parts: string[]) => parts.join("/"),
  },
  ViewColumn: { One: 1, Beside: 2 },
  Position: class {
    constructor(public line: number, public character: number) {}
  },
  Selection: class {
    constructor(public anchor: unknown, public active: unknown) {}
  },
  Range: class {
    constructor(public start: unknown, public end: unknown) {}
  },
  window: {
    createWebviewPanel: () => createMockPanel(),
    showTextDocument: () => Promise.resolve({ selection: null, revealRange: () => {} }),
  },
  workspace: {
    openTextDocument: () => Promise.resolve({}),
  },
  commands: {
    // Present so a regression that navigates with `vscode.open` is recorded
    // rather than throwing on an undefined stub.
    executeCommand: (command: string, ...args: unknown[]) => {
      executedCommands.push({ command, args });
      return Promise.resolve(undefined);
    },
  },
};

// Record what the webview navigation handler does, so a regression that opens
// `message.file` directly instead of routing through the guard is visible.
let guardOpenCalls: Array<{ filePath: string; options: unknown }> = [];

const { SqryGraphPanel } = proxyquire("../src/graphPanel", {
  vscode: vscodeStub,
  // Stub the workspace guard so the real one (which requires the host `vscode`
  // module) is not pulled in transitively during unit tests.
  "./workspaceGuard": {
    openFileWithinWorkspace: async (filePath: string, options: unknown) => {
      guardOpenCalls.push({ filePath, options });
      return undefined;
    },
  },
}) as { SqryGraphPanel: typeof import("../src/graphPanel").SqryGraphPanel };

describe("SqryGraphPanel", () => {
  beforeEach(() => {
    postedMessages = [];
    disposeCallCount = 0;
    revealCallCount = 0;
    onDidReceiveMessageHandler = null;
    onDidDisposeHandler = null;

    // Reset internal static panels map by creating and disposing
    // any leftover panels from prior tests
    try {
      const panel1 = SqryGraphPanel.createOrShow({} as any, "__cleanup_callGraph__");
      panel1.dispose();
    } catch {
      // ignore
    }
    try {
      const panel2 = SqryGraphPanel.createOrShow({} as any, "__cleanup_deps__");
      panel2.dispose();
    } catch {
      // ignore
    }
    postedMessages = [];
    disposeCallCount = 0;
    revealCallCount = 0;
    guardOpenCalls = [];
    executedCommands = [];
  });

  // The webview hands back a file path derived from indexed data, which is the
  // highest-risk navigation surface in the extension: it is an imperative open,
  // not a link the user inspects first. It must go through the guard.
  describe("navigateToFile message handling", () => {
    it("routes the webview navigation request through the workspace guard", async () => {
      const panel = SqryGraphPanel.createOrShow({} as any, "callGraph");
      expect(onDidReceiveMessageHandler).to.not.be.null;

      await onDidReceiveMessageHandler!({
        type: "navigateToFile",
        file: "/outside/evil.rs",
        line: 3,
      });

      expect(guardOpenCalls).to.have.lengthOf(1);
      expect(guardOpenCalls[0].filePath).to.equal("/outside/evil.rs");
      expect(executedCommands.filter((c) => c.command === "vscode.open")).to.have.lengthOf(0);
      panel.dispose();
    });

    it("ignores messages that are not a navigation request", async () => {
      const panel = SqryGraphPanel.createOrShow({} as any, "callGraph");

      await onDidReceiveMessageHandler!({ type: "somethingElse", file: "/src/a.rs", line: 1 });
      await onDidReceiveMessageHandler!({ type: "navigateToFile", line: 1 });

      expect(guardOpenCalls).to.have.lengthOf(0);
      panel.dispose();
    });
  });

  it("createOrShow() creates a panel", () => {
    const panel = SqryGraphPanel.createOrShow({} as any, "callGraph");
    expect(panel).to.not.be.undefined;
    expect(panel).to.not.be.null;
    panel.dispose();
  });

  it("second call with same mode reuses existing panel", () => {
    const panel1 = SqryGraphPanel.createOrShow({} as any, "callGraph");
    revealCallCount = 0;
    const panel2 = SqryGraphPanel.createOrShow({} as any, "callGraph");
    expect(panel2).to.equal(panel1);
    expect(revealCallCount).to.equal(1);
    panel1.dispose();
  });

  it("different mode creates new panel", () => {
    const panel1 = SqryGraphPanel.createOrShow({} as any, "callGraph");
    const panel2 = SqryGraphPanel.createOrShow({} as any, "dependencies");
    expect(panel2).to.not.equal(panel1);
    panel1.dispose();
    panel2.dispose();
  });

  it("sendGraphData() truncates nodes at 500 limit", () => {
    const panel = SqryGraphPanel.createOrShow({} as any, "callGraph");
    postedMessages = [];

    const nodes = [];
    for (let i = 0; i < 600; i++) {
      nodes.push({ id: `n${i}`, label: `Node ${i}` });
    }

    panel.sendGraphData(nodes, []);

    expect(postedMessages).to.have.lengthOf(1);
    const msg = postedMessages[0] as any;
    expect(msg.type).to.equal("graphData");
    expect(msg.nodes).to.have.lengthOf(500);
    expect(msg.truncated).to.be.true;
    expect(msg.totalNodes).to.equal(600);
    panel.dispose();
  });

  it("sendGraphData() truncates edges at 2000 limit", () => {
    const panel = SqryGraphPanel.createOrShow({} as any, "callGraph");
    postedMessages = [];

    const edges = [];
    for (let i = 0; i < 2500; i++) {
      edges.push({ source: `n${i}`, target: `n${i + 1}` });
    }

    panel.sendGraphData([{ id: "n0", label: "root" }], edges);

    expect(postedMessages).to.have.lengthOf(1);
    const msg = postedMessages[0] as any;
    expect(msg.type).to.equal("graphData");
    expect(msg.edges).to.have.lengthOf(2000);
    expect(msg.truncated).to.be.true;
    expect(msg.totalEdges).to.equal(2500);
    panel.dispose();
  });

  it("dispose() cleans up panel reference so next createOrShow creates fresh panel", () => {
    const panel1 = SqryGraphPanel.createOrShow({} as any, "testDispose");
    disposeCallCount = 0;
    panel1.dispose();

    // dispose() should have been called on the underlying webview panel
    expect(disposeCallCount).to.equal(1);

    // After dispose, creating with same mode should create a new panel.
    // The reveal count before creating panel2 helps verify a fresh panel was created
    // (reveal is only called when reusing an existing, non-disposed panel).
    revealCallCount = 0;
    const panel2 = SqryGraphPanel.createOrShow({} as any, "testDispose");
    // If the old panel was properly cleaned up, sendError should post a message
    // (proving panel2 is functional, not a stale reference)
    postedMessages = [];
    panel2.sendError("test");
    expect(postedMessages).to.have.lengthOf(1);
    expect((postedMessages[0] as any).type).to.equal("error");
    panel2.dispose();
  });

  it("sendGraphData() does not truncate when under limits", () => {
    const panel = SqryGraphPanel.createOrShow({} as any, "callGraph");
    postedMessages = [];

    const nodes = [
      { id: "a", label: "A" },
      { id: "b", label: "B" },
    ];
    const edges = [{ source: "a", target: "b" }];

    panel.sendGraphData(nodes, edges);

    const msg = postedMessages[0] as any;
    expect(msg.nodes).to.have.lengthOf(2);
    expect(msg.edges).to.have.lengthOf(1);
    expect(msg.truncated).to.be.false;
    panel.dispose();
  });

  it("sendError() posts error message", () => {
    const panel = SqryGraphPanel.createOrShow({} as any, "callGraph");
    postedMessages = [];

    panel.sendError("Something went wrong");

    expect(postedMessages).to.have.lengthOf(1);
    const msg = postedMessages[0] as any;
    expect(msg.type).to.equal("error");
    expect(msg.message).to.equal("Something went wrong");
    panel.dispose();
  });
});
