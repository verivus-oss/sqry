import { expect } from "chai";
import {
  InvalidTransitionError,
  LoadingPhase,
  LoadingStateMachine,
  MANUAL_GATE_TIMEOUT_MS,
} from "../src/loadingState";

describe("LoadingStateMachine — initial state", () => {
  it("starts in `Activating`", () => {
    const m = new LoadingStateMachine();
    expect(m.phase).to.equal("Activating");
    expect(m.isLoading()).to.equal(true);
    expect(m.isReady()).to.equal(false);
    expect(m.isFailed()).to.equal(false);
    m.dispose();
  });

  it("MANUAL_GATE_TIMEOUT_MS is the DAG-mandated 30s", () => {
    expect(MANUAL_GATE_TIMEOUT_MS).to.equal(30_000);
  });
});

describe("LoadingStateMachine — happy-path transitions", () => {
  it("walks Activating -> LspStarting -> WorkspaceResolving -> Ready", () => {
    const m = new LoadingStateMachine();
    const trail: LoadingPhase[] = [];
    m.onDidChangePhase((phase) => trail.push(phase));
    m.transition("LspStarting");
    m.transition("WorkspaceResolving");
    m.transition("Ready");
    expect(trail).to.deep.equal(["LspStarting", "WorkspaceResolving", "Ready"]);
    expect(m.phase).to.equal("Ready");
    expect(m.isReady()).to.equal(true);
    expect(m.isLoading()).to.equal(false);
    m.dispose();
  });

  it("Ready can reset to WorkspaceResolving (re-queue on workspace change)", () => {
    const m = new LoadingStateMachine();
    m.transition("LspStarting");
    m.transition("WorkspaceResolving");
    m.transition("Ready");
    m.transition("WorkspaceResolving");
    expect(m.phase).to.equal("WorkspaceResolving");
    m.transition("Ready");
    expect(m.phase).to.equal("Ready");
    m.dispose();
  });
});

describe("LoadingStateMachine — failure path", () => {
  it("any non-terminal phase can transition to Failed", () => {
    const cases: LoadingPhase[] = ["Activating", "LspStarting", "WorkspaceResolving"];
    for (const start of cases) {
      const m = new LoadingStateMachine();
      // walk to `start`
      if (start === "LspStarting") m.transition("LspStarting");
      if (start === "WorkspaceResolving") {
        m.transition("LspStarting");
        m.transition("WorkspaceResolving");
      }
      m.transition("Failed", { reason: "boom", viewLogsAction: true });
      expect(m.phase, `from ${start}`).to.equal("Failed");
      expect(m.isFailed()).to.equal(true);
      expect(m.failure?.reason).to.equal("boom");
      m.dispose();
    }
  });

  it("Failed is terminal — no further transitions allowed", () => {
    const m = new LoadingStateMachine();
    m.transition("Failed", { reason: "x", viewLogsAction: false });
    expect(() => m.transition("Ready")).to.throw(InvalidTransitionError);
    expect(() => m.transition("LspStarting")).to.throw(InvalidTransitionError);
    m.dispose();
  });

  it("default Failed details are set when none supplied", () => {
    const m = new LoadingStateMachine();
    m.transition("Failed");
    expect(m.failure?.reason).to.match(/initialization failed/i);
    expect(m.failure?.viewLogsAction).to.equal(true);
    m.dispose();
  });
});

describe("LoadingStateMachine — invalid transitions", () => {
  it("rejects skipping LspStarting", () => {
    const m = new LoadingStateMachine();
    expect(() => m.transition("WorkspaceResolving")).to.throw(InvalidTransitionError);
    expect(() => m.transition("Ready")).to.throw(InvalidTransitionError);
    m.dispose();
  });

  it("rejects backwards transitions", () => {
    const m = new LoadingStateMachine();
    m.transition("LspStarting");
    expect(() => m.transition("Activating")).to.throw(InvalidTransitionError);
    m.dispose();
  });
});

describe("LoadingStateMachine — waitForReady gate", () => {
  it("resolves immediately when already Ready", async () => {
    const m = new LoadingStateMachine();
    m.transition("LspStarting");
    m.transition("WorkspaceResolving");
    m.transition("Ready");
    await m.waitForReady(100);
    m.dispose();
  });

  it("rejects immediately when already Failed", async () => {
    const m = new LoadingStateMachine();
    m.transition("Failed", { reason: "down", viewLogsAction: true });
    let caught: Error | null = null;
    try {
      await m.waitForReady(100);
    } catch (e) {
      caught = e as Error;
    }
    expect(caught?.message).to.equal("down");
    m.dispose();
  });

  it("resolves when the next Ready transition occurs", async () => {
    const m = new LoadingStateMachine();
    m.transition("LspStarting");
    m.transition("WorkspaceResolving");
    const p = m.waitForReady(500);
    setTimeout(() => m.transition("Ready"), 10);
    await p;
    expect(m.phase).to.equal("Ready");
    m.dispose();
  });

  it("rejects with timeout error after the timeout elapses", async () => {
    const m = new LoadingStateMachine();
    m.transition("LspStarting");
    let caught: Error | null = null;
    try {
      await m.waitForReady(20);
    } catch (e) {
      caught = e as Error;
    }
    expect(caught?.message).to.match(/timed out/i);
    m.dispose();
  });

  it("rejects pending waiters when the machine transitions to Failed", async () => {
    const m = new LoadingStateMachine();
    m.transition("LspStarting");
    const p = m.waitForReady(500);
    setTimeout(() => m.transition("Failed", { reason: "lsp died", viewLogsAction: true }), 10);
    let caught: Error | null = null;
    try {
      await p;
    } catch (e) {
      caught = e as Error;
    }
    expect(caught?.message).to.equal("lsp died");
    m.dispose();
  });
});

describe("LoadingStateMachine — listener lifecycle", () => {
  it("disposable removes listeners", () => {
    const m = new LoadingStateMachine();
    let calls = 0;
    const sub = m.onDidChangePhase(() => {
      calls += 1;
    });
    m.transition("LspStarting");
    sub.dispose();
    m.transition("WorkspaceResolving");
    expect(calls).to.equal(1);
    m.dispose();
  });

  it("dispose() rejects pending waiters", async () => {
    const m = new LoadingStateMachine();
    m.transition("LspStarting");
    const p = m.waitForReady(500);
    m.dispose();
    let caught: Error | null = null;
    try {
      await p;
    } catch (e) {
      caught = e as Error;
    }
    expect(caught?.message).to.match(/deactivated/i);
  });

  it("listener errors do not corrupt the state machine", () => {
    const m = new LoadingStateMachine();
    m.onDidChangePhase(() => {
      throw new Error("listener crash");
    });
    expect(() => m.transition("LspStarting")).to.not.throw();
    expect(m.phase).to.equal("LspStarting");
    m.dispose();
  });
});
