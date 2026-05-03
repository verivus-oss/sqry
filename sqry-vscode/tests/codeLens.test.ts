import { expect } from "chai";
import proxyquireModule from "proxyquire";

const proxyquire = proxyquireModule.noCallThru();

// ===== VS Code Stubs =====

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

class StubCodeLens {
  public data?: unknown;
  public command?: { title: string; command: string; arguments?: unknown[] };

  constructor(
    public range: StubRange,
    command?: { title: string; command: string; arguments?: unknown[] },
  ) {
    if (command) {
      this.command = command;
    }
  }
}

class StubUri {
  constructor(private readonly value: string) {}
  toString(): string {
    return this.value;
  }
  get fsPath(): string {
    return this.value;
  }
  static parse(s: string): StubUri {
    return new StubUri(s);
  }
}

// VS Code SymbolKind enum values
const SymbolKind = {
  File: 0,
  Module: 1,
  Namespace: 2,
  Package: 3,
  Class: 4,
  Method: 5,
  Property: 6,
  Field: 7,
  Constructor: 8,
  Enum: 9,
  Interface: 10,
  Function: 11,
  Variable: 12,
  Constant: 13,
  String: 14,
  Number: 15,
  Boolean: 16,
  Array: 17,
  Object: 18,
  Key: 19,
  Null: 20,
  EnumMember: 21,
  Struct: 22,
  Event: 23,
  Operator: 24,
  TypeParameter: 25,
};

// Tracks listeners
let configChangeListeners: Array<() => void> = [];
let onDidChangeConfigListeners: Array<(event: unknown) => void> = [];

// Configuration store
let configValues: Record<string, unknown> = {};

// Tracks executeCommand calls
let executedCommands: string[] = [];

// Document symbols returned by executeDocumentSymbolProvider
let documentSymbols: unknown[] = [];

const vscodeStub = {
  __esModule: true,
  Position: StubPosition,
  Range: StubRange,
  CodeLens: StubCodeLens,
  Uri: StubUri,
  SymbolKind,
  workspace: {
    getConfiguration: (_section: string) => ({
      get: <T>(key: string, defaultValue: T): T => {
        const fullKey = `${_section}.${key}`;
        return fullKey in configValues
          ? (configValues[fullKey] as T)
          : defaultValue;
      },
    }),
    getWorkspaceFolder: (
      _uri: unknown,
    ): { uri: { fsPath: string }; name: string } | undefined => {
      return { uri: { fsPath: "/workspace" }, name: "test-workspace" };
    },
    onDidChangeConfiguration: (
      handler: (event: unknown) => void,
    ): { dispose: () => void } => {
      onDidChangeConfigListeners.push(handler);
      return {
        dispose: () => {
          onDidChangeConfigListeners = onDidChangeConfigListeners.filter(
            (h) => h !== handler,
          );
        },
      };
    },
  },
  commands: {
    executeCommand: async (command: string, ..._args: unknown[]) => {
      executedCommands.push(command);
      if (command === "vscode.executeDocumentSymbolProvider") {
        return documentSymbols;
      }
      return undefined;
    },
  },
};

// ===== Load module under test =====

const { SqryCodeLensProvider } = proxyquire("../src/codeLens", {
  vscode: vscodeStub,
  "./config": {
    readSettings: () => ({
      codeLensEnabled:
        configValues["sqry.codeLens.enabled"] !== undefined
          ? configValues["sqry.codeLens.enabled"]
          : true,
    }),
  },
}) as {
  SqryCodeLensProvider: typeof import("../src/codeLens").SqryCodeLensProvider;
};

// ===== Helpers =====

function makeToken(cancelled = false): { isCancellationRequested: boolean } {
  return { isCancellationRequested: cancelled };
}

function makeDocument(uri = "file:///src/main.rs"): {
  uri: StubUri;
} {
  return {
    uri: StubUri.parse(uri),
  };
}

/** Build a stub DocumentSymbol */
function makeSymbol(
  name: string,
  kind: number,
  children: unknown[] = [],
): unknown {
  return {
    name,
    kind,
    selectionRange: new StubRange(
      new StubPosition(0, 0),
      new StubPosition(0, name.length),
    ),
    children,
  };
}

function createMockClient(
  overrides: Record<string, unknown> = {},
): {
  batchCallerCalleeCount: (
    symbols: Array<{ name: string }>,
    workspace: unknown,
  ) => Promise<{ counts: Array<{ name: string; callers: number; callees: number }> }>;
  onDidChangeConfig: (handler: () => void) => { dispose: () => void };
} {
  return {
    batchCallerCalleeCount: async (
      symbols: Array<{ name: string }>,
    ) => ({
      counts: symbols.map((s) => ({
        name: s.name,
        callers: 3,
        callees: 2,
      })),
    }),
    onDidChangeConfig: (handler: () => void) => {
      configChangeListeners.push(handler);
      return {
        dispose: () => {
          configChangeListeners = configChangeListeners.filter(
            (h) => h !== handler,
          );
        },
      };
    },
    ...overrides,
  };
}

// ===== Tests =====

describe("SqryCodeLensProvider", () => {
  beforeEach(() => {
    configValues = {};
    configChangeListeners = [];
    onDidChangeConfigListeners = [];
    executedCommands = [];
    documentSymbols = [];
  });

  // ===== Segments =====

  describe("segments", () => {
    it("with default segments [callers, callees], produces 2 CodeLens items per symbol", async () => {
      documentSymbols = [makeSymbol("myFunction", SymbolKind.Function)];

      const client = createMockClient();
      const provider = new SqryCodeLensProvider(client as any);
      const doc = makeDocument();
      const token = makeToken();

      const lenses = await provider.provideCodeLenses(doc as any, token as any);

      expect(lenses).to.have.lengthOf(2);

      // Resolve both lenses
      const resolved0 = await provider.resolveCodeLens(
        lenses[0] as any,
        token as any,
      );
      const resolved1 = await provider.resolveCodeLens(
        lenses[1] as any,
        token as any,
      );

      expect(resolved0.command!.title).to.equal("Sqry callers: 3");
      expect(resolved0.command!.arguments![0]).to.equal(
        "callers:myFunction",
      );

      expect(resolved1.command!.title).to.equal("Sqry callees: 2");
      expect(resolved1.command!.arguments![0]).to.equal(
        "callees:myFunction",
      );
    });

    it('with segments ["callers"], produces 1 CodeLens per symbol', async () => {
      configValues["sqry.codeLens.segments"] = ["callers"];
      documentSymbols = [makeSymbol("myFunction", SymbolKind.Function)];

      const client = createMockClient();
      const provider = new SqryCodeLensProvider(client as any);
      const doc = makeDocument();
      const token = makeToken();

      const lenses = await provider.provideCodeLenses(doc as any, token as any);

      expect(lenses).to.have.lengthOf(1);

      const resolved = await provider.resolveCodeLens(
        lenses[0] as any,
        token as any,
      );
      expect(resolved.command!.title).to.equal("Sqry callers: 3");
      expect(resolved.command!.arguments![0]).to.equal(
        "callers:myFunction",
      );
    });
  });

  // ===== Budget =====

  describe("budget", () => {
    it("symbols beyond 100-symbol budget are skipped", async () => {
      // Create 101 function symbols
      documentSymbols = [];
      for (let i = 0; i < 101; i++) {
        documentSymbols.push(makeSymbol(`fn_${i}`, SymbolKind.Function));
      }

      // Only callers segment to simplify counting
      configValues["sqry.codeLens.segments"] = ["callers"];

      const client = createMockClient();
      const provider = new SqryCodeLensProvider(client as any);
      const doc = makeDocument();
      const token = makeToken();

      const lenses = await provider.provideCodeLenses(doc as any, token as any);

      // Should be 100 (max budget), not 101
      expect(lenses).to.have.lengthOf(100);
    });
  });

  // ===== Cache invalidation =====

  describe("cache", () => {
    it("cache is cleared on config change", async () => {
      documentSymbols = [makeSymbol("cachedFn", SymbolKind.Function)];
      configValues["sqry.codeLens.segments"] = ["callers"];

      let callCount = 0;
      const client = createMockClient({
        batchCallerCalleeCount: async (
          symbols: Array<{ name: string }>,
        ) => {
          callCount++;
          return {
            counts: symbols.map((s) => ({
              name: s.name,
              callers: 5,
              callees: 1,
            })),
          };
        },
      });

      const provider = new SqryCodeLensProvider(client as any);
      const doc = makeDocument();
      const token = makeToken();

      // First resolve — populates cache
      const lenses1 = await provider.provideCodeLenses(
        doc as any,
        token as any,
      );
      await provider.resolveCodeLens(lenses1[0] as any, token as any);
      expect(callCount).to.equal(1);

      // Resolve again — should use cache
      const lenses2 = await provider.provideCodeLenses(
        doc as any,
        token as any,
      );
      await provider.resolveCodeLens(lenses2[0] as any, token as any);
      expect(callCount).to.equal(1); // still 1, from cache

      // Simulate config change event
      for (const listener of onDidChangeConfigListeners) {
        listener({
          affectsConfiguration: (section: string) =>
            section === "sqry.codeLens.segments",
        });
      }

      // Resolve again — cache cleared, should re-fetch
      const lenses3 = await provider.provideCodeLenses(
        doc as any,
        token as any,
      );
      await provider.resolveCodeLens(lenses3[0] as any, token as any);
      expect(callCount).to.equal(2);
    });

    it("cache is cleared when onDidChangeConfig fires from client", async () => {
      documentSymbols = [makeSymbol("clientConfigFn", SymbolKind.Function)];
      configValues["sqry.codeLens.segments"] = ["callers"];

      let callCount = 0;
      const client = createMockClient({
        batchCallerCalleeCount: async (
          symbols: Array<{ name: string }>,
        ) => {
          callCount++;
          return {
            counts: symbols.map((s) => ({
              name: s.name,
              callers: 2,
              callees: 0,
            })),
          };
        },
      });

      const provider = new SqryCodeLensProvider(client as any);
      const doc = makeDocument();
      const token = makeToken();

      // First resolve
      const lenses1 = await provider.provideCodeLenses(
        doc as any,
        token as any,
      );
      await provider.resolveCodeLens(lenses1[0] as any, token as any);
      expect(callCount).to.equal(1);

      // Fire client config change — clears cache
      configChangeListeners.forEach((h) => h());

      // Resolve again — should re-fetch
      const lenses2 = await provider.provideCodeLenses(
        doc as any,
        token as any,
      );
      await provider.resolveCodeLens(lenses2[0] as any, token as any);
      expect(callCount).to.equal(2);
    });
  });

  // ===== Graceful degradation =====

  describe("graceful degradation", () => {
    it('batch request failure shows "?" instead of count', async () => {
      documentSymbols = [makeSymbol("failingFn", SymbolKind.Function)];

      const client = createMockClient({
        batchCallerCalleeCount: async () => {
          throw new Error("LSP connection failed");
        },
      });

      const provider = new SqryCodeLensProvider(client as any);
      const doc = makeDocument();
      const token = makeToken();

      const lenses = await provider.provideCodeLenses(doc as any, token as any);
      expect(lenses).to.have.lengthOf(2);

      const resolved0 = await provider.resolveCodeLens(
        lenses[0] as any,
        token as any,
      );
      const resolved1 = await provider.resolveCodeLens(
        lenses[1] as any,
        token as any,
      );

      expect(resolved0.command!.title).to.equal("Sqry callers: ?");
      expect(resolved1.command!.title).to.equal("Sqry callees: ?");
    });
  });

  // ===== Click command =====

  describe("click command", () => {
    it("each CodeLens segment has correct click command", async () => {
      documentSymbols = [makeSymbol("clickFn", SymbolKind.Function)];

      const client = createMockClient();
      const provider = new SqryCodeLensProvider(client as any);
      const doc = makeDocument();
      const token = makeToken();

      const lenses = await provider.provideCodeLenses(doc as any, token as any);

      const resolved0 = await provider.resolveCodeLens(
        lenses[0] as any,
        token as any,
      );
      const resolved1 = await provider.resolveCodeLens(
        lenses[1] as any,
        token as any,
      );

      expect(resolved0.command!.command).to.equal("sqry.runQueryInternal");
      expect(resolved0.command!.arguments).to.deep.equal([
        "callers:clickFn",
      ]);

      expect(resolved1.command!.command).to.equal("sqry.runQueryInternal");
      expect(resolved1.command!.arguments).to.deep.equal([
        "callees:clickFn",
      ]);
    });
  });

  // ===== Eligible symbols =====

  describe("eligible symbols", () => {
    it("only provides CodeLens for Functions, Methods, and Constructors", async () => {
      configValues["sqry.codeLens.segments"] = ["callers"];
      documentSymbols = [
        makeSymbol("myFunction", SymbolKind.Function),
        makeSymbol("myMethod", SymbolKind.Method),
        makeSymbol("myConstructor", SymbolKind.Constructor),
        makeSymbol("myVariable", SymbolKind.Variable),
        makeSymbol("MyClass", SymbolKind.Class),
      ];

      const client = createMockClient();
      const provider = new SqryCodeLensProvider(client as any);
      const doc = makeDocument();
      const token = makeToken();

      const lenses = await provider.provideCodeLenses(doc as any, token as any);

      // 3 eligible symbols * 1 segment each = 3
      expect(lenses).to.have.lengthOf(3);
    });
  });

  // ===== Qualified names =====

  describe("qualified names", () => {
    it("builds qualified name with class ancestor", async () => {
      configValues["sqry.codeLens.segments"] = ["callers"];
      documentSymbols = [
        makeSymbol("MyClass", SymbolKind.Class, [
          makeSymbol("myMethod", SymbolKind.Method),
        ]),
      ];

      const client = createMockClient({
        batchCallerCalleeCount: async (
          symbols: Array<{ name: string }>,
        ) => ({
          counts: symbols.map((s) => ({
            name: s.name,
            callers: 1,
            callees: 0,
          })),
        }),
      });

      const provider = new SqryCodeLensProvider(client as any);
      const doc = makeDocument();
      const token = makeToken();

      const lenses = await provider.provideCodeLenses(doc as any, token as any);
      expect(lenses).to.have.lengthOf(1);

      const resolved = await provider.resolveCodeLens(
        lenses[0] as any,
        token as any,
      );
      expect(resolved.command!.arguments![0]).to.equal(
        "callers:MyClass.myMethod",
      );
    });
  });

  // ===== Dispose =====

  describe("dispose", () => {
    it("clears cache and removes listeners", () => {
      const client = createMockClient();
      const provider = new SqryCodeLensProvider(client as any);

      provider.dispose();

      const cache: Map<unknown, unknown> = (provider as any).cache;
      expect(cache.size).to.equal(0);
      expect(configChangeListeners).to.have.lengthOf(0);
    });
  });

  // ===== Disabled =====

  describe("disabled", () => {
    it("returns empty lenses when codeLens is disabled", async () => {
      configValues["sqry.codeLens.enabled"] = false;
      documentSymbols = [makeSymbol("fn", SymbolKind.Function)];

      const client = createMockClient();
      const provider = new SqryCodeLensProvider(client as any);
      const doc = makeDocument();
      const token = makeToken();

      const lenses = await provider.provideCodeLenses(doc as any, token as any);
      expect(lenses).to.have.lengthOf(0);
    });
  });
});
