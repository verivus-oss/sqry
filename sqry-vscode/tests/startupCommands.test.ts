import { expect } from "chai";
import * as fs from "node:fs";
import * as path from "node:path";
import proxyquireModule from "proxyquire";

const proxyquire = proxyquireModule.noCallThru();

type CommandHandler = (...args: unknown[]) => Promise<unknown>;

interface DeferredContextWrite {
  readonly args: unknown[];
  resolve(): void;
  reject(error: Error): void;
}

interface StartupCommandHarness {
  readonly module: typeof import("../src/startupCommands");
  readonly commands: Map<string, CommandHandler>;
  readonly contextCalls: unknown[][];
  readonly pendingContextWrites: DeferredContextWrite[];
  readonly appliedContextValues: boolean[];
  readonly warnings: string[];
}

interface HarnessOptions {
  readonly deferContextWrites?: boolean;
  readonly contextError?: Error;
}

function loadStartupCommandHarness(options: HarnessOptions = {}): StartupCommandHarness {
  const commands = new Map<string, CommandHandler>();
  const contextCalls: unknown[][] = [];
  const pendingContextWrites: DeferredContextWrite[] = [];
  const appliedContextValues: boolean[] = [];
  const warnings: string[] = [];
  const vscodeStub = {
    commands: {
      registerCommand: (command: string, handler: CommandHandler) => {
        commands.set(command, handler);
        return {
          dispose: () => {
            if (commands.get(command) === handler) {
              commands.delete(command);
            }
          },
        };
      },
      executeCommand: (...args: unknown[]) => {
        contextCalls.push(args);
        if (options.contextError) {
          return Promise.reject(options.contextError);
        }
        if (!options.deferContextWrites) {
          appliedContextValues.push(args[2] as boolean);
          return Promise.resolve();
        }
        return new Promise<void>((resolve, reject) => {
          pendingContextWrites.push({
            args,
            resolve: () => {
              appliedContextValues.push(args[2] as boolean);
              resolve();
            },
            reject,
          });
        });
      },
    },
    window: {
      showWarningMessage: async (message: string) => {
        warnings.push(message);
        return undefined;
      },
    },
  };

  const startupCommands = proxyquire("../src/startupCommands", {
    vscode: vscodeStub,
  }) as typeof import("../src/startupCommands");

  return {
    module: startupCommands,
    commands,
    contextCalls,
    pendingContextWrites,
    appliedContextValues,
    warnings,
  };
}

async function flushAsyncWork(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

function manifestPublicCommandIds(): string[] {
  const manifestPath = path.join(__dirname, "..", "package.json");
  const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8")) as {
    contributes: { commands: Array<{ command: string }> };
  };
  return manifest.contributes.commands
    .map((contribution) => contribution.command)
    .filter((command) => command !== "sqry.showOutput")
    .sort();
}

describe("startupCommands — permanent safe dispatch", () => {
  it("registers a dispatcher for every public manifest command", () => {
    const harness = loadStartupCommandHarness();
    const registry = harness.module.registerStartupCommands({
      getLoadingState: () => undefined,
      getOutputChannel: () => undefined,
    });

    expect([...harness.commands.keys()].sort()).to.deep.equal(manifestPublicCommandIds());
    expect([...harness.module.STARTUP_GATED_COMMAND_IDS].sort()).to.deep.equal(
      manifestPublicCommandIds(),
    );
    registry.dispose();
  });

  it("keeps every public command local and actionable while startup is incomplete", async () => {
    const harness = loadStartupCommandHarness();
    let handlerCalls = 0;
    const registry = harness.module.registerStartupCommands({
      getLoadingState: () => ({
        failure: null,
        isReady: () => false,
        isFailed: () => false,
      }),
      getOutputChannel: () => ({
        appendLine: () => undefined,
        show: () => undefined,
      }) as unknown as import("vscode").OutputChannel,
    });

    for (const command of harness.module.STARTUP_GATED_COMMAND_IDS) {
      registry.registerHandler(command, async () => {
        handlerCalls += 1;
      });
      const dispatcher = harness.commands.get(command);
      if (!dispatcher) {
        throw new Error(`missing bootstrap dispatcher for ${command}`);
      }
      await dispatcher();
    }

    expect(handlerCalls).to.equal(0);
    expect(harness.warnings).to.have.length(harness.module.STARTUP_GATED_COMMAND_IDS.length);
    expect(harness.warnings.every((message) => message.includes("still starting"))).to.equal(true);
    registry.dispose();
  });

  it("keeps every public command local after terminal startup failure", async () => {
    const harness = loadStartupCommandHarness();
    let handlerCalls = 0;
    const registry = harness.module.registerStartupCommands({
      getLoadingState: () => ({
        failure: { reason: "command readiness reset failed" },
        isReady: () => false,
        isFailed: () => true,
      }),
      getOutputChannel: () => ({
        appendLine: () => undefined,
        show: () => undefined,
      }) as unknown as import("vscode").OutputChannel,
    });

    for (const command of harness.module.STARTUP_GATED_COMMAND_IDS) {
      registry.registerHandler(command, async () => {
        handlerCalls += 1;
      });
      const dispatcher = harness.commands.get(command);
      if (!dispatcher) {
        throw new Error(`missing bootstrap dispatcher for ${command}`);
      }
      await dispatcher();
    }

    expect(handlerCalls).to.equal(0);
    expect(harness.warnings).to.have.length(harness.module.STARTUP_GATED_COMMAND_IDS.length);
    expect(harness.warnings.every((message) => message.includes("command readiness reset failed"))).to.equal(true);
    registry.dispose();
  });

  it("dispatches a registered implementation only after Ready", async () => {
    const harness = loadStartupCommandHarness();
    let isReady = false;
    let handlerCalls = 0;
    const registry = harness.module.registerStartupCommands({
      getLoadingState: () => ({
        failure: null,
        isReady: () => isReady,
        isFailed: () => false,
      }),
      getOutputChannel: () => undefined,
    });
    registry.registerHandler("sqry.refreshStats", async () => {
      handlerCalls += 1;
    });
    const dispatcher = harness.commands.get("sqry.refreshStats");
    if (!dispatcher) {
      throw new Error("missing refresh dispatcher");
    }

    await dispatcher();
    isReady = true;
    await dispatcher();

    expect(handlerCalls).to.equal(1);
    registry.dispose();
  });

  it("reports a missing implementation locally even if the host exposes a stale Ready command", async () => {
    const harness = loadStartupCommandHarness();
    const lines: string[] = [];
    const registry = harness.module.registerStartupCommands({
      getLoadingState: () => ({
        failure: null,
        isReady: () => true,
        isFailed: () => false,
      }),
      getOutputChannel: () => ({
        appendLine: (line: string) => lines.push(line),
        show: () => undefined,
      }) as unknown as import("vscode").OutputChannel,
    });
    const dispatcher = harness.commands.get("sqry.refreshStats");
    if (!dispatcher) {
      throw new Error("missing refresh dispatcher");
    }

    await dispatcher();

    expect(lines.join("\n")).to.include("not ready to run sqry.refreshStats");
    expect(harness.warnings[0]).to.include("View Logs");
    registry.dispose();
  });
});

describe("startupCommands — ordered readiness context", () => {
  it("uses the initial false write as a barrier before subscribing to phases", async () => {
    const harness = loadStartupCommandHarness({ deferContextWrites: true });
    let listener: ((phase: string) => void) | undefined;
    const bindingPromise = harness.module.bindSqryReadyContext(
      {
        onDidChangePhase: (nextListener) => {
          listener = nextListener;
          return { dispose: () => { listener = undefined; } };
        },
      },
      undefined,
      () => undefined,
    );

    await flushAsyncWork();
    expect(harness.contextCalls).to.deep.equal([["setContext", "sqry.ready", false]]);
    expect(listener).to.equal(undefined);

    harness.pendingContextWrites[0].resolve();
    const binding = await bindingPromise;
    expect(listener).to.not.equal(undefined);

    const closePromise = binding.close();
    await flushAsyncWork();
    harness.pendingContextWrites[1].resolve();
    await closePromise;
  });

  it("serializes Ready-to-Failed context writes so the applied value ends false", async () => {
    const harness = loadStartupCommandHarness({ deferContextWrites: true });
    let listener: ((phase: string) => void) | undefined;
    const bindingPromise = harness.module.bindSqryReadyContext(
      {
        onDidChangePhase: (nextListener) => {
          listener = nextListener;
          return { dispose: () => { listener = undefined; } };
        },
      },
      undefined,
      () => undefined,
    );
    await flushAsyncWork();
    harness.pendingContextWrites[0].resolve();
    const binding = await bindingPromise;
    if (!listener) {
      throw new Error("expected readiness listener");
    }
    const emitPhase = listener;

    emitPhase("Ready");
    await flushAsyncWork();
    expect(harness.contextCalls).to.deep.equal([
      ["setContext", "sqry.ready", false],
      ["setContext", "sqry.ready", true],
    ]);
    emitPhase("Failed");
    await flushAsyncWork();
    expect(harness.contextCalls).to.have.length(2);

    harness.pendingContextWrites[1].resolve();
    await flushAsyncWork();
    expect(harness.contextCalls).to.deep.equal([
      ["setContext", "sqry.ready", false],
      ["setContext", "sqry.ready", true],
      ["setContext", "sqry.ready", false],
    ]);
    harness.pendingContextWrites[2].resolve();
    await flushAsyncWork();
    expect(harness.appliedContextValues).to.deep.equal([false, true, false]);

    const closePromise = binding.close();
    await flushAsyncWork();
    harness.pendingContextWrites[3].resolve();
    await closePromise;
  });

  it("fails closed when the strict initial false write rejects", async () => {
    const harness = loadStartupCommandHarness({ deferContextWrites: true });
    const lines: string[] = [];
    let listener: ((phase: string) => void) | undefined;
    const bindingPromise = harness.module.bindSqryReadyContext(
      {
        onDidChangePhase: (nextListener) => {
          listener = nextListener;
          return { dispose: () => { listener = undefined; } };
        },
      },
      { appendLine: (line: string) => lines.push(line) } as unknown as import("vscode").OutputChannel,
      () => undefined,
    );
    await flushAsyncWork();
    harness.pendingContextWrites[0].reject(new Error("context service unavailable"));

    let caught: Error | undefined;
    try {
      await bindingPromise;
    } catch (error) {
      caught = error as Error;
    }

    expect(caught).to.be.instanceOf(harness.module.ReadinessContextInitializationError);
    expect(listener).to.equal(undefined);
    expect(harness.contextCalls).to.deep.equal([["setContext", "sqry.ready", false]]);
    expect(lines.join("\n")).to.include("context service unavailable");
  });

  it("recovers the ordered queue after a runtime write failure", async () => {
    const harness = loadStartupCommandHarness({ deferContextWrites: true });
    const runtimeFailures: Error[] = [];
    let listener: ((phase: string) => void) | undefined;
    const bindingPromise = harness.module.bindSqryReadyContext(
      {
        onDidChangePhase: (nextListener) => {
          listener = nextListener;
          return { dispose: () => { listener = undefined; } };
        },
      },
      undefined,
      (error) => runtimeFailures.push(error),
    );
    await flushAsyncWork();
    harness.pendingContextWrites[0].resolve();
    const binding = await bindingPromise;
    if (!listener) {
      throw new Error("expected readiness listener");
    }
    const emitPhase = listener;

    emitPhase("Ready");
    await flushAsyncWork();
    harness.pendingContextWrites[1].resolve();
    await flushAsyncWork();
    listener("WorkspaceResolving");
    await flushAsyncWork();
    harness.pendingContextWrites[2].reject(new Error("temporary context failure"));
    await flushAsyncWork();
    expect(runtimeFailures.map((error) => error.message)).to.deep.equal(["temporary context failure"]);

    listener("Ready");
    await flushAsyncWork();
    expect(harness.contextCalls[3]).to.deep.equal(["setContext", "sqry.ready", true]);
    harness.pendingContextWrites[3].resolve();
    await flushAsyncWork();

    const closePromise = binding.close();
    await flushAsyncWork();
    harness.pendingContextWrites[4].resolve();
    await closePromise;
  });

  it("orders deactivation false after queued Ready and makes close idempotent", async () => {
    const harness = loadStartupCommandHarness({ deferContextWrites: true });
    let listener: ((phase: string) => void) | undefined;
    const bindingPromise = harness.module.bindSqryReadyContext(
      {
        onDidChangePhase: (nextListener) => {
          listener = nextListener;
          return { dispose: () => { listener = undefined; } };
        },
      },
      undefined,
      () => undefined,
    );
    await flushAsyncWork();
    harness.pendingContextWrites[0].resolve();
    const binding = await bindingPromise;
    if (!listener) {
      throw new Error("expected readiness listener");
    }

    const emitPhase = listener;
    emitPhase("Ready");
    await flushAsyncWork();
    const firstClose = binding.close();
    const secondClose = binding.close();
    expect(secondClose).to.equal(firstClose);
    emitPhase("Failed");
    await flushAsyncWork();
    expect(harness.contextCalls).to.have.length(2);

    harness.pendingContextWrites[1].resolve();
    await flushAsyncWork();
    expect(harness.contextCalls[2]).to.deep.equal(["setContext", "sqry.ready", false]);
    harness.pendingContextWrites[2].resolve();
    await Promise.all([firstClose, secondClose]);
    expect(harness.appliedContextValues).to.deep.equal([false, true, false]);
  });
});
