/**
 * STEP_5 multi-root acceptance — exercises the `SqryClient`
 * workspace-status surface (`getWorkspaceStatus` /
 * `getSourceRootStatus`), the per-request cancellation contract, and
 * the `LanguageClientOptions.workspaceFolder` non-pin invariant.
 *
 * Tests stub the `vscode` module via `proxyquire` (no extension host
 * required). The transport layer is replaced with a fake that records
 * the cancellation tokens passed by the client, which lets us assert
 * the criterion 6 contract: concurrent in-flight requests own
 * independent tokens and do NOT cancel each other.
 */
import { expect } from "chai";
import proxyquireModule from "proxyquire";

const proxyquire = proxyquireModule.noCallThru();

interface RecordedToken {
  isCancellationRequested: boolean;
  source: { cancel(): void; dispose(): void };
}

class FakeCancellationTokenSource {
  public token: {
    isCancellationRequested: boolean;
    onCancellationRequested(listener: () => void): { dispose(): void };
  };
  public _disposed = false;
  public cancelCalls = 0;
  public disposeCalls = 0;
  public cancellationListenerDisposals = 0;
  private readonly cancellationListeners = new Set<() => void>();
  constructor() {
    this.token = {
      isCancellationRequested: false,
      onCancellationRequested: (listener) => {
        this.cancellationListeners.add(listener);
        return {
          dispose: () => {
            if (this.cancellationListeners.delete(listener)) {
              this.cancellationListenerDisposals += 1;
            }
          },
        };
      },
    };
  }
  cancel(): void {
    this.cancelCalls += 1;
    if (this.token.isCancellationRequested) {
      return;
    }
    this.token.isCancellationRequested = true;
    for (const listener of [...this.cancellationListeners]) {
      listener();
    }
  }
  dispose(): void {
    this.disposeCalls += 1;
    this._disposed = true;
  }
}

interface FakeLanguageClientCalls {
  initOptions: unknown;
  workspaceFolderPin: unknown;
  receivedRequests: Array<{ method: string; params: unknown; token: { isCancellationRequested: boolean } }>;
  cancellationSources: FakeCancellationTokenSource[];
  languageClientConstructions: number;
  languageClientStarts: number;
  languageClientStops: number;
  triggerSqryConfigurationChange(): void;
}

interface FakeLanguageClientLifecycleOptions {
  /** Optional gates for selected `LanguageClient.stop()` calls (one-based). */
  readonly stopGates?: ReadonlyMap<number, Promise<void>>;
  /** Optional gates for selected `LanguageClient.start()` calls (one-based). */
  readonly startGates?: ReadonlyMap<number, Promise<void>>;
  /** Synchronously observes a fake client entering `start()`. */
  readonly onStart?: (ordinal: number) => void;
}

function buildSqryClientModule(
  responsesByMethod: Map<string, (params: unknown) => unknown>,
  timeoutOverrides: Partial<{ timeoutMs: number; indexTimeoutMs: number }> = {},
  lifecycleOptions: FakeLanguageClientLifecycleOptions = {},
): {
  SqryClient: typeof import("../src/sqryClient").SqryClient;
  calls: FakeLanguageClientCalls;
} {
  let configurationChangeListener:
    | ((event: { affectsConfiguration(section: string): boolean }) => unknown)
    | undefined;
  const calls: FakeLanguageClientCalls = {
    initOptions: undefined,
    workspaceFolderPin: undefined,
    receivedRequests: [],
    cancellationSources: [],
    languageClientConstructions: 0,
    languageClientStarts: 0,
    languageClientStops: 0,
    triggerSqryConfigurationChange: () => {
      void configurationChangeListener?.({
        affectsConfiguration: (section: string) => section === "sqry",
      });
    },
  };

  // The SqryClient owns one LanguageClient via vscode-languageclient/node.
  // We stub the module so client.start() resolves immediately and
  // sendRequest pulls from the response map.
  class FakeLanguageClient {
    constructor(
      _id: string,
      _name: string,
      _serverOptions: unknown,
      clientOptions: { initializationOptions?: unknown; workspaceFolder?: unknown },
    ) {
      calls.languageClientConstructions += 1;
      calls.initOptions = clientOptions.initializationOptions;
      calls.workspaceFolderPin = clientOptions.workspaceFolder;
    }
    start(): Promise<void> {
      calls.languageClientStarts += 1;
      lifecycleOptions.onStart?.(calls.languageClientStarts);
      return lifecycleOptions.startGates?.get(calls.languageClientStarts) ?? Promise.resolve();
    }
    stop(): Promise<void> {
      calls.languageClientStops += 1;
      return lifecycleOptions.stopGates?.get(calls.languageClientStops) ?? Promise.resolve();
    }
    onNotification(): void {
      // no-op
    }
    async sendRequest(method: unknown, params: unknown, token: { isCancellationRequested: boolean }): Promise<unknown> {
      // Surface the actual JSON-RPC method name even when callers pass
      // the typed `ExecuteCommandRequest.type` constant.
      const methodName = typeof method === "string" ? method : (method as { method?: string }).method ?? "executeCommand";
      calls.receivedRequests.push({ method: methodName, params, token });
      const handler = responsesByMethod.get(methodName);
      if (!handler) {
        throw new Error(`unstubbed method: ${methodName}`);
      }
      return handler(params);
    }
  }

  // Partial vscode + node-languageclient stubs. SqryClient pulls in
  // `which` and `node:fs` for binary resolution; we short-circuit by
  // providing a concrete `sqry.path` and a fake `which` that returns
  // the path verbatim.
  class RecordingCancellationTokenSource extends FakeCancellationTokenSource {
    constructor() {
      super();
      calls.cancellationSources.push(this);
    }
  }

  const lcStub = {
    LanguageClient: FakeLanguageClient,
    CancellationTokenSource: RecordingCancellationTokenSource,
    ExecuteCommandRequest: { type: { method: "workspace/executeCommand" } },
  };

  const vscodeStub = {
    EventEmitter: class {
      public event = (_listener: (...a: unknown[]) => void) => ({ dispose: () => undefined });
      public fire(): void {
        // no-op
      }
      public dispose(): void {
        // no-op
      }
    },
    workspace: {
      getConfiguration: () => ({
        // eslint-disable-next-line @typescript-eslint/no-unused-vars
        get<T>(_key: string, fallback?: T): T | undefined {
          return fallback;
        },
      }),
      onDidChangeConfiguration: (
        listener: (event: { affectsConfiguration(section: string): boolean }) => unknown,
      ) => {
        configurationChangeListener = listener;
        return {
          dispose: () => {
            if (configurationChangeListener === listener) {
              configurationChangeListener = undefined;
            }
          },
        };
      },
      // STEP_5 acceptance criterion 7 — even when the host has folders,
      // the client must NOT pin the first one. We deliberately surface
      // a folder so the test fails if SqryClient passes it through.
      workspaceFolders: [
        { uri: { fsPath: "/workspace/a" }, name: "a", index: 0 },
      ],
    },
    Uri: {
      parse: (s: string) => ({ fsPath: s, scheme: "file", toString: () => s }),
    },
    window: {
      showWarningMessage: async () => undefined,
    },
  };

  // Force the binary resolver to a deterministic stub path.
  const configStub = {
    resolveConfig: async () => ({
      sqryPath: "sqry",
      limit: 200,
      timeoutMs: timeoutOverrides.timeoutMs ?? 1_000,
      indexTimeoutMs: timeoutOverrides.indexTimeoutMs ?? 60_000,
      autoIndexOnOpen: "never",
      codeLensEnabled: false,
      indexRoot: "",
      projectRootMode: "gitRoot",
      workspaceFolderExcludes: [],
      workspaceClassification: null,
      resolvedBinaryPath: "/usr/local/bin/sqry",
    }),
  };

  const indexQueueStub = {
    IndexQueue: class {
      run<T>(_key: string, fn: () => Promise<T>): Promise<T> {
        return fn();
      }
    },
  };

  const commandResultsStub = {
    handleExecuteCommandResult: (
      _command: string,
      _args: unknown[],
      next: (command: string, args: unknown[]) => Promise<unknown>,
    ) => next(_command, _args),
  };

  const { SqryClient } = proxyquire("../src/sqryClient", {
    vscode: vscodeStub,
    "vscode-languageclient/node": lcStub,
    "./commandResults": commandResultsStub,
    "./config": configStub,
    "./indexQueue": indexQueueStub,
  }) as { SqryClient: typeof import("../src/sqryClient").SqryClient };

  return { SqryClient, calls };
}

const noopOutput = {
  appendLine: () => undefined,
  show: () => undefined,
} as unknown as import("vscode").OutputChannel;

function deferred(): { readonly promise: Promise<void>; resolve(): void } {
  let resolvePromise: (() => void) | undefined;
  const promise = new Promise<void>((resolve) => {
    resolvePromise = resolve;
  });
  return {
    promise,
    resolve: () => {
      if (!resolvePromise) {
        throw new Error("deferred resolver was not initialized");
      }
      resolvePromise();
    },
  };
}

describe("STEP_5 — SqryClient.getWorkspaceStatus", () => {
  it("returns the aggregate from sqry/workspaceStatus", async () => {
    const responses = new Map<string, (params: unknown) => unknown>([
      [
        "sqry/workspaceStatus",
        () => ({
          workspace_id_short: "abc123",
          workspace_id_full: "abc123",
          project_root_mode: "gitRoot",
          source_roots: ["/a", "/b"],
          member_folders: [],
          exclusions: [],
          aggregate: {
            source_root_statuses: [
              { path: "/a", status: "ok", symbol_count: 100 },
              { path: "/b", status: "missing" },
            ],
            missing_count: 1,
            building_count: 0,
            ok_count: 1,
            error_count: 0,
            generated_at: "2026-04-26T00:00:00Z",
          },
        }),
      ],
    ]);
    const { SqryClient } = buildSqryClientModule(responses);
    const client = new SqryClient(noopOutput);
    await client.initialize();
    const status = await client.getWorkspaceStatus();
    expect(status.source_root_statuses).to.have.length(2);
    expect(status.ok_count).to.equal(1);
    expect(status.missing_count).to.equal(1);
    client.dispose();
  });

  it("returns a single-source-root workspace aggregate", async () => {
    const responses = new Map<string, (params: unknown) => unknown>([
      [
        "sqry/workspaceStatus",
        () => ({
          workspace_id_short: "single",
          workspace_id_full: "single",
          project_root_mode: "gitRoot",
          source_roots: ["/single/folder"],
          member_folders: [],
          exclusions: [],
          aggregate: {
            source_root_statuses: [
              { path: "/single/folder", status: "ok", symbol_count: 42 },
            ],
            missing_count: 0,
            building_count: 0,
            ok_count: 1,
            error_count: 0,
            generated_at: "2026-04-26T00:00:00Z",
          },
        }),
      ],
    ]);
    const { SqryClient } = buildSqryClientModule(responses);
    const client = new SqryClient(noopOutput);
    await client.initialize();
    const status = await client.getWorkspaceStatus();
    expect(status.source_root_statuses).to.have.length(1);
    expect(status.source_root_statuses[0].path).to.equal("/single/folder");
    expect(status.source_root_statuses[0].status).to.equal("ok");
    expect(status.ok_count).to.equal(1);
    client.dispose();
  });
});

describe("STEP_5 — SqryClient.getSourceRootStatus", () => {
  it("extracts the matching source-root entry from the aggregate", async () => {
    const responses = new Map<string, (params: unknown) => unknown>([
      [
        "sqry/indexStatus",
        (params: unknown) => {
          // sanity-check: the client forwards the requested path
          expect((params as { path?: string }).path).to.equal("/b");
          return {
            status: {
              exists: false,
              supports_fuzzy: true,
              supports_relations: true,
              aggregate: {
                source_root_statuses: [
                  { path: "/a", status: "ok" },
                  { path: "/b", status: "building" },
                ],
                missing_count: 0,
                building_count: 1,
                ok_count: 1,
                error_count: 0,
                generated_at: "now",
              },
            },
          };
        },
      ],
    ]);
    const { SqryClient } = buildSqryClientModule(responses);
    const client = new SqryClient(noopOutput);
    await client.initialize();
    const folder = { uri: { fsPath: "/b" } } as import("vscode").WorkspaceFolder;
    const status = await client.getSourceRootStatus(folder);
    expect(status.path).to.equal("/b");
    expect(status.status).to.equal("building");
    client.dispose();
  });
});

describe("STEP_5 acceptance criterion 6 — per-request cancellation", () => {
  it("concurrent requests own distinct cancellation tokens", async () => {
    type Resolver = (v: unknown) => void;
    const slots: { first: Resolver | null; second: Resolver | null } = { first: null, second: null };
    const responses = new Map<string, (params: unknown) => unknown>([
      [
        "sqry/indexStatus",
        () => {
          if (slots.first === null) {
            return new Promise((resolve) => {
              slots.first = resolve;
            });
          }
          return new Promise((resolve) => {
            slots.second = resolve;
          });
        },
      ],
      [
        "sqry/workspaceStatus",
        () => new Promise((resolve) => {
          slots.second = resolve;
        }),
      ],
    ]);
    const { SqryClient, calls } = buildSqryClientModule(responses);
    const client = new SqryClient(noopOutput);
    await client.initialize();

    const folder = { uri: { fsPath: "/x" } } as import("vscode").WorkspaceFolder;
    const a = client.getSourceRootStatus(folder);
    const b = client.getWorkspaceStatus();

    // Allow both to enter `sendRequest` before resolving.
    await new Promise((r) => setTimeout(r, 5));

    expect(calls.receivedRequests).to.have.length(2);
    const tokenA = calls.receivedRequests[0].token;
    const tokenB = calls.receivedRequests[1].token;
    // Distinct tokens — issuing the second request did NOT cancel the first.
    expect(tokenA).to.not.equal(tokenB);
    expect(tokenA.isCancellationRequested).to.equal(false);
    expect(tokenB.isCancellationRequested).to.equal(false);

    // Resolve both — they must not have been auto-cancelled by each other.
    if (!slots.first || !slots.second) {
      throw new Error("expected both requests to suspend in sendRequest");
    }
    slots.first({
      status: {
        exists: true,
        supports_fuzzy: true,
        supports_relations: true,
        aggregate: {
          source_root_statuses: [{ path: "/x", status: "ok" }],
          missing_count: 0,
          building_count: 0,
          ok_count: 1,
          error_count: 0,
          generated_at: "now",
        },
      },
    });
    slots.second({
      workspace_id_short: "x",
      workspace_id_full: "x",
      project_root_mode: "gitRoot",
      source_roots: ["/x"],
      member_folders: [],
      exclusions: [],
      aggregate: {
        source_root_statuses: [{ path: "/x", status: "ok" }],
        missing_count: 0,
        building_count: 0,
        ok_count: 1,
        error_count: 0,
        generated_at: "now",
      },
    });
    await Promise.all([a, b]);
    client.dispose();
  });

  it("locally rejects a never-settling ordinary request and cleans up its source", async () => {
    let rejectTransport: ((reason?: unknown) => void) | undefined;
    const responses = new Map<string, (params: unknown) => unknown>([
      [
        "sqry/workspaceStatus",
        () => new Promise<unknown>((_resolve, reject) => {
          rejectTransport = reject;
        }),
      ],
    ]);
    const { SqryClient, calls } = buildSqryClientModule(responses, { timeoutMs: 15 });
    const client = new SqryClient(noopOutput);
    await client.initialize();

    let caught: Error | undefined;
    try {
      await client.getWorkspaceStatus();
    } catch (error) {
      caught = error as Error;
    }

    expect(caught?.message).to.include("timed out after 15ms");
    // Initialization and its successful candidate each own a source; the
    // request source is constructed last and settles independently.
    expect(calls.cancellationSources).to.have.length(3);
    expect(calls.cancellationSources[2].cancelCalls).to.equal(1);
    expect(calls.cancellationSources[2].disposeCalls).to.equal(1);
    expect(
      (client as unknown as { activeRequests: Set<unknown> }).activeRequests.size,
    ).to.equal(0);

    const unhandled: unknown[] = [];
    const onUnhandled = (reason: unknown) => unhandled.push(reason);
    process.on("unhandledRejection", onUnhandled);
    try {
      if (!rejectTransport) {
        throw new Error("expected request transport to be pending");
      }
      rejectTransport(new Error("late transport failure"));
      await new Promise((resolve) => setTimeout(resolve, 0));
    } finally {
      process.off("unhandledRejection", onUnhandled);
    }
    expect(unhandled).to.deep.equal([]);
    client.dispose();
  });

  it("locally rejects a never-settling index command using sqry.indexTimeoutMs", async () => {
    const responses = new Map<string, (params: unknown) => unknown>([
      ["workspace/executeCommand", () => new Promise<unknown>(() => undefined)],
    ]);
    const { SqryClient, calls } = buildSqryClientModule(responses, {
      timeoutMs: 100,
      indexTimeoutMs: 15,
    });
    const client = new SqryClient(noopOutput);
    await client.initialize();
    const workspace = { uri: { fsPath: "/index-root" } } as import("vscode").WorkspaceFolder;

    let caught: Error | undefined;
    try {
      await client.runIndex(workspace);
    } catch (error) {
      caught = error as Error;
    }

    expect(caught?.message).to.include("timed out after 15ms");
    expect(caught?.message).to.include("sqry.indexTimeoutMs");
    expect(calls.cancellationSources[2].cancelCalls).to.equal(1);
    expect(calls.cancellationSources[2].disposeCalls).to.equal(1);
    expect(
      (client as unknown as { activeRequests: Set<unknown> }).activeRequests.size,
    ).to.equal(0);
    client.dispose();
  });

  it("settles a pending caller during extension disposal", async () => {
    const responses = new Map<string, (params: unknown) => unknown>([
      ["sqry/workspaceStatus", () => new Promise<unknown>(() => undefined)],
    ]);
    const { SqryClient, calls } = buildSqryClientModule(responses, { timeoutMs: 1_000 });
    const client = new SqryClient(noopOutput);
    await client.initialize();

    const pending = client.getWorkspaceStatus();
    await new Promise((resolve) => setTimeout(resolve, 0));
    client.dispose();

    let caught: Error | undefined;
    try {
      await pending;
    } catch (error) {
      caught = error as Error;
    }

    expect(caught?.message).to.include("extension is deactivating");
    expect(calls.cancellationSources[2].cancelCalls).to.equal(1);
    expect(calls.cancellationSources[2].disposeCalls).to.equal(1);
    expect(
      (client as unknown as { activeRequests: Set<unknown> }).activeRequests.size,
    ).to.equal(0);
  });

  it("does not restart or dispatch an ordinary RPC after its local deadline wins during client restart", async () => {
    const stopGate = deferred();
    const responses = new Map<string, (params: unknown) => unknown>();
    const { SqryClient, calls } = buildSqryClientModule(
      responses,
      { timeoutMs: 15 },
      { stopGates: new Map([[1, stopGate.promise]]) },
    );
    const client = new SqryClient(noopOutput);
    await client.initialize();
    (client as unknown as { currentBinaryPath: string | null }).currentBinaryPath = "/stale/sqry";

    const pending = client.getWorkspaceStatus();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(calls.languageClientStops).to.equal(1);

    let caught: Error | undefined;
    try {
      await pending;
    } catch (error) {
      caught = error as Error;
    }
    expect(caught?.message).to.include("timed out after 15ms");

    stopGate.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(calls.languageClientConstructions).to.equal(1);
    expect(calls.languageClientStarts).to.equal(1);
    expect(calls.receivedRequests).to.deep.equal([]);
    client.dispose();
  });

  it("does not restart or dispatch an index RPC after disposal wins during client restart", async () => {
    const stopGate = deferred();
    const responses = new Map<string, (params: unknown) => unknown>();
    const { SqryClient, calls } = buildSqryClientModule(
      responses,
      { timeoutMs: 1_000, indexTimeoutMs: 1_000 },
      { stopGates: new Map([[1, stopGate.promise]]) },
    );
    const client = new SqryClient(noopOutput);
    await client.initialize();
    (client as unknown as { currentBinaryPath: string | null }).currentBinaryPath = "/stale/sqry";
    const workspace = { uri: { fsPath: "/index-root" } } as import("vscode").WorkspaceFolder;

    const pending = client.runIndex(workspace);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(calls.languageClientStops).to.equal(1);
    client.dispose();

    let caught: Error | undefined;
    try {
      await pending;
    } catch (error) {
      caught = error as Error;
    }
    expect(caught?.message).to.include("extension is deactivating");

    stopGate.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(calls.languageClientConstructions).to.equal(1);
    expect(calls.languageClientStarts).to.equal(1);
    expect(calls.receivedRequests).to.deep.equal([]);
  });

  it("stops a delayed candidate and never dispatches an ordinary RPC after its local deadline", async () => {
    const secondStartGate = deferred();
    const secondStartEntered = deferred();
    const responses = new Map<string, (params: unknown) => unknown>();
    const { SqryClient, calls } = buildSqryClientModule(
      responses,
      { timeoutMs: 15 },
      {
        startGates: new Map([[2, secondStartGate.promise]]),
        onStart: (ordinal) => {
          if (ordinal === 2) {
            secondStartEntered.resolve();
          }
        },
      },
    );
    const client = new SqryClient(noopOutput);
    await client.initialize();
    (client as unknown as { languageClient: unknown }).languageClient = null;

    const pending = client.getWorkspaceStatus();
    await secondStartEntered.promise;

    let caught: Error | undefined;
    try {
      await pending;
    } catch (error) {
      caught = error as Error;
    }
    expect(caught?.message).to.include("timed out after 15ms");
    expect(calls.languageClientConstructions).to.equal(2);
    expect(calls.languageClientStarts).to.equal(2);
    expect(calls.languageClientStops).to.equal(1);
    expect(calls.cancellationSources[1].cancellationListenerDisposals).to.equal(1);
    expect((client as unknown as { languageClient: unknown }).languageClient).to.equal(null);
    expect(calls.receivedRequests).to.deep.equal([]);

    const unhandled: unknown[] = [];
    const onUnhandled = (reason: unknown) => unhandled.push(reason);
    process.on("unhandledRejection", onUnhandled);
    try {
      secondStartGate.resolve();
      await new Promise((resolve) => setTimeout(resolve, 0));
    } finally {
      process.off("unhandledRejection", onUnhandled);
    }
    expect(unhandled).to.deep.equal([]);
    expect(calls.receivedRequests).to.deep.equal([]);
    client.dispose();
  });

  it("stops a delayed candidate and never dispatches an index RPC after its local deadline", async () => {
    const secondStartGate = deferred();
    const secondStartEntered = deferred();
    const responses = new Map<string, (params: unknown) => unknown>();
    const { SqryClient, calls } = buildSqryClientModule(
      responses,
      { timeoutMs: 100, indexTimeoutMs: 15 },
      {
        startGates: new Map([[2, secondStartGate.promise]]),
        onStart: (ordinal) => {
          if (ordinal === 2) {
            secondStartEntered.resolve();
          }
        },
      },
    );
    const client = new SqryClient(noopOutput);
    await client.initialize();
    (client as unknown as { languageClient: unknown }).languageClient = null;
    const workspace = { uri: { fsPath: "/index-root" } } as import("vscode").WorkspaceFolder;

    const pending = client.runIndex(workspace);
    await secondStartEntered.promise;

    let caught: Error | undefined;
    try {
      await pending;
    } catch (error) {
      caught = error as Error;
    }
    expect(caught?.message).to.include("timed out after 15ms");
    expect(caught?.message).to.include("sqry.indexTimeoutMs");
    expect(calls.languageClientStops).to.equal(1);
    expect(calls.receivedRequests).to.deep.equal([]);

    secondStartGate.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect((client as unknown as { languageClient: unknown }).languageClient).to.equal(null);
    expect(calls.receivedRequests).to.deep.equal([]);
    client.dispose();
  });

  it("stops a delayed candidate and never dispatches after extension disposal", async () => {
    const secondStartGate = deferred();
    const secondStartEntered = deferred();
    const responses = new Map<string, (params: unknown) => unknown>();
    const { SqryClient, calls } = buildSqryClientModule(
      responses,
      { timeoutMs: 1_000 },
      {
        startGates: new Map([[2, secondStartGate.promise]]),
        onStart: (ordinal) => {
          if (ordinal === 2) {
            secondStartEntered.resolve();
          }
        },
      },
    );
    const client = new SqryClient(noopOutput);
    await client.initialize();
    (client as unknown as { languageClient: unknown }).languageClient = null;

    const pending = client.getWorkspaceStatus();
    await secondStartEntered.promise;
    client.dispose();

    let caught: Error | undefined;
    try {
      await pending;
    } catch (error) {
      caught = error as Error;
    }
    expect(caught?.message).to.include("extension is deactivating");
    expect(calls.languageClientStops).to.equal(1);
    expect((client as unknown as { languageClient: unknown }).languageClient).to.equal(null);
    expect(calls.receivedRequests).to.deep.equal([]);

    secondStartGate.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(calls.receivedRequests).to.deep.equal([]);
  });

  it("stops a delayed explicit-restart candidate immediately on extension disposal", async () => {
    const secondStartGate = deferred();
    const secondStartEntered = deferred();
    const responses = new Map<string, (params: unknown) => unknown>();
    const { SqryClient, calls } = buildSqryClientModule(
      responses,
      {},
      {
        startGates: new Map([[2, secondStartGate.promise]]),
        onStart: (ordinal) => {
          if (ordinal === 2) {
            secondStartEntered.resolve();
          }
        },
      },
    );
    const client = new SqryClient(noopOutput);
    await client.initialize();

    const restarting = client.restart();
    await secondStartEntered.promise;
    client.dispose();

    let caught: Error | undefined;
    try {
      await restarting;
    } catch (error) {
      caught = error as Error;
    }

    expect(caught?.message).to.include("cancelled because the extension is deactivating");
    expect(calls.languageClientStops).to.equal(2);
    expect(calls.cancellationSources[2].cancelCalls).to.equal(1);
    expect(calls.cancellationSources[2].disposeCalls).to.equal(1);
    expect((client as unknown as { languageClient: unknown }).languageClient).to.equal(null);
    expect(calls.receivedRequests).to.deep.equal([]);

    const unhandled: unknown[] = [];
    const onUnhandled = (reason: unknown) => unhandled.push(reason);
    process.on("unhandledRejection", onUnhandled);
    try {
      secondStartGate.resolve();
      await new Promise((resolve) => setTimeout(resolve, 0));
    } finally {
      process.off("unhandledRejection", onUnhandled);
    }
    expect(unhandled).to.deep.equal([]);
    expect((client as unknown as { languageClient: unknown }).languageClient).to.equal(null);
  });

  it("stops a delayed configuration-reload candidate immediately on extension disposal", async () => {
    const secondStartGate = deferred();
    const secondStartEntered = deferred();
    const responses = new Map<string, (params: unknown) => unknown>();
    const { SqryClient, calls } = buildSqryClientModule(
      responses,
      {},
      {
        startGates: new Map([[2, secondStartGate.promise]]),
        onStart: (ordinal) => {
          if (ordinal === 2) {
            secondStartEntered.resolve();
          }
        },
      },
    );
    const client = new SqryClient(noopOutput);
    await client.initialize();
    (client as unknown as { languageClient: unknown }).languageClient = null;

    const unhandled: unknown[] = [];
    const onUnhandled = (reason: unknown) => unhandled.push(reason);
    process.on("unhandledRejection", onUnhandled);
    try {
      calls.triggerSqryConfigurationChange();
      await secondStartEntered.promise;
      client.dispose();
      await new Promise((resolve) => setTimeout(resolve, 0));

      expect(calls.languageClientStops).to.equal(1);
      expect(calls.cancellationSources[2].cancelCalls).to.equal(1);
      expect(calls.cancellationSources[2].disposeCalls).to.equal(1);
      expect((client as unknown as { languageClient: unknown }).languageClient).to.equal(null);
      expect(calls.receivedRequests).to.deep.equal([]);

      secondStartGate.resolve();
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect((client as unknown as { languageClient: unknown }).languageClient).to.equal(null);
    } finally {
      process.off("unhandledRejection", onUnhandled);
    }
    expect(unhandled).to.deep.equal([]);
  });

  it("serializes overlapping restarts so only one candidate exists at a time", async () => {
    const secondStartGate = deferred();
    const thirdStartGate = deferred();
    const secondStartEntered = deferred();
    const thirdStartEntered = deferred();
    const responses = new Map<string, (params: unknown) => unknown>();
    const { SqryClient, calls } = buildSqryClientModule(
      responses,
      {},
      {
        startGates: new Map([
          [2, secondStartGate.promise],
          [3, thirdStartGate.promise],
        ]),
        onStart: (ordinal) => {
          if (ordinal === 2) {
            secondStartEntered.resolve();
          }
          if (ordinal === 3) {
            thirdStartEntered.resolve();
          }
        },
      },
    );
    const client = new SqryClient(noopOutput);
    await client.initialize();

    const firstRestart = client.restart();
    await secondStartEntered.promise;
    const secondRestart = client.restart();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(calls.languageClientStarts).to.equal(2);

    secondStartGate.resolve();
    await firstRestart;
    await thirdStartEntered.promise;
    expect(calls.languageClientStarts).to.equal(3);
    expect(calls.languageClientStops).to.equal(2);

    thirdStartGate.resolve();
    await secondRestart;
    expect((client as unknown as { languageClient: unknown }).languageClient).to.not.equal(null);

    client.dispose();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(calls.languageClientStops).to.equal(3);
  });

  it("serializes an overlapping configuration reload and explicit restart", async () => {
    const secondStartGate = deferred();
    const thirdStartGate = deferred();
    const secondStartEntered = deferred();
    const thirdStartEntered = deferred();
    const responses = new Map<string, (params: unknown) => unknown>();
    const { SqryClient, calls } = buildSqryClientModule(
      responses,
      {},
      {
        startGates: new Map([
          [2, secondStartGate.promise],
          [3, thirdStartGate.promise],
        ]),
        onStart: (ordinal) => {
          if (ordinal === 2) {
            secondStartEntered.resolve();
          }
          if (ordinal === 3) {
            thirdStartEntered.resolve();
          }
        },
      },
    );
    const client = new SqryClient(noopOutput);
    await client.initialize();
    (client as unknown as { languageClient: unknown }).languageClient = null;

    calls.triggerSqryConfigurationChange();
    await secondStartEntered.promise;
    const explicitRestart = client.restart();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(calls.languageClientStarts).to.equal(2);

    secondStartGate.resolve();
    await thirdStartEntered.promise;
    expect(calls.languageClientStarts).to.equal(3);
    expect(calls.languageClientStops).to.equal(1);

    thirdStartGate.resolve();
    await explicitRestart;
    expect((client as unknown as { languageClient: unknown }).languageClient).to.not.equal(null);
    client.dispose();
  });

  it("cancels a delayed initial client candidate immediately on extension disposal", async () => {
    const initialStartGate = deferred();
    const initialStartEntered = deferred();
    const responses = new Map<string, (params: unknown) => unknown>();
    const { SqryClient, calls } = buildSqryClientModule(
      responses,
      {},
      {
        startGates: new Map([[1, initialStartGate.promise]]),
        onStart: (ordinal) => {
          if (ordinal === 1) {
            initialStartEntered.resolve();
          }
        },
      },
    );
    const client = new SqryClient(noopOutput);
    const initializing = client.initialize();
    await initialStartEntered.promise;
    expect((client as unknown as { languageClient: unknown }).languageClient).to.equal(null);

    client.dispose();
    let caught: Error | undefined;
    try {
      await initializing;
    } catch (error) {
      caught = error as Error;
    }

    expect(caught?.message).to.include("cancelled while starting the language client");
    expect(calls.cancellationSources).to.have.length(2);
    expect(calls.cancellationSources[0].cancelCalls).to.equal(1);
    expect(calls.cancellationSources[0].disposeCalls).to.equal(1);
    expect(calls.cancellationSources[1].cancelCalls).to.equal(1);
    expect(calls.cancellationSources[1].disposeCalls).to.equal(1);
    expect(calls.languageClientStops).to.equal(1);
    expect(calls.receivedRequests).to.deep.equal([]);
    expect((client as unknown as { languageClient: unknown }).languageClient).to.equal(null);

    const unhandled: unknown[] = [];
    const onUnhandled = (reason: unknown) => unhandled.push(reason);
    process.on("unhandledRejection", onUnhandled);
    try {
      initialStartGate.resolve();
      await new Promise((resolve) => setTimeout(resolve, 0));
    } finally {
      process.off("unhandledRejection", onUnhandled);
    }
    expect(unhandled).to.deep.equal([]);
    expect((client as unknown as { languageClient: unknown }).languageClient).to.equal(null);
  });

  it("cancels a delayed initial candidate on timeout and blocks terminal-failure config restarts", async () => {
    const initialStartGate = deferred();
    const initialStartEntered = deferred();
    const responses = new Map<string, (params: unknown) => unknown>();
    const { SqryClient, calls } = buildSqryClientModule(
      responses,
      {},
      {
        startGates: new Map([[1, initialStartGate.promise]]),
        onStart: (ordinal) => {
          if (ordinal === 1) {
            initialStartEntered.resolve();
          }
        },
      },
    );
    // A short constructor-only test seam exercises the production 10s
    // initialization owner without a real ten-second test wait.
    const client = new SqryClient(noopOutput, 50);
    const initializing = client.initialize();
    await initialStartEntered.promise;

    // Queue tokenless configuration work behind the in-flight initialization
    // restart. The terminal activation cleanup below must prevent this queued
    // continuation from constructing a second client after the timeout.
    calls.triggerSqryConfigurationChange();
    await new Promise((resolve) => setTimeout(resolve, 0));

    let caught: Error | undefined;
    try {
      await initializing;
    } catch (error) {
      caught = error as Error;
    }

    expect(caught?.message).to.include("Configuration timeout after 10s");
    expect(calls.cancellationSources).to.have.length(2);
    expect(calls.cancellationSources[0].cancelCalls).to.equal(1);
    expect(calls.cancellationSources[0].disposeCalls).to.equal(1);
    expect(calls.cancellationSources[1].cancelCalls).to.equal(0);
    expect(calls.cancellationSources[1].disposeCalls).to.equal(1);
    expect(calls.languageClientStops).to.equal(1);
    expect(calls.receivedRequests).to.deep.equal([]);
    expect((client as unknown as { languageClient: unknown }).languageClient).to.equal(null);

    // Activation disposes this final-failure client before returning. That
    // cancels the queued configuration continuation and unregisters its
    // listener, so neither queued nor later tokenless reloads can revive it.
    client.dispose();
    calls.triggerSqryConfigurationChange();

    initialStartGate.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(calls.languageClientConstructions).to.equal(1);
    expect(calls.languageClientStarts).to.equal(1);
    expect((client as unknown as { languageClient: unknown }).languageClient).to.equal(null);
  });
});

describe("STEP_5 acceptance criterion 7 — LanguageClientOptions does NOT pin workspaceFolder", () => {
  it("client construction omits the workspaceFolder pin even when folders are open", async () => {
    const responses = new Map<string, (params: unknown) => unknown>();
    const { SqryClient, calls } = buildSqryClientModule(responses);
    const client = new SqryClient(noopOutput);
    await client.initialize();
    expect(calls.workspaceFolderPin).to.equal(undefined);
    client.dispose();
  });

  it("forwards initializationOptions.sqry.{workspace,workspaceFile} when set", async () => {
    const responses = new Map<string, (params: unknown) => unknown>();
    const { SqryClient, calls } = buildSqryClientModule(responses);
    const client = new SqryClient(noopOutput);
    // STEP_5 codex iter1 MAJOR fix: `workspace` is the parsed object,
    // `workspaceFile` is the path string.
    client.setInitializationOptions({
      workspace: {
        folders: [{ path: "./repo-a" }, { path: "./repo-b", name: "Repo B" }],
        classification: {
          sourceRoots: ["./repo-a"],
          memberFolders: ["./repo-b"],
          projectRootMode: "gitRoot",
        },
      },
      workspaceFile: "/path/to/proj.code-workspace",
    });
    await client.initialize();
    expect(calls.initOptions).to.deep.equal({
      sqry: {
        workspace: {
          folders: [{ path: "./repo-a" }, { path: "./repo-b", name: "Repo B" }],
          classification: {
            sourceRoots: ["./repo-a"],
            memberFolders: ["./repo-b"],
            projectRootMode: "gitRoot",
          },
        },
        workspaceFile: "/path/to/proj.code-workspace",
      },
    });
    client.dispose();
  });
});
