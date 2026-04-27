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

// eslint-disable-next-line @typescript-eslint/no-var-requires
const proxyquire = require("proxyquire").noCallThru();

interface RecordedToken {
  isCancellationRequested: boolean;
  source: { cancel(): void; dispose(): void };
}

class FakeCancellationTokenSource {
  public token: { isCancellationRequested: boolean };
  public _disposed = false;
  constructor() {
    this.token = { isCancellationRequested: false };
  }
  cancel(): void {
    this.token.isCancellationRequested = true;
  }
  dispose(): void {
    this._disposed = true;
  }
}

interface FakeLanguageClientCalls {
  initOptions: unknown;
  workspaceFolderPin: unknown;
  receivedRequests: Array<{ method: string; params: unknown; token: { isCancellationRequested: boolean } }>;
}

function buildSqryClientModule(
  responsesByMethod: Map<string, (params: unknown) => unknown>,
): {
  SqryClient: typeof import("../src/sqryClient").SqryClient;
  calls: FakeLanguageClientCalls;
} {
  const calls: FakeLanguageClientCalls = {
    initOptions: undefined,
    workspaceFolderPin: undefined,
    receivedRequests: [],
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
      calls.initOptions = clientOptions.initializationOptions;
      calls.workspaceFolderPin = clientOptions.workspaceFolder;
    }
    start(): Promise<void> {
      return Promise.resolve();
    }
    stop(): Promise<void> {
      return Promise.resolve();
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
  const lcStub = {
    LanguageClient: FakeLanguageClient,
    CancellationTokenSource: FakeCancellationTokenSource,
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
      onDidChangeConfiguration: () => ({ dispose: () => undefined }),
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
  };

  // Force the binary resolver to a deterministic stub path.
  const configStub = {
    resolveConfig: async () => ({
      sqryPath: "sqry",
      limit: 200,
      timeoutMs: 1_000,
      indexTimeoutMs: 60_000,
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

  const { SqryClient } = proxyquire("../src/sqryClient", {
    vscode: vscodeStub,
    "vscode-languageclient/node": lcStub,
    "./config": configStub,
    "./indexQueue": indexQueueStub,
  }) as { SqryClient: typeof import("../src/sqryClient").SqryClient };

  return { SqryClient, calls };
}

const noopOutput = {
  appendLine: () => undefined,
  show: () => undefined,
} as unknown as import("vscode").OutputChannel;

describe("STEP_5 — SqryClient.getWorkspaceStatus", () => {
  it("returns the aggregate when the LSP supplies one", async () => {
    const responses = new Map<string, (params: unknown) => unknown>([
      [
        "sqry/indexStatus",
        () => ({
          status: {
            exists: true,
            supports_fuzzy: true,
            supports_relations: true,
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
            partial: true,
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

  it("repackages a non-aggregate response into a one-entry aggregate", async () => {
    const responses = new Map<string, (params: unknown) => unknown>([
      [
        "sqry/indexStatus",
        () => ({
          status: {
            exists: true,
            symbol_count: 42,
            path: "/single/folder",
            supports_fuzzy: true,
            supports_relations: false,
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
    await Promise.all([a, b]);
    client.dispose();
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
