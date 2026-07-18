import { expect } from "chai";
import { LoadingPhase, LoadingStateMachine } from "../src/loadingState";
import {
  completeInitialWorkspaceResolution,
  deactivateStartupResources,
  disposeStartupClient,
} from "../src/startupLifecycle";
import type { SqryClient } from "../src/sqryClient";

function deferred<T>(): { promise: Promise<T>; resolve(value: T): void } {
  let resolvePromise: ((value: T) => void) | undefined;
  const promise = new Promise<T>((resolve) => {
    resolvePromise = resolve;
  });
  return {
    promise,
    resolve: (value) => resolvePromise?.(value),
  };
}

function readyStateMachine(): LoadingStateMachine {
  const loadingState = new LoadingStateMachine();
  loadingState.transition("LspStarting");
  loadingState.transition("WorkspaceResolving");
  return loadingState;
}

function fakeClient(): SqryClient {
  return {} as SqryClient;
}

describe("startupLifecycle — initial workspace resolution", () => {
  it("disposes a terminal startup client synchronously", () => {
    let disposeCalls = 0;

    disposeStartupClient({
      dispose: () => {
        disposeCalls += 1;
      },
    });

    expect(disposeCalls).to.equal(1);
  });

  it("disposes the client before awaiting a stalled readiness-context reset", async () => {
    const closeGate = deferred<{ readonly ok: true }>();
    const events: string[] = [];
    let deactivationFinished = false;
    const deactivation = deactivateStartupResources({
      activeClient: {
        dispose: () => {
          events.push("client.dispose");
        },
      },
      readinessContextBinding: {
        close: () => {
          events.push("context.close");
          return closeGate.promise;
        },
        dispose: () => undefined,
      } as unknown as import("../src/startupCommands").ReadinessContextBinding,
    });
    void deactivation.then(() => {
      deactivationFinished = true;
    });

    expect(events).to.deep.equal(["client.dispose", "context.close"]);
    await Promise.resolve();
    expect(deactivationFinished).to.equal(false);

    closeGate.resolve({ ok: true });
    await deactivation;
    expect(deactivationFinished).to.equal(true);
  });

  it("reaches Ready before scheduling best-effort telemetry", async () => {
    const loadingState = readyStateMachine();
    const events: string[] = [];
    loadingState.onDidChangePhase((phase: LoadingPhase) => events.push(phase));
    let autoIndexCalls = 0;

    const completed = await completeInitialWorkspaceResolution({
      activeClient: fakeClient(),
      loadingState,
      refreshWorkspaceStatus: async () => ({ ok: true }),
      emitTelemetry: async () => {
        events.push("telemetry");
      },
      maybeAutoIndex: async () => {
        autoIndexCalls += 1;
      },
      log: (line) => events.push(line),
    });

    await Promise.resolve();
    expect(completed).to.equal(true);
    expect(loadingState.isReady()).to.equal(true);
    expect(events.indexOf("Ready")).to.be.lessThan(events.indexOf("telemetry"));
    expect(autoIndexCalls).to.equal(1);
    loadingState.dispose();
  });

  it("does not reach Ready, run telemetry, or auto-index after aggregate failure", async () => {
    const loadingState = readyStateMachine();
    let telemetryCalls = 0;
    let autoIndexCalls = 0;

    const completed = await completeInitialWorkspaceResolution({
      activeClient: fakeClient(),
      loadingState,
      refreshWorkspaceStatus: async () => ({ ok: false, error: new Error("status deadline") }),
      emitTelemetry: async () => {
        telemetryCalls += 1;
      },
      maybeAutoIndex: async () => {
        autoIndexCalls += 1;
      },
      log: () => undefined,
    });

    await Promise.resolve();
    expect(completed).to.equal(false);
    expect(loadingState.isFailed()).to.equal(true);
    expect(loadingState.failure?.reason).to.include("status deadline");
    expect(loadingState.failure?.viewLogsAction).to.equal(true);
    expect(telemetryCalls).to.equal(0);
    expect(autoIndexCalls).to.equal(0);
    loadingState.dispose();
  });

  it("turns an unexpected post-initialization exception into visible terminal failure", async () => {
    const loadingState = readyStateMachine();
    const lines: string[] = [];

    const completed = await completeInitialWorkspaceResolution({
      activeClient: fakeClient(),
      loadingState,
      refreshWorkspaceStatus: async () => ({ ok: true }),
      emitTelemetry: async () => undefined,
      maybeAutoIndex: async () => {
        throw new Error("auto-index setup crashed");
      },
      log: (line) => lines.push(line),
    });

    expect(completed).to.equal(false);
    expect(loadingState.isFailed()).to.equal(true);
    expect(loadingState.failure?.reason).to.include("auto-index setup crashed");
    expect(lines.join("\n")).to.include("startup did not complete");
    loadingState.dispose();
  });

  it("does not await a never-settling telemetry operation", async () => {
    const loadingState = readyStateMachine();
    let autoIndexCalls = 0;

    const completed = await Promise.race([
      completeInitialWorkspaceResolution({
        activeClient: fakeClient(),
        loadingState,
        refreshWorkspaceStatus: async () => ({ ok: true }),
        emitTelemetry: () => new Promise<void>(() => undefined),
        maybeAutoIndex: async () => {
          autoIndexCalls += 1;
        },
        log: () => undefined,
      }),
      new Promise<boolean>((_resolve, reject) => {
        setTimeout(() => reject(new Error("startup waited for telemetry")), 100);
      }),
    ]);

    expect(completed).to.equal(true);
    expect(loadingState.isReady()).to.equal(true);
    expect(autoIndexCalls).to.equal(1);
    loadingState.dispose();
  });

  it("logs telemetry rejection without leaving Ready", async () => {
    const loadingState = readyStateMachine();
    const lines: string[] = [];

    await completeInitialWorkspaceResolution({
      activeClient: fakeClient(),
      loadingState,
      refreshWorkspaceStatus: async () => ({ ok: true }),
      emitTelemetry: async () => {
        throw new Error("telemetry transport failed");
      },
      maybeAutoIndex: async () => undefined,
      log: (line) => lines.push(line),
    });

    await Promise.resolve();
    await Promise.resolve();
    expect(loadingState.isReady()).to.equal(true);
    expect(lines.join("\n")).to.include("telemetry transport failed");
    loadingState.dispose();
  });
});
