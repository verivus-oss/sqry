import { expect } from "chai";
import proxyquireModule from "proxyquire";

const proxyquire = proxyquireModule.noCallThru();

// ===== VS Code Stubs =====

class StubRange {
  public readonly start: { line: number; character: number };
  public readonly end: { line: number; character: number };
  constructor(
    public startLine: number,
    public startChar: number,
    public endLine: number,
    public endChar: number,
  ) {
    this.start = { line: startLine, character: startChar };
    this.end = { line: endLine, character: endChar };
  }
}

class StubUri {
  constructor(
    public readonly scheme: string,
    private readonly value: string,
    public readonly fsPath: string,
  ) {}
  toString(): string {
    return this.value;
  }
  static file(s: string): StubUri {
    return new StubUri("file", `file://${s}`, s);
  }
  static withScheme(scheme: string, s: string): StubUri {
    return new StubUri(scheme, `${scheme}://${s}`, s);
  }
}

class StubLocation {
  constructor(
    public uri: StubUri,
    public range: StubRange,
  ) {}
}

class StubDiagnosticRelatedInformation {
  constructor(
    public location: StubLocation,
    public message: string,
  ) {}
}

class StubDiagnostic {
  public source?: string;
  public code?: string;
  public relatedInformation?: StubDiagnosticRelatedInformation[];

  constructor(
    public range: StubRange,
    public message: string,
    public severity: number,
  ) {}
}

class StubCodeAction {
  public command?: { command: string; title: string; arguments?: unknown[] };
  public diagnostics?: StubDiagnostic[];

  constructor(
    public title: string,
    public kind: { value: string },
  ) {}
}

const StubCodeActionKind = {
  QuickFix: { value: "quickfix" },
  RefactorRewrite: { value: "refactor.rewrite" },
};

const DiagnosticSeverity = {
  Error: 0,
  Warning: 1,
  Information: 2,
  Hint: 3,
};

const vscodeStub = {
  __esModule: true,
  CodeActionKind: StubCodeActionKind,
  CodeAction: StubCodeAction,
  Diagnostic: StubDiagnostic,
  DiagnosticSeverity,
};

// Load the module with the vscode stub
const { SqryCodeActionProvider } = proxyquire("../src/codeActions", {
  vscode: vscodeStub,
}) as { SqryCodeActionProvider: typeof import("../src/codeActions").SqryCodeActionProvider };

function makeRange(): StubRange {
  return new StubRange(0, 0, 0, 10);
}

function makeContext(diagnostics: StubDiagnostic[]): { diagnostics: StubDiagnostic[] } {
  return { diagnostics };
}

describe("SqryCodeActionProvider", () => {
  let provider: InstanceType<typeof SqryCodeActionProvider>;

  beforeEach(() => {
    provider = new SqryCodeActionProvider();
  });

  it("providedCodeActionKinds includes QuickFix", () => {
    expect(SqryCodeActionProvider.providedCodeActionKinds).to.deep.equal([
      StubCodeActionKind.QuickFix,
    ]);
  });

  it("sqry:unused diagnostic produces 'Show callers in sqry' action with QuickFix kind", () => {
    const diag = new StubDiagnostic(makeRange(), "'myFunction' appears to be unused", DiagnosticSeverity.Warning);
    diag.source = "sqry";
    diag.code = "sqry:unused";

    const actions = provider.provideCodeActions(
      {} as any,
      makeRange() as any,
      makeContext([diag]) as any,
    );

    expect(actions).to.have.length(1);
    const action = actions[0] as StubCodeAction;
    expect(action.title).to.equal("Show callers of 'myFunction' in sqry");
    expect(action.kind).to.deep.equal(StubCodeActionKind.QuickFix);
    expect(action.command?.command).to.equal("sqry.runQueryInternal");
    expect(action.command?.arguments).to.deep.equal(["callers:myFunction"]);
    expect(action.diagnostics).to.deep.equal([diag]);
  });

  it("sqry:cycle diagnostic produces 'Show cycle path' action", () => {
    const diag = new StubDiagnostic(
      makeRange(),
      "circular dependency: moduleA -> moduleB -> moduleA",
      DiagnosticSeverity.Error,
    );
    diag.source = "sqry";
    diag.code = "sqry:cycle";

    const actions = provider.provideCodeActions(
      {} as any,
      makeRange() as any,
      makeContext([diag]) as any,
    );

    expect(actions).to.have.length(1);
    const action = actions[0] as StubCodeAction;
    expect(action.title).to.equal("Show cycle path in sqry");
    expect(action.kind).to.deep.equal(StubCodeActionKind.QuickFix);
    expect(action.command?.command).to.equal("sqry.runQueryInternal");
    expect(action.command?.arguments).to.deep.equal(["moduleA -> moduleB -> moduleA"]);
    expect(action.diagnostics).to.deep.equal([diag]);
  });

  it("sqry:cycle action is created even without cycleMatch in message", () => {
    const diag = new StubDiagnostic(makeRange(), "cycle detected", DiagnosticSeverity.Error);
    diag.source = "sqry";
    diag.code = "sqry:cycle";

    const actions = provider.provideCodeActions(
      {} as any,
      makeRange() as any,
      makeContext([diag]) as any,
    );

    expect(actions).to.have.length(1);
    const action = actions[0] as StubCodeAction;
    expect(action.title).to.equal("Show cycle path in sqry");
    // No command set when cycleMatch fails
    expect(action.command).to.be.undefined;
  });

  it("sqry:duplicate diagnostic produces 'Navigate to duplicate' action", () => {
    const relatedUri = StubUri.file("/path/to/duplicate.ts");
    const relatedRange = new StubRange(5, 0, 5, 20);
    const related = new StubDiagnosticRelatedInformation(
      new StubLocation(relatedUri, relatedRange),
      "duplicate symbol here",
    );

    const diag = new StubDiagnostic(makeRange(), "duplicate symbol: myClass", DiagnosticSeverity.Warning);
    diag.source = "sqry";
    diag.code = "sqry:duplicate";
    diag.relatedInformation = [related];

    const actions = provider.provideCodeActions(
      {} as any,
      makeRange() as any,
      makeContext([diag]) as any,
    );

    expect(actions).to.have.length(1);
    const action = actions[0] as StubCodeAction;
    expect(action.title).to.equal("Navigate to duplicate");
    expect(action.kind).to.deep.equal(StubCodeActionKind.QuickFix);
    // Routed through the workspace-containment guard, not raw vscode.open.
    expect(action.command?.command).to.equal("sqry.openResultFile");
    expect((action.command?.arguments as unknown[])?.[0]).to.equal(relatedUri.fsPath);
    expect((action.command?.arguments as unknown[])?.[1]).to.deep.equal({
      startLine: relatedRange.start.line,
      startCharacter: relatedRange.start.character,
      endLine: relatedRange.end.line,
      endCharacter: relatedRange.end.character,
    });
    expect(action.diagnostics).to.deep.equal([diag]);
  });

  it("sqry:duplicate offers no navigate action for a non-file related URI", () => {
    const relatedUri = StubUri.withScheme("untitled", "/path/to/duplicate.ts");
    const relatedRange = new StubRange(5, 0, 5, 20);
    const related = new StubDiagnosticRelatedInformation(
      new StubLocation(relatedUri, relatedRange),
      "duplicate symbol here",
    );

    const diag = new StubDiagnostic(makeRange(), "duplicate symbol: myClass", DiagnosticSeverity.Warning);
    diag.source = "sqry";
    diag.code = "sqry:duplicate";
    diag.relatedInformation = [related];

    const actions = provider.provideCodeActions(
      {} as any,
      makeRange() as any,
      makeContext([diag]) as any,
    );

    // A non-file scheme is refused at the source, so no navigate action is offered.
    expect(actions).to.have.length(0);
  });

  it("sqry:duplicate produces no action when relatedInformation is absent", () => {
    const diag = new StubDiagnostic(makeRange(), "duplicate symbol: myClass", DiagnosticSeverity.Warning);
    diag.source = "sqry";
    diag.code = "sqry:duplicate";
    // no relatedInformation

    const actions = provider.provideCodeActions(
      {} as any,
      makeRange() as any,
      makeContext([diag]) as any,
    );

    expect(actions).to.have.length(0);
  });

  it("non-sqry diagnostics produce no actions", () => {
    const diag = new StubDiagnostic(makeRange(), "'foo' is unused", DiagnosticSeverity.Warning);
    diag.source = "typescript";
    diag.code = "sqry:unused";

    const actions = provider.provideCodeActions(
      {} as any,
      makeRange() as any,
      makeContext([diag]) as any,
    );

    expect(actions).to.have.length(0);
  });

  it("diagnostics with no source produce no actions", () => {
    const diag = new StubDiagnostic(makeRange(), "'bar' appears to be unused", DiagnosticSeverity.Warning);
    diag.code = "sqry:unused";
    // no source set

    const actions = provider.provideCodeActions(
      {} as any,
      makeRange() as any,
      makeContext([diag]) as any,
    );

    expect(actions).to.have.length(0);
  });

  it("empty diagnostics context produces no actions", () => {
    const actions = provider.provideCodeActions(
      {} as any,
      makeRange() as any,
      makeContext([]) as any,
    );

    expect(actions).to.have.length(0);
  });

  it("all actions have CodeActionKind.QuickFix", () => {
    const unusedDiag = new StubDiagnostic(makeRange(), "'fn1' appears to be unused", DiagnosticSeverity.Warning);
    unusedDiag.source = "sqry";
    unusedDiag.code = "sqry:unused";

    const cycleDiag = new StubDiagnostic(makeRange(), "circular dependency: A -> B -> A", DiagnosticSeverity.Error);
    cycleDiag.source = "sqry";
    cycleDiag.code = "sqry:cycle";

    const relatedUri = StubUri.file("/dup.ts");
    const dupDiag = new StubDiagnostic(makeRange(), "duplicate symbol", DiagnosticSeverity.Warning);
    dupDiag.source = "sqry";
    dupDiag.code = "sqry:duplicate";
    dupDiag.relatedInformation = [
      new StubDiagnosticRelatedInformation(
        new StubLocation(relatedUri, makeRange()),
        "duplicate here",
      ),
    ];

    const actions = provider.provideCodeActions(
      {} as any,
      makeRange() as any,
      makeContext([unusedDiag, cycleDiag, dupDiag]) as any,
    );

    expect(actions).to.have.length(3);
    for (const action of actions as StubCodeAction[]) {
      expect(action.kind).to.deep.equal(StubCodeActionKind.QuickFix);
    }
  });
});
