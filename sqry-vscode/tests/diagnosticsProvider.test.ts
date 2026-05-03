import { expect } from "chai";
import proxyquireModule from "proxyquire";

const proxyquire = proxyquireModule.noCallThru();

// ===== VS Code Stubs =====

class StubRange {
  constructor(
    public startLine: number,
    public startChar: number,
    public endLine: number,
    public endChar: number,
  ) {}
}

class StubPosition {
  constructor(
    public line: number,
    public character: number,
  ) {}
}

class StubLocation {
  constructor(
    public uri: { toString(): string },
    public range: StubRange,
  ) {}
}

class StubDiagnostic {
  public tags?: number[];
  public source?: string;
  public code?: string;
  public relatedInformation?: StubDiagnosticRelatedInformation[];

  constructor(
    public range: StubRange,
    public message: string,
    public severity: number,
  ) {}
}

class StubDiagnosticRelatedInformation {
  constructor(
    public location: StubLocation,
    public message: string,
  ) {}
}

class StubUri {
  constructor(private readonly value: string) {}
  toString(): string {
    return this.value;
  }
  get fsPath(): string {
    return this.value.replace("file://", "");
  }
  static parse(s: string): StubUri {
    return new StubUri(s);
  }
  static file(s: string): StubUri {
    return new StubUri(`file://${s}`);
  }
}

// Mock DiagnosticCollection
class MockDiagnosticCollection {
  public entries = new Map<string, StubDiagnostic[]>();
  public disposed = false;

  set(uri: StubUri, diagnostics: StubDiagnostic[]): void {
    this.entries.set(uri.toString(), diagnostics);
  }

  delete(uri: StubUri): void {
    this.entries.delete(uri.toString());
  }

  clear(): void {
    this.entries.clear();
  }

  dispose(): void {
    this.disposed = true;
  }
}

// Configuration store for tests
let configValues: Record<string, unknown> = {};

const vscodeStub = {
  __esModule: true,
  Range: StubRange,
  Position: StubPosition,
  Location: StubLocation,
  Diagnostic: StubDiagnostic,
  DiagnosticRelatedInformation: StubDiagnosticRelatedInformation,
  DiagnosticSeverity: {
    Error: 0,
    Warning: 1,
    Information: 2,
    Hint: 3,
  },
  DiagnosticTag: {
    Unnecessary: 1,
    Deprecated: 2,
  },
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
  },
  window: {
    visibleTextEditors: [] as Array<{ document: { uri: StubUri } }>,
  },
};

// Load module under test with vscode stub
const { SqryDiagnosticsProvider } = proxyquire("../src/diagnosticsProvider", {
  vscode: vscodeStub,
}) as {
  SqryDiagnosticsProvider: typeof import("../src/diagnosticsProvider").SqryDiagnosticsProvider;
};

// ===== Mock SqryClient =====

function createMockClient(overrides: Record<string, unknown> = {}): any {
  return {
    listUnusedSymbols: async () => ({
      symbols: [],
      total: 0,
      truncated: false,
      scope: "all",
    }),
    listCircularDependencies: async () => ({
      cycles: [],
      total_cycles: 0,
      truncated: false,
    }),
    listDuplicateGroups: async () => ({
      groups: [],
      total_groups: 0,
      total_symbols: 0,
      truncated: false,
    }),
    ...overrides,
  };
}

function makeSearchItem(
  name: string,
  uri: string,
  startLine: number,
  startChar: number,
  endLine: number,
  endChar: number,
): any {
  return {
    name,
    kind: "function",
    qualified_name: name,
    language: "rust",
    location: {
      uri,
      range: {
        start: { line: startLine, character: startChar },
        end: { line: endLine, character: endChar },
      },
    },
  };
}

const TEST_URI = "file:///src/main.rs";
const TEST_WORKSPACE: any = {
  uri: { fsPath: "/workspace" },
  name: "test-workspace",
};

describe("SqryDiagnosticsProvider", () => {
  beforeEach(() => {
    configValues = {};
    vscodeStub.window.visibleTextEditors = [];
  });

  // ===== Unused symbols =====

  describe("unused symbols", () => {
    it("creates Hint diagnostic with Unnecessary tag", async () => {
      const collection = new MockDiagnosticCollection();
      const client = createMockClient({
        listUnusedSymbols: async () => ({
          symbols: [makeSearchItem("dead_fn", TEST_URI, 10, 0, 10, 7)],
          total: 1,
          truncated: false,
          scope: "all",
        }),
      });

      const provider = new SqryDiagnosticsProvider(
        collection as any,
        client,
        null,
      );

      await provider.refreshForFile(
        StubUri.parse(TEST_URI) as any,
        TEST_WORKSPACE,
      );

      const diags = collection.entries.get(TEST_URI);
      expect(diags).to.have.lengthOf(1);

      const d = diags![0];
      expect(d.severity).to.equal(vscodeStub.DiagnosticSeverity.Hint);
      expect(d.tags).to.deep.equal([vscodeStub.DiagnosticTag.Unnecessary]);
      expect(d.message).to.equal("'dead_fn' appears to be unused");
      expect(d.source).to.equal("sqry");
      expect(d.code).to.equal("sqry:unused");
    });

    it("filters symbols to the requested file URI", async () => {
      const collection = new MockDiagnosticCollection();
      const otherUri = "file:///src/other.rs";
      const client = createMockClient({
        listUnusedSymbols: async () => ({
          symbols: [
            makeSearchItem("fn_a", TEST_URI, 1, 0, 1, 4),
            makeSearchItem("fn_b", otherUri, 5, 0, 5, 4),
          ],
          total: 2,
          truncated: false,
          scope: "all",
        }),
      });

      const provider = new SqryDiagnosticsProvider(
        collection as any,
        client,
        null,
      );
      await provider.refreshForFile(
        StubUri.parse(TEST_URI) as any,
        TEST_WORKSPACE,
      );

      const diags = collection.entries.get(TEST_URI);
      expect(diags).to.have.lengthOf(1);
      expect(diags![0].message).to.include("fn_a");
    });
  });

  // ===== Circular dependencies =====

  describe("circular dependencies", () => {
    it("creates Information diagnostic with relatedInformation for cycle members", async () => {
      const collection = new MockDiagnosticCollection();
      const client = createMockClient({
        listCircularDependencies: async () => ({
          cycles: [
            {
              cycle_id: "abc123",
              depth: 2,
              members: ["foo", "bar"],
              cycle_type: "calls",
              member_locations: [
                { name: "foo", file: "/src/a.rs", line: 2, column: 5 },
                { name: "bar", file: "/src/b.rs", line: 4, column: 3 },
              ],
            },
          ],
          total_cycles: 1,
          truncated: false,
        }),
      });

      const provider = new SqryDiagnosticsProvider(
        collection as any,
        client,
        null,
      );
      await provider.scanWorkspace(TEST_WORKSPACE);

      // Should produce diagnostics for both members
      const fooUri = "file:///src/a.rs";
      const barUri = "file:///src/b.rs";

      const fooDiags = collection.entries.get(fooUri);
      expect(fooDiags).to.have.lengthOf(1);
      expect(fooDiags![0].severity).to.equal(
        vscodeStub.DiagnosticSeverity.Information,
      );
      expect(fooDiags![0].source).to.equal("sqry");
      expect(fooDiags![0].code).to.equal("sqry:cycle");
      expect(fooDiags![0].message).to.include("foo -> bar -> foo");
      expect(fooDiags![0].range.startLine).to.equal(2);
      expect(fooDiags![0].range.startChar).to.equal(5);
      expect(fooDiags![0].relatedInformation).to.have.lengthOf(1);
      expect(fooDiags![0].relatedInformation![0].message).to.include("bar");
      expect(fooDiags![0].relatedInformation![0].location.range.startLine).to.equal(4);
      expect(fooDiags![0].relatedInformation![0].location.range.startChar).to.equal(3);

      const barDiags = collection.entries.get(barUri);
      expect(barDiags).to.have.lengthOf(1);
      expect(barDiags![0].relatedInformation).to.have.lengthOf(1);
      expect(barDiags![0].relatedInformation![0].message).to.include("foo");
    });

    it("skips cycle members without resolved file locations", async () => {
      const collection = new MockDiagnosticCollection();
      const client = createMockClient({
        listCircularDependencies: async () => ({
          cycles: [
            {
              cycle_id: "xyz",
              depth: 2,
              members: ["a", "b"],
              cycle_type: "calls",
              member_locations: [
                { name: "a", file: "/src/a.rs", line: 2, column: null },
                { name: "b", file: "/src/b.rs", line: 2, column: 5 },
              ],
            },
          ],
          total_cycles: 1,
          truncated: false,
        }),
      });

      const provider = new SqryDiagnosticsProvider(
        collection as any,
        client,
        null,
      );
      await provider.scanWorkspace(TEST_WORKSPACE);

      // Only 'a' should have a diagnostic
      const aUri = "file:///src/a.rs";
      const aDiags = collection.entries.get(aUri);
      expect(aDiags).to.have.lengthOf(1);
      expect(aDiags![0].range.startLine).to.equal(2);
      expect(aDiags![0].range.startChar).to.equal(0);

      expect(aDiags![0].relatedInformation).to.have.lengthOf(1);
      expect(aDiags![0].relatedInformation![0].location.range.startLine).to.equal(2);
      expect(aDiags![0].relatedInformation![0].location.range.startChar).to.equal(5);
    });
  });

  // ===== Duplicate code =====

  describe("duplicate code", () => {
    it("creates Information diagnostic with relatedInformation pointing to duplicates", async () => {
      const collection = new MockDiagnosticCollection();
      const uri1 = "file:///src/mod_a.rs";
      const uri2 = "file:///src/mod_b.rs";

      const client = createMockClient({
        listDuplicateGroups: async () => ({
          groups: [
            {
              group_id: "hash1",
              count: 2,
              representative_name: "process_data",
              symbols: [
                makeSearchItem("process_data", uri1, 5, 0, 15, 1),
                makeSearchItem("process_data", uri2, 10, 0, 20, 1),
              ],
            },
          ],
          total_groups: 1,
          total_symbols: 2,
          truncated: false,
        }),
      });

      const provider = new SqryDiagnosticsProvider(
        collection as any,
        client,
        null,
      );
      await provider.scanWorkspace(TEST_WORKSPACE);

      const diags1 = collection.entries.get(uri1);
      expect(diags1).to.have.lengthOf(1);
      expect(diags1![0].severity).to.equal(
        vscodeStub.DiagnosticSeverity.Information,
      );
      expect(diags1![0].source).to.equal("sqry");
      expect(diags1![0].code).to.equal("sqry:duplicate");
      expect(diags1![0].message).to.include("process_data");
      expect(diags1![0].message).to.include("2 copies");
      expect(diags1![0].relatedInformation).to.have.lengthOf(1);

      const diags2 = collection.entries.get(uri2);
      expect(diags2).to.have.lengthOf(1);
      expect(diags2![0].relatedInformation).to.have.lengthOf(1);
    });
  });

  // ===== Caps =====

  describe("per-file cap", () => {
    it("truncates at 500 and adds summary diagnostic", async () => {
      const collection = new MockDiagnosticCollection();
      const symbols: ReturnType<typeof makeSearchItem>[] = [];
      for (let i = 0; i < 550; i++) {
        symbols.push(makeSearchItem(`fn_${i}`, TEST_URI, i, 0, i, 5));
      }

      const client = createMockClient({
        listUnusedSymbols: async () => ({
          symbols,
          total: 550,
          truncated: false,
          scope: "all",
        }),
      });

      const provider = new SqryDiagnosticsProvider(
        collection as any,
        client,
        null,
      );
      await provider.refreshForFile(
        StubUri.parse(TEST_URI) as any,
        TEST_WORKSPACE,
      );

      const diags = collection.entries.get(TEST_URI);
      // 500 real + 1 summary = 501
      expect(diags).to.have.lengthOf(501);

      const summary = diags![500];
      expect(summary.message).to.include("showing 500 of 550");
      expect(summary.source).to.equal("sqry");
    });
  });

  // ===== Clear =====

  describe("clear", () => {
    it("removes all diagnostics", async () => {
      const collection = new MockDiagnosticCollection();
      const client = createMockClient({
        listUnusedSymbols: async () => ({
          symbols: [makeSearchItem("x", TEST_URI, 0, 0, 0, 1)],
          total: 1,
          truncated: false,
          scope: "all",
        }),
      });

      const provider = new SqryDiagnosticsProvider(
        collection as any,
        client,
        null,
      );

      await provider.refreshForFile(
        StubUri.parse(TEST_URI) as any,
        TEST_WORKSPACE,
      );
      expect(collection.entries.size).to.be.greaterThan(0);

      provider.clear();
      expect(collection.entries.size).to.equal(0);
    });

    it("clearFile removes only that file's diagnostics", async () => {
      const collection = new MockDiagnosticCollection();
      const uri2 = "file:///src/other.rs";

      const client = createMockClient({
        listUnusedSymbols: async () => ({
          symbols: [
            makeSearchItem("a", TEST_URI, 0, 0, 0, 1),
            makeSearchItem("b", uri2, 0, 0, 0, 1),
          ],
          total: 2,
          truncated: false,
          scope: "all",
        }),
        listCircularDependencies: async () => ({
          cycles: [],
          total_cycles: 0,
          truncated: false,
        }),
        listDuplicateGroups: async () => ({
          groups: [],
          total_groups: 0,
          total_symbols: 0,
          truncated: false,
        }),
      });

      const provider = new SqryDiagnosticsProvider(
        collection as any,
        client,
        null,
      );
      await provider.scanWorkspace(TEST_WORKSPACE);
      expect(collection.entries.size).to.equal(2);

      provider.clearFile(StubUri.parse(TEST_URI) as any);
      expect(collection.entries.has(TEST_URI)).to.be.false;
      expect(collection.entries.has(uri2)).to.be.true;
    });
  });

  // ===== Settings =====

  describe("settings", () => {
    it("does nothing when diagnostics.enabled is false", async () => {
      configValues["sqry.diagnostics.enabled"] = false;
      const collection = new MockDiagnosticCollection();
      const client = createMockClient({
        listUnusedSymbols: async () => {
          throw new Error("should not be called");
        },
      });

      const provider = new SqryDiagnosticsProvider(
        collection as any,
        client,
        null,
      );
      await provider.refreshForFile(
        StubUri.parse(TEST_URI) as any,
        TEST_WORKSPACE,
      );

      expect(collection.entries.size).to.equal(0);
    });

    it("skips unused code when diagnostics.unusedCode is false", async () => {
      configValues["sqry.diagnostics.unusedCode"] = false;
      const collection = new MockDiagnosticCollection();
      const client = createMockClient({
        listUnusedSymbols: async () => {
          throw new Error("should not be called");
        },
      });

      const provider = new SqryDiagnosticsProvider(
        collection as any,
        client,
        null,
      );
      await provider.refreshForFile(
        StubUri.parse(TEST_URI) as any,
        TEST_WORKSPACE,
      );

      expect(collection.entries.size).to.equal(0);
    });
  });

  // ===== Dispose =====

  describe("dispose", () => {
    it("disposes the diagnostic collection", () => {
      const collection = new MockDiagnosticCollection();
      const client = createMockClient();
      const provider = new SqryDiagnosticsProvider(
        collection as any,
        client,
        null,
      );

      provider.dispose();
      expect(collection.disposed).to.be.true;
    });
  });

  // ===== refreshForOpenEditors =====

  describe("refreshForOpenEditors", () => {
    it("publishes diagnostics only for open editor URIs", async () => {
      const collection = new MockDiagnosticCollection();
      const openUri = "file:///src/open.rs";
      const closedUri = "file:///src/closed.rs";

      vscodeStub.window.visibleTextEditors = [
        { document: { uri: StubUri.parse(openUri) } },
      ];

      const client = createMockClient({
        listUnusedSymbols: async () => ({
          symbols: [
            makeSearchItem("open_fn", openUri, 0, 0, 0, 7),
            makeSearchItem("closed_fn", closedUri, 0, 0, 0, 9),
          ],
          total: 2,
          truncated: false,
          scope: "all",
        }),
      });

      const provider = new SqryDiagnosticsProvider(
        collection as any,
        client,
        null,
      );
      await provider.refreshForOpenEditors(TEST_WORKSPACE);

      expect(collection.entries.has(openUri)).to.be.true;
      expect(collection.entries.has(closedUri)).to.be.false;
    });
  });
});
