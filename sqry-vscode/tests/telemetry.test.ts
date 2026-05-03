/**
 * STEP_12 — extension startup-line telemetry tests.
 *
 * Acceptance contract from the DAG (STEP_12_TELEMETRY):
 *
 *   "Extension emits one aggregate outputChannel line per startup:
 *    `[sqry] Resolved workspace <workspace_id_short> with N source
 *    roots, M members, K exclusions`."
 *
 *   "Extension emits exactly ONE outputChannel line per startup;
 *    format matches spec verbatim"
 *
 * The line is emitted by `formatWorkspaceResolutionTelemetry` in
 * `src/workspaceTelemetry.ts`. The formatter lives in its own
 * vscode-free module so the test does not need to stub the extension
 * host or proxy every transitive `vscode` import. The activation site
 * in `extension.ts` calls this exact function once at startup.
 */
import * as fs from "node:fs";
import * as path from "node:path";
import { expect } from "chai";

import {
  emitWorkspaceResolutionTelemetry,
  formatWorkspaceResolutionTelemetry,
  type OutputSink,
  type WorkspaceInfoSupplier,
} from "../src/workspaceTelemetry";

/**
 * Recording fakes used by the iter1 MINOR fix tests below.
 *
 * The fakes count `getLogicalWorkspaceInfo()` and `appendLine()`
 * invocations so the "exactly ONE outputChannel line per startup"
 * acceptance criterion (#4) is verified end-to-end against the same
 * helper that `extension.ts` activation calls.
 */
class RecordingSupplier implements WorkspaceInfoSupplier {
  public readonly calls: Array<unknown> = [];
  constructor(
    private readonly behaviour:
      | {
          kind: "ok";
          workspace_id_short: string;
          sourceRoots: number;
          members: number;
          exclusions: number;
        }
      | { kind: "throw"; error: Error },
  ) {}
  async getLogicalWorkspaceInfo(): Promise<{
    readonly workspace_id_short: string;
    readonly source_roots: ReadonlyArray<unknown>;
    readonly member_folders: ReadonlyArray<unknown>;
    readonly exclusions: ReadonlyArray<unknown>;
  }> {
    this.calls.push({});
    if (this.behaviour.kind === "throw") {
      throw this.behaviour.error;
    }
    return {
      workspace_id_short: this.behaviour.workspace_id_short,
      source_roots: new Array(this.behaviour.sourceRoots).fill(null),
      member_folders: new Array(this.behaviour.members).fill(null),
      exclusions: new Array(this.behaviour.exclusions).fill(null),
    };
  }
}

class RecordingSink implements OutputSink {
  public readonly lines: Array<string> = [];
  appendLine(message: string): void {
    this.lines.push(message);
  }
}

const TELEMETRY_REGEX =
  /^\[sqry\] Resolved workspace [a-f0-9]+ with \d+ source roots?, \d+ members?, \d+ exclusions?$/;

describe("STEP_12 — formatWorkspaceResolutionTelemetry", () => {
  it("emits the verbatim DAG-spec format", () => {
    const line = formatWorkspaceResolutionTelemetry({
      workspaceIdShort: "abcdef0123456789",
      sourceRootCount: 2,
      memberCount: 3,
      exclusionCount: 1,
    });
    expect(line).to.equal(
      "[sqry] Resolved workspace abcdef0123456789 with 2 source roots, 3 members, 1 exclusions",
    );
    expect(line).to.match(TELEMETRY_REGEX);
  });

  it("uses literal plural nouns for every count (machine-readable shape)", () => {
    // The DAG-spec format hard-codes the literal plural forms ("source
    // roots", "members", "exclusions") so machine consumers can rely on
    // a fixed wire shape regardless of count. The regex above is
    // tolerant of an optional trailing `s` so a future grammar polish
    // would still pass — but the formatter ITSELF intentionally keeps
    // the spec-verbatim plural form. This test pins that contract.
    const line = formatWorkspaceResolutionTelemetry({
      workspaceIdShort: "0011223344556677",
      sourceRootCount: 1,
      memberCount: 1,
      exclusionCount: 1,
    });
    expect(line).to.equal(
      "[sqry] Resolved workspace 0011223344556677 with 1 source roots, 1 members, 1 exclusions",
    );
    expect(line).to.match(TELEMETRY_REGEX);
  });

  it("formats zero-count buckets as plain numerals (no special phrasing)", () => {
    const line = formatWorkspaceResolutionTelemetry({
      workspaceIdShort: "deadbeefcafef00d",
      sourceRootCount: 0,
      memberCount: 0,
      exclusionCount: 0,
    });
    expect(line).to.equal(
      "[sqry] Resolved workspace deadbeefcafef00d with 0 source roots, 0 members, 0 exclusions",
    );
    expect(line).to.match(TELEMETRY_REGEX);
  });

  it("uses the short hex (16 chars) for the identity token, not the full hex", () => {
    // The DAG explicitly says scripts consuming JSON should key on
    // workspace_id_full to avoid the remote possibility of short-hex
    // collisions. The outputChannel line is for human eyes only, so it
    // carries the short form for scannability — full hex stays
    // reachable through `sqry/workspaceStatus.workspace_id_full` and
    // through the daemon's `daemon/status.workspace_id_full`.
    const fullHex = "a".repeat(64);
    const shortHex = fullHex.slice(0, 16);
    const line = formatWorkspaceResolutionTelemetry({
      workspaceIdShort: shortHex,
      sourceRootCount: 4,
      memberCount: 5,
      exclusionCount: 6,
    });
    expect(line).to.contain(` ${shortHex} `);
    expect(line).to.not.contain(fullHex);
  });

  it("returns a single line (no embedded newlines)", () => {
    // Per-folder spam regression guard: the formatter is a pure
    // single-line formatter. If a future drive-by ever tries to wrap a
    // per-folder loop around it, the structural contract is enforced
    // by the static-routing-gate (Step 11) at PR time + the call site
    // count in `extension.ts` (one — at the Ready transition). This
    // test pins the per-line property at the function level.
    const line = formatWorkspaceResolutionTelemetry({
      workspaceIdShort: "1122334455667788",
      sourceRootCount: 7,
      memberCount: 8,
      exclusionCount: 9,
    });
    expect(line.split("\n")).to.have.length(
      1,
      `formatter must return a single line; got ${JSON.stringify(line)}`,
    );
  });
});

// ============================================================================
// STEP_12 codex iter1 MINOR fix — pin the call-count contract for the
// shared `emitWorkspaceResolutionTelemetry` helper. The activation site
// in `extension.ts` delegates to this helper; therefore proving the
// helper invokes `getLogicalWorkspaceInfo()` exactly once and
// `appendLine()` exactly once is sufficient to prove DAG criterion #4
// ("exactly ONE outputChannel line per startup").
// ============================================================================

describe("STEP_12 — emitWorkspaceResolutionTelemetry call counts (criterion #4)", () => {
  it("calls getLogicalWorkspaceInfo() exactly once on the happy path", async () => {
    const supplier = new RecordingSupplier({
      kind: "ok",
      workspace_id_short: "0123456789abcdef",
      sourceRoots: 2,
      members: 1,
      exclusions: 0,
    });
    const sink = new RecordingSink();
    await emitWorkspaceResolutionTelemetry(supplier, sink);
    expect(supplier.calls).to.have.length(
      1,
      "supplier must be queried exactly once per startup",
    );
  });

  it("calls appendLine() exactly once on the happy path", async () => {
    const supplier = new RecordingSupplier({
      kind: "ok",
      workspace_id_short: "0123456789abcdef",
      sourceRoots: 2,
      members: 1,
      exclusions: 0,
    });
    const sink = new RecordingSink();
    await emitWorkspaceResolutionTelemetry(supplier, sink);
    expect(sink.lines).to.have.length(
      1,
      "sink must receive exactly ONE line per startup (criterion #4)",
    );
  });

  it("emits the verbatim DAG-spec line on the happy path", async () => {
    const supplier = new RecordingSupplier({
      kind: "ok",
      workspace_id_short: "0123456789abcdef",
      sourceRoots: 3,
      members: 4,
      exclusions: 5,
    });
    const sink = new RecordingSink();
    await emitWorkspaceResolutionTelemetry(supplier, sink);
    expect(sink.lines[0]).to.equal(
      "[sqry] Resolved workspace 0123456789abcdef with 3 source roots, 4 members, 5 exclusions",
    );
    expect(sink.lines[0]).to.match(TELEMETRY_REGEX);
  });

  it("calls getLogicalWorkspaceInfo() exactly once and appendLine() exactly once on supplier failure", async () => {
    // Telemetry is best-effort and must not block activation. The
    // helper still emits ONE line — a failure marker — so log readers
    // see a consistent presence/absence signal regardless of outcome.
    const supplier = new RecordingSupplier({
      kind: "throw",
      error: new Error("LSP went away"),
    });
    const sink = new RecordingSink();
    await emitWorkspaceResolutionTelemetry(supplier, sink);
    expect(supplier.calls).to.have.length(1);
    expect(sink.lines).to.have.length(
      1,
      "even on failure, exactly ONE line is appended (best-effort marker)",
    );
    expect(sink.lines[0]).to.contain(
      "[sqry] STEP_12 telemetry: failed to fetch logical workspace info",
    );
    expect(sink.lines[0]).to.contain("LSP went away");
  });

  it("never throws — telemetry must not block activation", async () => {
    const supplier = new RecordingSupplier({
      kind: "throw",
      error: new Error("network error"),
    });
    const sink = new RecordingSink();
    let threw = false;
    try {
      await emitWorkspaceResolutionTelemetry(supplier, sink);
    } catch {
      threw = true;
    }
    expect(threw).to.equal(false, "helper must swallow supplier errors");
  });

  it("does NOT loop per source root — the call count is invariant under workspace size", async () => {
    // Per-folder spam regression guard at the helper level: even with
    // many source roots, the supplier is hit ONCE and the sink
    // receives ONE line. If a future refactor ever wraps a per-root
    // loop around the helper, this test fails.
    const supplier = new RecordingSupplier({
      kind: "ok",
      workspace_id_short: "ffffffff00000000",
      sourceRoots: 25,
      members: 50,
      exclusions: 7,
    });
    const sink = new RecordingSink();
    await emitWorkspaceResolutionTelemetry(supplier, sink);
    expect(supplier.calls).to.have.length(1);
    expect(sink.lines).to.have.length(1);
    expect(sink.lines[0]).to.equal(
      "[sqry] Resolved workspace ffffffff00000000 with 25 source roots, 50 members, 7 exclusions",
    );
  });
});

describe("STEP_12 — extension.ts re-exports the shared helper", () => {
  it("`extension.ts` exports `sharedEmitWorkspaceResolutionTelemetry` and `formatWorkspaceResolutionTelemetry` from `./workspaceTelemetry`", () => {
    // Lightweight static-shape check — the activation site delegates
    // to the same helper this test exercises, so the call-count
    // contract above transitively applies to the activation path.
    // We import only the type/value shape, never the activation
    // function itself (which pulls in `vscode`).
    //
    const extensionSource = fs.readFileSync(
      path.resolve(__dirname, "../src/extension.ts"),
      "utf8",
    );
    expect(extensionSource).to.contain(
      "sharedEmitWorkspaceResolutionTelemetry",
      "extension.ts must delegate to the shared helper",
    );
    expect(extensionSource).to.contain(
      'from "./workspaceTelemetry"',
      "extension.ts must import from the workspaceTelemetry module",
    );
    // The activation site MUST invoke the helper exactly once — the
    // call site occurs at the Ready transition. The static check pins
    // "exactly one call" by counting matches of the helper invocation
    // expression. Imports + re-exports are excluded by counting the
    // open-paren form only.
    const callSiteMatches = extensionSource.match(
      /sharedEmitWorkspaceResolutionTelemetry\(/g,
    );
    expect(
      callSiteMatches?.length ?? 0,
      "exactly ONE invocation of the shared helper at the activation Ready transition",
    ).to.equal(1);
  });
});
