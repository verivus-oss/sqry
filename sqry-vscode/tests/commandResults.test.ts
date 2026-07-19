import { expect } from "chai";
import proxyquireModule from "proxyquire";

const proxyquire = proxyquireModule.noCallThru();

// ── recorded side effects ────────────────────────────────────────────────────
let executeCommandCalls: Array<{ command: string; args: unknown[] }> = [];
let infoMessages: string[] = [];
let errorMessages: string[] = [];
let createdPanels: Array<{ title: string; html: string; revealCount: number }> = [];
let outputLines: string[] = [];
// The explain panel is a module-level singleton, so it survives across tests.
// Track live mock panels and dispose them before each test to reset that state.
let livePanels: Array<{ dispose: () => void }> = [];

function resetRecorders(): void {
  for (const panel of livePanels) {
    panel.dispose();
  }
  livePanels = [];
  executeCommandCalls = [];
  infoMessages = [];
  errorMessages = [];
  createdPanels = [];
  outputLines = [];
}

const outputChannelStub = {
  appendLine: (line: string) => {
    outputLines.push(line);
  },
} as unknown as import("vscode").OutputChannel;

const vscodeStub = {
  __esModule: true,
  Uri: {
    parse: (str: string) => ({ scheme: "file", path: str, toString: () => str }),
  },
  ViewColumn: { One: 1, Beside: 2 },
  Position: class {
    constructor(public line: number, public character: number) {}
  },
  Range: class {
    constructor(
      public startLine: number,
      public startCharacter: number,
      public endLine: number,
      public endCharacter: number,
    ) {}
  },
  Location: class {
    constructor(public uri: unknown, public range: unknown) {}
  },
  commands: {
    executeCommand: (command: string, ...args: unknown[]) => {
      executeCommandCalls.push({ command, args });
      return Promise.resolve();
    },
  },
  window: {
    showInformationMessage: (message: string) => {
      infoMessages.push(message);
      return Promise.resolve(undefined);
    },
    showErrorMessage: (message: string) => {
      errorMessages.push(message);
      return Promise.resolve(undefined);
    },
    createWebviewPanel: (_type: string, title: string) => {
      const record = { title, html: "", revealCount: 0 };
      createdPanels.push(record);
      let disposeHandler: (() => void) | undefined;
      const panel = {
        webview: {
          set html(value: string) {
            record.html = value;
          },
          get html() {
            return record.html;
          },
        },
        set title(value: string) {
          record.title = value;
        },
        reveal: () => {
          record.revealCount++;
        },
        onDidDispose: (handler: () => void) => {
          disposeHandler = handler;
          return { dispose: () => disposeHandler?.() };
        },
        dispose: () => disposeHandler?.(),
      };
      livePanels.push(panel);
      return panel;
    },
  },
};

const mod = proxyquire("../src/commandResults", { vscode: vscodeStub }) as typeof import("../src/commandResults");

const {
  SQRY_SHOW_CALLERS,
  SQRY_SHOW_REFERENCES,
  SQRY_EXPLAIN_SYMBOL,
  isRenderedCommand,
  handleExecuteCommandResult,
  extractContext,
  extractLocations,
  toVscodeLocation,
  escapeHtml,
  buildExplainHtml,
} = mod;

const CTX = { uri: "file:///a.rs", position: { line: 3, character: 7 } };

function loc(uri: string, line: number): unknown {
  return {
    uri,
    range: { start: { line, character: 0 }, end: { line, character: 4 } },
  };
}

describe("commandResults", () => {
  beforeEach(resetRecorders);

  describe("isRenderedCommand", () => {
    it("recognizes the three server-owned commands", () => {
      expect(isRenderedCommand(SQRY_SHOW_CALLERS)).to.equal(true);
      expect(isRenderedCommand(SQRY_SHOW_REFERENCES)).to.equal(true);
      expect(isRenderedCommand(SQRY_EXPLAIN_SYMBOL)).to.equal(true);
    });
    it("ignores other commands (e.g. sqry.index)", () => {
      expect(isRenderedCommand("sqry.index")).to.equal(false);
      expect(isRenderedCommand("editor.action.showReferences")).to.equal(false);
    });
  });

  describe("handleExecuteCommandResult passthrough", () => {
    it("forwards a non-rendered command and returns its result without rendering", async () => {
      let seen: { command: string; args: unknown[] } | undefined;
      const result = await handleExecuteCommandResult(
        "sqry.index",
        [CTX],
        (command, args) => {
          seen = { command, args };
          return Promise.resolve("passthrough");
        },
        outputChannelStub,
      );
      expect(result).to.equal("passthrough");
      expect(seen).to.deep.equal({ command: "sqry.index", args: [CTX] });
      expect(executeCommandCalls).to.have.length(0);
      expect(createdPanels).to.have.length(0);
      expect(infoMessages).to.have.length(0);
    });
  });

  describe("references / callers rendering", () => {
    it("opens the peek view with converted locations anchored at the context", async () => {
      const result = {
        symbol: { name: "foo" },
        results: { locations: [loc("file:///b.rs", 2), loc("file:///c.rs", 9)] },
      };
      await handleExecuteCommandResult(
        SQRY_SHOW_CALLERS,
        [CTX],
        () => Promise.resolve(result),
        outputChannelStub,
      );
      expect(executeCommandCalls).to.have.length(1);
      const call = executeCommandCalls[0];
      expect(call.command).to.equal("editor.action.showReferences");
      const [anchorUri, anchorPos, locations] = call.args as [
        { toString: () => string },
        { line: number; character: number },
        unknown[],
      ];
      expect(anchorUri.toString()).to.equal(CTX.uri);
      expect(anchorPos.line).to.equal(3);
      expect(anchorPos.character).to.equal(7);
      expect(locations).to.have.length(2);
    });

    it("shows an info message and no peek when there are no callers", async () => {
      await handleExecuteCommandResult(
        SQRY_SHOW_CALLERS,
        [CTX],
        () => Promise.resolve({ symbol: { name: "foo" }, results: { locations: [] } }),
        outputChannelStub,
      );
      expect(executeCommandCalls).to.have.length(0);
      expect(infoMessages).to.have.length(1);
      expect(infoMessages[0]).to.contain("no callers");
      expect(infoMessages[0]).to.contain("foo");
    });

    it("uses the 'references' label for showReferences", async () => {
      await handleExecuteCommandResult(
        SQRY_SHOW_REFERENCES,
        [CTX],
        () => Promise.resolve({ results: { locations: [] } }),
        outputChannelStub,
      );
      expect(infoMessages[0]).to.contain("no references");
    });

    it("falls back to the first location as anchor when context args are malformed", async () => {
      await handleExecuteCommandResult(
        SQRY_SHOW_CALLERS,
        [{ bogus: true }],
        () => Promise.resolve({ results: { locations: [loc("file:///d.rs", 5)] } }),
        outputChannelStub,
      );
      expect(executeCommandCalls).to.have.length(1);
      const [anchorUri] = executeCommandCalls[0].args as [{ toString: () => string }];
      expect(anchorUri.toString()).to.equal("file:///d.rs");
    });
  });

  describe("explain rendering", () => {
    it("opens a webview with the signature and documentation", async () => {
      await handleExecuteCommandResult(
        SQRY_EXPLAIN_SYMBOL,
        [CTX],
        () =>
          Promise.resolve({
            name: "foo",
            qualifiedName: "mod::foo",
            language: "rust",
            signature: "fn foo(x: i32) -> i32",
            documentation: "Adds one.",
          }),
        outputChannelStub,
      );
      expect(createdPanels).to.have.length(1);
      expect(createdPanels[0].title).to.contain("foo");
      expect(createdPanels[0].html).to.contain("fn foo(x: i32)");
      expect(createdPanels[0].html).to.contain("Adds one.");
      expect(createdPanels[0].html).to.contain("mod::foo");
    });

    it("reuses the singleton panel on a second explain", async () => {
      const next = () => Promise.resolve({ name: "foo", signature: "fn foo()" });
      await handleExecuteCommandResult(SQRY_EXPLAIN_SYMBOL, [CTX], next, outputChannelStub);
      await handleExecuteCommandResult(SQRY_EXPLAIN_SYMBOL, [CTX], next, outputChannelStub);
      expect(createdPanels).to.have.length(1);
      expect(createdPanels[0].revealCount).to.be.greaterThan(0);
    });

    it("shows an info message when there is no symbol at the position", async () => {
      await handleExecuteCommandResult(
        SQRY_EXPLAIN_SYMBOL,
        [CTX],
        () => Promise.resolve(null),
        outputChannelStub,
      );
      expect(createdPanels).to.have.length(0);
      expect(infoMessages[0]).to.contain("no symbol");
    });
  });

  describe("error handling", () => {
    it("surfaces a server error and returns undefined without rendering", async () => {
      const result = await handleExecuteCommandResult(
        SQRY_SHOW_CALLERS,
        [CTX],
        () => Promise.reject(new Error("boom")),
        outputChannelStub,
      );
      expect(result).to.equal(undefined);
      expect(errorMessages).to.have.length(1);
      expect(errorMessages[0]).to.contain("boom");
      expect(outputLines.some((line) => line.includes("boom"))).to.equal(true);
      expect(executeCommandCalls).to.have.length(0);
    });
  });

  describe("pure helpers", () => {
    it("extractContext accepts well-formed args and rejects malformed ones", () => {
      expect(extractContext([CTX])).to.deep.equal(CTX);
      expect(extractContext([])).to.equal(undefined);
      expect(extractContext([{ uri: 1, position: CTX.position }])).to.equal(undefined);
      expect(extractContext([{ uri: "x", position: { line: "0", character: 0 } }])).to.equal(
        undefined,
      );
    });

    it("extractLocations returns only well-formed LSP locations", () => {
      const raw = {
        results: {
          locations: [loc("file:///a.rs", 1), { uri: "no-range" }, { nonsense: true }, null],
        },
      };
      expect(extractLocations(raw)).to.have.length(1);
      expect(extractLocations({})).to.have.length(0);
      expect(extractLocations(undefined)).to.have.length(0);
    });

    it("toVscodeLocation maps uri and range fields", () => {
      const v = toVscodeLocation(loc("file:///a.rs", 4) as never) as unknown as {
        uri: { toString: () => string };
        range: { startLine: number; endCharacter: number };
      };
      expect(v.uri.toString()).to.equal("file:///a.rs");
      expect(v.range.startLine).to.equal(4);
      expect(v.range.endCharacter).to.equal(4);
    });

    it("escapeHtml neutralizes markup", () => {
      expect(escapeHtml(`<script>"x"&'y'`)).to.equal(
        "&lt;script&gt;&quot;x&quot;&amp;&#39;y&#39;",
      );
    });

    it("buildExplainHtml escapes payload content and handles missing documentation", () => {
      const html = buildExplainHtml({ name: "<b>", signature: "fn <b>()", documentation: "" });
      expect(html).to.contain("fn &lt;b&gt;()");
      expect(html).to.not.contain("<b>()");
      expect(html).to.contain("No documentation available.");
    });
  });
});
