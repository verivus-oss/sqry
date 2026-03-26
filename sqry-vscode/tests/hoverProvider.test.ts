import { expect } from "chai";

// eslint-disable-next-line @typescript-eslint/no-var-requires
const proxyquire = require("proxyquire").noCallThru();

// ===== VS Code Stubs =====

class StubMarkdownString {
  public value = "";

  appendMarkdown(text: string): this {
    this.value += text;
    return this;
  }
}

class StubHover {
  constructor(public contents: StubMarkdownString) {}
}

class StubPosition {
  constructor(
    public line: number,
    public character: number,
  ) {}
}

class StubRange {
  constructor(
    public start: StubPosition,
    public end: StubPosition,
  ) {}
}

class StubUri {
  constructor(private readonly value: string) {}
  toString(): string {
    return this.value;
  }
  static parse(s: string): StubUri {
    return new StubUri(s);
  }
}

// Tracks onDidChangeConfig listeners for testing
let configChangeListeners: Array<() => void> = [];

// Configuration store
let configValues: Record<string, unknown> = {};

const vscodeStub = {
  __esModule: true,
  MarkdownString: StubMarkdownString,
  Hover: StubHover,
  Position: StubPosition,
  Range: StubRange,
  Uri: StubUri,
  workspace: {
    getConfiguration: (_section: string) => ({
      get: <T>(key: string, defaultValue: T): T => {
        const fullKey = `${_section}.${key}`;
        return fullKey in configValues
          ? (configValues[fullKey] as T)
          : defaultValue;
      },
    }),
    getWorkspaceFolder: (_uri: unknown): { uri: { fsPath: string }; name: string } | undefined => {
      return { uri: { fsPath: "/workspace" }, name: "test-workspace" };
    },
  },
};

// ===== Load module under test =====

const { SqryHoverProvider } = proxyquire("../src/hoverProvider", {
  vscode: vscodeStub,
}) as {
  SqryHoverProvider: typeof import("../src/hoverProvider").SqryHoverProvider;
};

// ===== Helpers =====

const TEST_URI = "file:///src/main.rs";
const TEST_WORKSPACE = {
  uri: { fsPath: "/workspace" },
  name: "test-workspace",
};

/** Build a stub CancellationToken */
function makeToken(cancelled = false): { isCancellationRequested: boolean } {
  return { isCancellationRequested: cancelled };
}

/** Build a stub TextDocument */
function makeDocument(
  uri: string,
  wordAtPosition: string | null,
): {
  uri: StubUri;
  getWordRangeAtPosition: (pos: unknown) => StubRange | undefined;
  getText: (range: StubRange) => string;
} {
  const stubUri = StubUri.parse(uri);
  return {
    uri: stubUri,
    getWordRangeAtPosition: (_pos: unknown) =>
      wordAtPosition !== null
        ? new StubRange(new StubPosition(0, 0), new StubPosition(0, wordAtPosition.length))
        : undefined,
    getText: (_range: StubRange) => wordAtPosition ?? "",
  };
}

/** Build a mock SqryClient */
function createMockClient(overrides: Record<string, unknown> = {}): {
  runQuery: (query: string, workspace: unknown) => Promise<{ symbols: unknown[] } | null>;
  onDidChangeConfig: (handler: () => void) => { dispose: () => void };
} {
  return {
    runQuery: async (_query: string, _workspace: unknown) => ({ symbols: [] }),
    onDidChangeConfig: (handler: () => void) => {
      configChangeListeners.push(handler);
      return {
        dispose: () => {
          configChangeListeners = configChangeListeners.filter(h => h !== handler);
        },
      };
    },
    ...overrides,
  };
}

// ===== Tests =====

describe("SqryHoverProvider", () => {
  beforeEach(() => {
    configValues = {};
    configChangeListeners = [];
  });

  // ===== Basic hover =====

  describe("provideHover", () => {
    it("returns hover with caller and callee counts when symbol is found", async () => {
      const client = createMockClient({
        runQuery: async (query: string) => {
          if (query.startsWith("callers:")) return { symbols: ["a", "b", "c"] };
          if (query.startsWith("callees:")) return { symbols: ["x"] };
          return { symbols: [] };
        },
      });

      const provider = new SqryHoverProvider(client as any, null);
      const doc = makeDocument(TEST_URI, "myFunction");
      const pos = new StubPosition(0, 3);
      const token = makeToken();

      const hover = await provider.provideHover(doc as any, pos as any, token as any);

      expect(hover).to.not.be.null;
      const md = (hover as unknown as StubHover).contents as StubMarkdownString;
      expect(md.value).to.include("3 callers");
      expect(md.value).to.include("1 callees");
    });

    it("returns null when no word is found at position", async () => {
      const client = createMockClient();
      const provider = new SqryHoverProvider(client as any, null);
      const doc = makeDocument(TEST_URI, null); // no word at position
      const pos = new StubPosition(0, 0);
      const token = makeToken();

      const hover = await provider.provideHover(doc as any, pos as any, token as any);

      expect(hover).to.be.null;
    });

    it("returns null when cancellation token is already cancelled", async () => {
      let queryCalled = false;
      const client = createMockClient({
        runQuery: async () => {
          queryCalled = true;
          return { symbols: [] };
        },
      });

      const provider = new SqryHoverProvider(client as any, null);
      const doc = makeDocument(TEST_URI, "someSymbol");
      const pos = new StubPosition(0, 0);
      const token = makeToken(true); // already cancelled

      const hover = await provider.provideHover(doc as any, pos as any, token as any);

      expect(hover).to.be.null;
      expect(queryCalled).to.be.false;
    });

    it("returns null on LSP error (graceful degradation)", async () => {
      const client = createMockClient({
        runQuery: async () => {
          throw new Error("LSP connection failed");
        },
      });

      const provider = new SqryHoverProvider(client as any, null);
      const doc = makeDocument(TEST_URI, "failingSymbol");
      const pos = new StubPosition(0, 0);
      const token = makeToken();

      const hover = await provider.provideHover(doc as any, pos as any, token as any);

      expect(hover).to.be.null;
    });

    it("returns null when hover.enabled is false", async () => {
      configValues["sqry.hover.enabled"] = false;

      let queryCalled = false;
      const client = createMockClient({
        runQuery: async () => {
          queryCalled = true;
          return { symbols: [] };
        },
      });

      const provider = new SqryHoverProvider(client as any, null);
      const doc = makeDocument(TEST_URI, "someSymbol");
      const pos = new StubPosition(0, 0);
      const token = makeToken();

      const hover = await provider.provideHover(doc as any, pos as any, token as any);

      expect(hover).to.be.null;
      expect(queryCalled).to.be.false;
    });

    it("returns zero callers and callees when queries return empty results", async () => {
      const client = createMockClient({
        runQuery: async () => ({ symbols: [] }),
      });

      const provider = new SqryHoverProvider(client as any, null);
      const doc = makeDocument(TEST_URI, "isolatedFn");
      const pos = new StubPosition(0, 0);
      const token = makeToken();

      const hover = await provider.provideHover(doc as any, pos as any, token as any);

      expect(hover).to.not.be.null;
      const md = (hover as unknown as StubHover).contents as StubMarkdownString;
      expect(md.value).to.include("0 callers");
      expect(md.value).to.include("0 callees");
    });

    it("handles null result from runQuery gracefully", async () => {
      const client = createMockClient({
        runQuery: async () => null,
      });

      const provider = new SqryHoverProvider(client as any, null);
      const doc = makeDocument(TEST_URI, "nullResultFn");
      const pos = new StubPosition(0, 0);
      const token = makeToken();

      const hover = await provider.provideHover(doc as any, pos as any, token as any);

      expect(hover).to.not.be.null;
      const md = (hover as unknown as StubHover).contents as StubMarkdownString;
      expect(md.value).to.include("0 callers");
      expect(md.value).to.include("0 callees");
    });
  });

  // ===== Cache =====

  describe("cache", () => {
    it("returns same result within 10s TTL (runQuery called only once)", async () => {
      let callCount = 0;
      const client = createMockClient({
        runQuery: async (query: string) => {
          callCount++;
          if (query.startsWith("callers:")) return { symbols: ["a", "b"] };
          return { symbols: [] };
        },
      });

      const provider = new SqryHoverProvider(client as any, null);
      const doc = makeDocument(TEST_URI, "cachedFn");
      const pos = new StubPosition(0, 0);
      const token = makeToken();

      // First call
      const hover1 = await provider.provideHover(doc as any, pos as any, token as any);
      // Second call — should use cache
      const hover2 = await provider.provideHover(doc as any, pos as any, token as any);

      expect(hover1).to.not.be.null;
      expect(hover2).to.not.be.null;
      // Two queries per call (callers + callees), so 2 calls total (not 4)
      expect(callCount).to.equal(2);
    });

    it("re-fetches after TTL expires", async () => {
      let callCount = 0;
      const client = createMockClient({
        runQuery: async () => {
          callCount++;
          return { symbols: [] };
        },
      });

      const provider = new SqryHoverProvider(client as any, null);
      const doc = makeDocument(TEST_URI, "expiredFn");
      const pos = new StubPosition(0, 0);
      const token = makeToken();

      // First call — populates cache
      await provider.provideHover(doc as any, pos as any, token as any);
      const firstCallCount = callCount;

      // Manually expire the cache entry by patching timestamp
      const cache: Map<string, { content: unknown; timestamp: number }> =
        (provider as any).cache;
      for (const [key, entry] of cache.entries()) {
        cache.set(key, { ...entry, timestamp: Date.now() - 11_000 }); // 11s ago
      }

      // Second call — cache expired, should re-fetch
      await provider.provideHover(doc as any, pos as any, token as any);

      expect(callCount).to.be.greaterThan(firstCallCount);
    });

    it("clears cache when onDidChangeConfig fires", async () => {
      let callCount = 0;
      const client = createMockClient({
        runQuery: async () => {
          callCount++;
          return { symbols: [] };
        },
      });

      const provider = new SqryHoverProvider(client as any, null);
      const doc = makeDocument(TEST_URI, "configFn");
      const pos = new StubPosition(0, 0);
      const token = makeToken();

      // First call — populates cache
      await provider.provideHover(doc as any, pos as any, token as any);
      const afterFirstCall = callCount;

      // Fire config change — should clear cache
      configChangeListeners.forEach(h => h());

      // Second call — cache cleared, re-fetches
      await provider.provideHover(doc as any, pos as any, token as any);

      expect(callCount).to.be.greaterThan(afterFirstCall);
    });
  });

  // ===== Dispose =====

  describe("dispose", () => {
    it("clears cache and disposes listeners", async () => {
      let queryCalled = false;
      const client = createMockClient({
        runQuery: async () => {
          queryCalled = true;
          return { symbols: ["x"] };
        },
      });

      const provider = new SqryHoverProvider(client as any, null);
      const doc = makeDocument(TEST_URI, "disposedFn");
      const pos = new StubPosition(0, 0);
      const token = makeToken();

      // Populate cache
      await provider.provideHover(doc as any, pos as any, token as any);
      expect(queryCalled).to.be.true;

      provider.dispose();

      // Cache should be empty after dispose
      const cache: Map<unknown, unknown> = (provider as any).cache;
      expect(cache.size).to.equal(0);

      // Config change listener should be removed after dispose
      expect(configChangeListeners).to.have.lengthOf(0);
    });
  });
});
