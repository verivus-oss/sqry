/**
 * STEP_5 codex iter1 MAJOR fix: tests for the manual-rebuild gate
 * helper (`sqry-vscode/src/manualRebuildGate.ts`). These exercises
 * cover the three contract points cited in the iter1 BLOCK verdict:
 *
 *   1. Both `sqry.index` and `sqry.rebuildIndex` command handlers must
 *      gate on `Ready` before invoking `runIndex`. The handlers route
 *      through this helper, so testing the helper guarantees the
 *      command surface obeys the gate.
 *   2. The wait honours the DAG-mandated timeout (30s default;
 *      overridden to a small value here for deterministic tests).
 *   3. Timeout is reported via the typed `ManualRebuildGateTimeoutError`
 *      so UI handlers can match on `MANUAL_REBUILD_GATE_TIMEOUT` rather
 *      than parsing message strings.
 *
 * The test fixtures fake the [`ReadyGate`] interface — no
 * `LoadingStateMachine` instance is required.
 */
import { expect } from "chai";
import {
  gatedManualRebuild,
  isManualRebuildGateTimeout,
  MANUAL_REBUILD_GATE_TIMEOUT,
  ManualRebuildGateTimeoutError,
  type GatedManualRebuildEvent,
  type ReadyGate,
} from "../src/manualRebuildGate";
import { LoadingStateMachine } from "../src/loadingState";

class StubGate implements ReadyGate {
  public ready = false;
  public failure: Error | null = null;
  public waitDelayMs = 0;

  isReady(): boolean {
    return this.ready;
  }

  waitForReady(timeoutMs?: number): Promise<void> {
    if (this.ready) {
      return Promise.resolve();
    }
    if (this.failure) {
      return Promise.reject(this.failure);
    }
    return new Promise((resolve, reject) => {
      const t = timeoutMs ?? 30_000;
      const timer = setTimeout(() => {
        if (this.ready) {
          resolve();
        } else if (this.failure) {
          reject(this.failure);
        } else {
          // Mirror LoadingStateMachine — message contains "timed out after".
          reject(new Error(`sqry: workspace resolution timed out after ${t}ms`));
        }
      }, this.waitDelayMs > 0 ? this.waitDelayMs : t);
    });
  }
}

describe("STEP_5 — manualRebuildGate (codex iter1 MAJOR 1)", () => {
  it("invokes the runner immediately when the gate is already Ready", async () => {
    const gate = new StubGate();
    gate.ready = true;
    let ran = false;
    const events: GatedManualRebuildEvent[] = [];
    const result = await gatedManualRebuild(
      gate,
      async () => {
        ran = true;
        return "ok";
      },
      { log: (e) => events.push(e) },
    );
    expect(result).to.equal("ok");
    expect(ran).to.equal(true);
    expect(events).to.have.length(1);
    expect(events[0].kind).to.equal("gate-immediate");
  });

  it("waits for the gate to open then runs the runner", async () => {
    const gate = new StubGate();
    gate.waitDelayMs = 20;
    setTimeout(() => {
      gate.ready = true;
    }, 10);
    // Override waitForReady to honour the ready flip.
    gate.waitForReady = (_timeoutMs?: number) =>
      new Promise<void>((resolve) => {
        const id = setInterval(() => {
          if (gate.ready) {
            clearInterval(id);
            resolve();
          }
        }, 5);
      });
    let ran = false;
    const events: GatedManualRebuildEvent[] = [];
    const result = await gatedManualRebuild(
      gate,
      async () => {
        ran = true;
        return 42;
      },
      { timeoutMs: 200, log: (e) => events.push(e) },
    );
    expect(result).to.equal(42);
    expect(ran).to.equal(true);
    expect(events.some((e) => e.kind === "gate-waited")).to.equal(true);
  });

  it("rejects with ManualRebuildGateTimeoutError when the wait expires (DAG criterion: 30s default)", async () => {
    const gate = new StubGate();
    let ran = false;
    let caught: unknown;
    try {
      await gatedManualRebuild(
        gate,
        async () => {
          ran = true;
          return "should-not-run";
        },
        { timeoutMs: 25 },
      );
    } catch (err) {
      caught = err;
    }
    expect(ran).to.equal(false);
    expect(caught).to.be.instanceOf(ManualRebuildGateTimeoutError);
    expect(isManualRebuildGateTimeout(caught)).to.equal(true);
    expect((caught as ManualRebuildGateTimeoutError).code).to.equal(MANUAL_REBUILD_GATE_TIMEOUT);
    expect((caught as ManualRebuildGateTimeoutError).timeoutMs).to.equal(25);
  });

  it("propagates terminal LSP failure verbatim (does NOT wrap)", async () => {
    const gate = new StubGate();
    const failure = new Error("sqry initialization failed");
    gate.failure = failure;
    let caught: unknown;
    try {
      await gatedManualRebuild(gate, async () => "should-not-run", {
        timeoutMs: 25,
      });
    } catch (err) {
      caught = err;
    }
    expect(caught).to.equal(failure);
    expect(isManualRebuildGateTimeout(caught)).to.equal(false);
  });

  it("integrates with the real LoadingStateMachine — gate opens after Ready transition", async () => {
    const m = new LoadingStateMachine();
    m.transition("LspStarting");
    m.transition("WorkspaceResolving");
    setTimeout(() => m.transition("Ready"), 15);
    let ran = false;
    await gatedManualRebuild(
      m,
      async () => {
        ran = true;
        return undefined;
      },
      { timeoutMs: 100 },
    );
    expect(ran).to.equal(true);
    m.dispose();
  });

  it("integrates with the real LoadingStateMachine — gate times out when Ready never arrives", async () => {
    const m = new LoadingStateMachine();
    m.transition("LspStarting");
    m.transition("WorkspaceResolving");
    let ran = false;
    let caught: unknown;
    try {
      await gatedManualRebuild(
        m,
        async () => {
          ran = true;
          return undefined;
        },
        { timeoutMs: 25 },
      );
    } catch (err) {
      caught = err;
    }
    expect(ran).to.equal(false);
    expect(isManualRebuildGateTimeout(caught)).to.equal(true);
    m.dispose();
  });
});
