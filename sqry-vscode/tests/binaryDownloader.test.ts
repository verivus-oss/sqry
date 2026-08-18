import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { EventEmitter } from "node:events";
import { PassThrough, Writable } from "node:stream";
import { expect } from "chai";
import proxyquireModule from "proxyquire";

const proxyquire = proxyquireModule.noCallThru();

// Minimal vscode stub for modules that import vscode
const vscodeStub = {
  __esModule: true,
  ExtensionMode: {
    Production: 1,
    Development: 2,
    Test: 3,
  },
  workspace: {
    getConfiguration: (section: string) => ({
      get: (_key: string, fallback: unknown) => fallback,
    }),
  },
  window: {
    createOutputChannel: () => ({
      appendLine: () => {},
      show: () => {},
      dispose: () => {},
    }),
  },
  Uri: {
    file: (p: string) => ({ fsPath: p }),
  },
};

function loadModule(overrides: Record<string, unknown> = {}) {
  return proxyquire("../src/binaryDownloader", {
    vscode: vscodeStub,
    ...overrides,
  });
}

class TestCancellationToken {
  public isCancellationRequested = false;
  public subscriptions = 0;
  public disposals = 0;
  private readonly listeners = new Set<() => void>();

  onCancellationRequested(listener: () => void): { dispose(): void } {
    this.subscriptions += 1;
    this.listeners.add(listener);
    return {
      dispose: () => {
        if (this.listeners.delete(listener)) {
          this.disposals += 1;
        }
      },
    };
  }

  cancel(): void {
    if (this.isCancellationRequested) {
      return;
    }
    this.isCancellationRequested = true;
    for (const listener of [...this.listeners]) {
      listener();
    }
  }
}

class TestRequest extends EventEmitter {
  public destroyCalls = 0;
  public destroyed = false;

  destroy(_error?: Error): this {
    this.destroyCalls += 1;
    this.destroyed = true;
    return this;
  }
}

class SynchronousWriteStream extends EventEmitter {
  public closed = false;
  public writable = true;
  public readonly chunks: Buffer[] = [];

  write(chunk: Buffer): boolean {
    this.chunks.push(Buffer.from(chunk));
    return true;
  }

  end(chunk?: Buffer): this {
    if (chunk) {
      this.write(chunk);
    }
    this.emit("finish");
    return this;
  }

  destroy(): this {
    this.closed = true;
    this.emit("close");
    return this;
  }
}

type TestResponse = PassThrough & {
  statusCode?: number;
  headers: Record<string, string | undefined>;
  resumeCalls: number;
  destroyCalls: number;
};

function makeResponse(
  statusCode: number,
  headers: Record<string, string | undefined> = {},
  autoDestroy = true,
): TestResponse {
  const response = new PassThrough({ autoDestroy }) as TestResponse;
  response.statusCode = statusCode;
  response.headers = headers;
  response.resumeCalls = 0;
  response.destroyCalls = 0;
  const resume = response.resume.bind(response);
  response.resume = () => {
    response.resumeCalls += 1;
    return resume();
  };
  const destroy = response.destroy.bind(response);
  response.destroy = ((error?: Error) => {
    response.destroyCalls += 1;
    return destroy(error);
  }) as typeof response.destroy;
  return response;
}

type DownloadScript = (
  callback: (response: TestResponse) => void,
  request: TestRequest,
) => void;

function loadDownloadHarness(scripts: DownloadScript[], overrides: Record<string, unknown> = {}) {
  const requests: TestRequest[] = [];
  const mod = loadModule({
    "node:https": {
      get: (_options: unknown, callback: (response: TestResponse) => void) => {
        const script = scripts.shift();
        if (!script) {
          throw new Error("unexpected HTTPS request");
        }
        const request = new TestRequest();
        requests.push(request);
        script(callback, request);
        return request;
      },
    },
    ...overrides,
  });
  return { mod, requests };
}

async function rejectWithin(promise: Promise<unknown>, timeoutMs = 250): Promise<Error> {
  return new Promise<Error>((resolve, reject) => {
    const timeout = setTimeout(() => {
      reject(new Error(`expected rejection within ${timeoutMs}ms`));
    }, timeoutMs);
    void promise.then(
      () => {
        clearTimeout(timeout);
        reject(new Error("expected promise to reject"));
      },
      (error: unknown) => {
        clearTimeout(timeout);
        resolve(error instanceof Error ? error : new Error(String(error)));
      },
    );
  });
}

function temporaryDestination(): { dir: string; destPath: string } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sqry-download-liveness-"));
  return { dir, destPath: path.join(dir, "download.tmp") };
}

function removeTemporaryDirectory(dir: string): void {
  fs.rmSync(dir, { recursive: true, force: true });
}

function createAttestationBundle(subjects: unknown[]) {
  const payload = Buffer.from(JSON.stringify({ subject: subjects }), "utf-8").toString("base64");
  return {
    mediaType: "application/vnd.dev.sigstore.bundle.v0.3+json",
    dsseEnvelope: {
      payload,
      payloadType: "application/vnd.in-toto+json",
      signatures: [],
    },
  };
}

function createOutputChannel() {
  const lines: string[] = [];
  return {
    channel: {
      appendLine: (line: string) => {
        lines.push(line);
      },
    },
    lines,
  };
}

describe("binaryDownloader", () => {
  describe("downloadWithProgress lifecycle", () => {
    const testTimeouts = { responseTimeoutMs: 25, idleTimeoutMs: 25 };

    it("settles and aborts when final response headers never arrive", async () => {
      const { dir, destPath } = temporaryDestination();
      try {
        const token = new TestCancellationToken();
        const { mod, requests } = loadDownloadHarness([() => undefined]);

        const error = await rejectWithin(
          mod.downloadWithProgress(
            "https://github.com/verivus-oss/sqry/releases/download/v29.0.3/SHA256SUMS.txt",
            destPath,
            undefined,
            token as unknown as import("vscode").CancellationToken,
            5,
            testTimeouts,
          ),
        );

        expect(error).to.be.instanceOf(mod.DownloadTimeoutError);
        expect(requests).to.have.length(1);
        expect(requests[0].destroyCalls).to.equal(1);
        expect(token.subscriptions).to.equal(1);
        expect(token.disposals).to.equal(1);
        expect(fs.existsSync(destPath)).to.equal(false);
      } finally {
        removeTemporaryDirectory(dir);
      }
    });

    it("registers cancellation before HTTPS returns a request", async () => {
      const { dir, destPath } = temporaryDestination();
      try {
        const token = new TestCancellationToken();
        const { mod, requests } = loadDownloadHarness([
          () => token.cancel(),
        ]);

        const error = await rejectWithin(
          mod.downloadWithProgress(
            "https://github.com/verivus-oss/sqry/releases/download/v29.0.3/SHA256SUMS.txt",
            destPath,
            undefined,
            token as unknown as import("vscode").CancellationToken,
            5,
            testTimeouts,
          ),
        );

        expect(error).to.be.instanceOf(mod.DownloadCancelledError);
        expect(requests).to.have.length(1);
        expect(requests[0].destroyCalls).to.equal(1);
        expect(token.disposals).to.equal(1);
      } finally {
        removeTemporaryDirectory(dir);
      }
    });

    it("settles an idle body, closes its stream, and removes the partial output", async () => {
      const { dir, destPath } = temporaryDestination();
      try {
        const token = new TestCancellationToken();
        const response = makeResponse(200, { "content-length": "10" });
        const { mod, requests } = loadDownloadHarness([
          (callback) => {
            callback(response);
            setTimeout(() => response.write(Buffer.from("partial")), 2);
          },
        ]);

        const error = await rejectWithin(
          mod.downloadWithProgress(
            "https://github.com/verivus-oss/sqry/releases/download/v29.0.3/SHA256SUMS.txt",
            destPath,
            undefined,
            token as unknown as import("vscode").CancellationToken,
            5,
            testTimeouts,
          ),
        );

        expect(error).to.be.instanceOf(mod.DownloadTimeoutError);
        expect(requests[0].destroyCalls).to.equal(1);
        expect(response.destroyed).to.equal(true);
        expect(fs.existsSync(destPath)).to.equal(false);
        expect(token.disposals).to.equal(1);
      } finally {
        removeTemporaryDirectory(dir);
      }
    });

    it("allows slow active progress without imposing a whole-download deadline", async () => {
      const { dir, destPath } = temporaryDestination();
      try {
        const token = new TestCancellationToken();
        const response = makeResponse(200, { "content-length": "3" });
        const progress: number[] = [];
        const { mod } = loadDownloadHarness([
          (callback) => {
            callback(response);
            setTimeout(() => response.write(Buffer.from("a")), 2);
            setTimeout(() => response.write(Buffer.from("b")), 12);
            setTimeout(() => response.end(Buffer.from("c")), 22);
          },
        ]);

        await mod.downloadWithProgress(
          "https://github.com/verivus-oss/sqry/releases/download/v29.0.3/SHA256SUMS.txt",
          destPath,
          (downloaded: number) => progress.push(downloaded),
          token as unknown as import("vscode").CancellationToken,
          5,
          { responseTimeoutMs: 25, idleTimeoutMs: 15 },
        );

        expect(fs.readFileSync(destPath, "utf8")).to.equal("abc");
        expect(progress).to.deep.equal([1, 2, 3]);
        expect(token.disposals).to.equal(1);
      } finally {
        removeTemporaryDirectory(dir);
      }
    });

    it("owns and aborts every redirect hop when the response-chain deadline expires", async () => {
      const { dir, destPath } = temporaryDestination();
      try {
        const token = new TestCancellationToken();
        const redirect = makeResponse(302, { location: "https://github.com/redirected" });
        const { mod, requests } = loadDownloadHarness([
          (callback) => callback(redirect),
          () => undefined,
        ]);

        const error = await rejectWithin(
          mod.downloadWithProgress(
            "https://github.com/verivus-oss/sqry/releases/download/v29.0.3/SHA256SUMS.txt",
            destPath,
            undefined,
            token as unknown as import("vscode").CancellationToken,
            5,
            testTimeouts,
          ),
        );

        expect(error).to.be.instanceOf(mod.DownloadTimeoutError);
        expect(redirect.resumeCalls).to.equal(1);
        expect(requests).to.have.length(2);
        expect(requests[0].destroyCalls).to.equal(1);
        expect(requests[1].destroyCalls).to.equal(1);
        expect(redirect.destroyed).to.equal(true);
        expect(token.subscriptions).to.equal(1);
        expect(token.disposals).to.equal(1);
      } finally {
        removeTemporaryDirectory(dir);
      }
    });

    it("cancels the active redirect child without starting another attempt", async () => {
      const { dir, destPath } = temporaryDestination();
      try {
        const token = new TestCancellationToken();
        const redirect = makeResponse(302, { location: "https://github.com/redirected" });
        const { mod, requests } = loadDownloadHarness([
          (callback) => callback(redirect),
          () => undefined,
        ]);
        const pending = mod.downloadWithProgress(
          "https://github.com/verivus-oss/sqry/releases/download/v29.0.3/SHA256SUMS.txt",
          destPath,
          undefined,
          token as unknown as import("vscode").CancellationToken,
          5,
          { responseTimeoutMs: 100, idleTimeoutMs: 25 },
        );
        setTimeout(() => token.cancel(), 5);

        const error = await rejectWithin(pending);

        expect(error).to.be.instanceOf(mod.DownloadCancelledError);
        expect(requests).to.have.length(2);
        expect(requests[0].destroyCalls).to.equal(1);
        expect(requests[1].destroyCalls).to.equal(1);
      } finally {
        removeTemporaryDirectory(dir);
      }
    });

    it("closes a lingering redirect parent when the redirected child succeeds", async () => {
      const { dir, destPath } = temporaryDestination();
      try {
        const token = new TestCancellationToken();
        const redirect = makeResponse(302, { location: "https://github.com/redirected" });
        const child = makeResponse(200, { "content-length": "2" });
        const { mod, requests } = loadDownloadHarness([
          (callback) => callback(redirect),
          (callback) => {
            callback(child);
            setTimeout(() => child.end(Buffer.from("ok")), 2);
          },
        ]);

        await mod.downloadWithProgress(
          "https://github.com/verivus-oss/sqry/releases/download/v29.0.3/SHA256SUMS.txt",
          destPath,
          undefined,
          token as unknown as import("vscode").CancellationToken,
          5,
          { responseTimeoutMs: 100, idleTimeoutMs: 25 },
        );

        expect(fs.readFileSync(destPath, "utf8")).to.equal("ok");
        expect(requests).to.have.length(2);
        expect(redirect.resumeCalls).to.equal(1);
        expect(redirect.destroyed).to.equal(true);
        expect(redirect.destroyCalls).to.equal(1);
        expect(requests[0].destroyCalls).to.equal(1);
        expect(requests[1].destroyCalls).to.equal(0);
        expect(token.disposals).to.equal(1);
        expect(() => redirect.emit("error", new Error("late redirect parent error"))).to.not.throw();
      } finally {
        removeTemporaryDirectory(dir);
      }
    });

    it("preserves the completed child when synchronous transport callbacks finish before parent return", async () => {
      const { dir, destPath } = temporaryDestination();
      try {
        const redirect = makeResponse(302, { location: "https://github.com/redirected" });
        const child = makeResponse(200, { "content-length": "2" }, false);
        const synchronousStream = new SynchronousWriteStream();
        const { mod, requests } = loadDownloadHarness(
          [
            (callback) => callback(redirect),
            (callback) => {
              callback(child);
              child.end(Buffer.from("ok"));
            },
          ],
          {
            "node:fs": {
              createWriteStream: () => synchronousStream as unknown as fs.WriteStream,
            },
          },
        );

        await mod.downloadWithProgress(
          "https://github.com/verivus-oss/sqry/releases/download/v29.0.3/SHA256SUMS.txt",
          destPath,
          undefined,
          undefined,
          5,
          { responseTimeoutMs: 100, idleTimeoutMs: 25 },
        );

        expect(Buffer.concat(synchronousStream.chunks).toString("utf8")).to.equal("ok");
        expect(requests).to.have.length(2);
        expect(redirect.destroyCalls).to.equal(1);
        expect(requests[0].destroyCalls).to.equal(1);
        expect(child.destroyCalls).to.equal(0);
        expect(requests[1].destroyCalls).to.equal(0);
        child.destroy();
      } finally {
        removeTemporaryDirectory(dir);
      }
    });

    it("absorbs a late redirect-parent response error and closes every live hop", async () => {
      const { dir, destPath } = temporaryDestination();
      try {
        const redirect = makeResponse(302, { location: "https://github.com/redirected" });
        const { mod, requests } = loadDownloadHarness([
          (callback) => callback(redirect),
          () => undefined,
        ]);
        const pending = mod.downloadWithProgress(
          "https://github.com/verivus-oss/sqry/releases/download/v29.0.3/SHA256SUMS.txt",
          destPath,
          undefined,
          undefined,
          5,
          { responseTimeoutMs: 100, idleTimeoutMs: 25 },
        );

        expect(requests).to.have.length(2);
        expect(() => redirect.emit("error", new Error("redirect parent failed"))).to.not.throw();
        const error = await rejectWithin(pending);

        expect(error.message).to.include("redirect parent failed");
        expect(requests[0].destroyCalls).to.equal(1);
        expect(requests[1].destroyCalls).to.equal(1);
      } finally {
        removeTemporaryDirectory(dir);
      }
    });

    it("absorbs late transport errors after the terminal timeout", async () => {
      const { dir, destPath } = temporaryDestination();
      try {
        const { mod, requests } = loadDownloadHarness([() => undefined]);
        const pending = mod.downloadWithProgress(
          "https://github.com/verivus-oss/sqry/releases/download/v29.0.3/SHA256SUMS.txt",
          destPath,
          undefined,
          undefined,
          5,
          testTimeouts,
        );

        await rejectWithin(pending);
        const unhandled: unknown[] = [];
        const onUnhandled = (reason: unknown) => unhandled.push(reason);
        process.on("unhandledRejection", onUnhandled);
        try {
          requests[0].emit("error", new Error("late request error"));
          await new Promise((resolve) => setTimeout(resolve, 0));
        } finally {
          process.off("unhandledRejection", onUnhandled);
        }

        expect(unhandled).to.deep.equal([]);
        expect(requests[0].destroyCalls).to.equal(1);
      } finally {
        removeTemporaryDirectory(dir);
      }
    });

    it("removes a partial output only after its write stream emits close", async () => {
      const response = makeResponse(200, { "content-length": "10" });
      const delayedStream = new Writable({
        write(_chunk, _encoding, callback) {
          callback();
        },
      });
      let closed = false;
      Object.defineProperty(delayedStream, "closed", {
        configurable: true,
        get: () => closed,
      });
      delayedStream.destroy = ((_error?: Error) => {
        setTimeout(() => {
          closed = true;
          delayedStream.emit("close");
        }, 5);
        return delayedStream;
      }) as typeof delayedStream.destroy;
      let unlinkCalls = 0;
      const { mod } = loadDownloadHarness(
        [
          (callback) => {
            callback(response);
            setTimeout(() => response.write(Buffer.from("partial")), 2);
          },
        ],
        {
          "node:fs": {
            createWriteStream: () => delayedStream,
            existsSync: () => true,
            unlinkSync: () => {
              expect(closed).to.equal(true);
              unlinkCalls += 1;
            },
          },
        },
      );

      await rejectWithin(
        mod.downloadWithProgress(
          "https://github.com/verivus-oss/sqry/releases/download/v29.0.3/SHA256SUMS.txt",
          "/virtual/partial.tmp",
          undefined,
          undefined,
          5,
          testTimeouts,
        ),
      );

      expect(unlinkCalls).to.equal(1);
    });

    it("retains HTTPS, host-allowlist, redirect, and redirect-limit protections", async () => {
      const { dir, destPath } = temporaryDestination();
      try {
        const directHarness = loadDownloadHarness([]);
        const nonHttps = await rejectWithin(
          directHarness.mod.downloadWithProgress("http://github.com/asset", destPath),
        );
        expect(nonHttps.message).to.include("non-HTTPS");
        expect(directHarness.requests).to.have.length(0);

        const disallowed = await rejectWithin(
          directHarness.mod.downloadWithProgress("https://example.com/asset", destPath),
        );
        expect(disallowed.message).to.include("allowlist");
        expect(directHarness.requests).to.have.length(0);

        const insecureRedirect = makeResponse(302, { location: "http://example.com/asset" });
        const redirectHarness = loadDownloadHarness([
          (callback) => callback(insecureRedirect),
        ]);
        const redirectError = await rejectWithin(
          redirectHarness.mod.downloadWithProgress(
            "https://github.com/asset",
            destPath,
            undefined,
            undefined,
            5,
            testTimeouts,
          ),
        );
        expect(redirectError.message).to.include("non-HTTPS");
        expect(insecureRedirect.resumeCalls).to.equal(1);

        const limitedRedirect = makeResponse(302, { location: "https://github.com/next" });
        const limitHarness = loadDownloadHarness([
          (callback) => callback(limitedRedirect),
        ]);
        const limitError = await rejectWithin(
          limitHarness.mod.downloadWithProgress(
            "https://github.com/asset",
            destPath,
            undefined,
            undefined,
            0,
            testTimeouts,
          ),
        );
        expect(limitError.message).to.include("Too many redirects");
        expect(limitedRedirect.resumeCalls).to.equal(1);
      } finally {
        removeTemporaryDirectory(dir);
      }
    });

    it("stops checksum candidate fallback immediately when the user cancels", async () => {
      const { dir } = temporaryDestination();
      try {
        const token = new TestCancellationToken();
        const { mod, requests } = loadDownloadHarness([
          () => token.cancel(),
        ]);
        const output = createOutputChannel().channel;
        const context = {
          globalStorageUri: { fsPath: dir },
          extensionMode: vscodeStub.ExtensionMode.Production,
        } as unknown as import("vscode").ExtensionContext;

        const error = await rejectWithin(
          mod.downloadBinary(
            context,
            output as unknown as import("vscode").OutputChannel,
            token as unknown as import("vscode").CancellationToken,
          ),
        );

        expect(error).to.be.instanceOf(mod.DownloadCancelledError);
        expect(requests).to.have.length(1);
        expect(fs.existsSync(path.join(dir, "download.lock"))).to.equal(false);
      } finally {
        removeTemporaryDirectory(dir);
      }
    });

    it("retains 404 fallback but stops the next candidate when it is cancelled", async () => {
      const { dir } = temporaryDestination();
      try {
        const token = new TestCancellationToken();
        const missing = makeResponse(404);
        const { mod, requests } = loadDownloadHarness([
          (callback) => callback(missing),
          () => token.cancel(),
        ]);
        const output = createOutputChannel().channel;
        const context = {
          globalStorageUri: { fsPath: dir },
          extensionMode: vscodeStub.ExtensionMode.Production,
        } as unknown as import("vscode").ExtensionContext;

        const error = await rejectWithin(
          mod.downloadBinary(
            context,
            output as unknown as import("vscode").OutputChannel,
            token as unknown as import("vscode").CancellationToken,
          ),
        );

        expect(error).to.be.instanceOf(mod.DownloadCancelledError);
        expect(requests).to.have.length(2);
        expect(fs.existsSync(path.join(dir, "download.lock"))).to.equal(false);
      } finally {
        removeTemporaryDirectory(dir);
      }
    });
  });

  describe("release asset selection", () => {
    it("prefers current checksum manifest name with legacy fallback", () => {
      const mod = loadModule();
      expect(mod.getChecksumAssetCandidates()).to.deep.equal([
        "SHA256SUMS.txt",
        "CHECKSUMS.sha256",
      ]);
    });

    it("prefers the shared attestation bundle with legacy per-asset fallback", () => {
      const mod = loadModule();
      expect(mod.getBundleAssetCandidates("sqry-linux-x86_64")).to.deep.equal([
        "release-artifacts.attestation.json",
        "sqry-linux-x86_64.bundle",
      ]);
    });
  });

  describe("download version candidates", () => {
    it("keeps production exact-match behavior", () => {
      const mod = loadModule();
      expect(mod.getDownloadVersionCandidates("8.0.6", vscodeStub.ExtensionMode.Production)).to.deep.equal([
        "8.0.6",
      ]);
    });

    it("falls back through lower patch versions in development mode", () => {
      const mod = loadModule();
      expect(mod.getDownloadVersionCandidates("8.0.2", vscodeStub.ExtensionMode.Development)).to.deep.equal([
        "8.0.2",
        "8.0.1",
        "8.0.0",
      ]);
    });

    it("keeps the raw version when it cannot derive patch fallbacks", () => {
      const mod = loadModule();
      expect(mod.getDownloadVersionCandidates("8.0.6-dev", vscodeStub.ExtensionMode.Development)).to.deep.equal([
        "8.0.6-dev",
      ]);
    });
  });

  describe("sigstore verifier options", () => {
    it("builds a persistent tuf cache path and raised timeout", () => {
      const mod = loadModule();
      expect(mod.buildSigstoreVerifyOptions({ fsPath: "/tmp/sqry-storage" })).to.deep.equal({
        timeout: 30000,
        tufCachePath: "/tmp/sqry-storage/sigstore-tuf-cache",
      });
    });
  });

  describe("withTemporarySigstoreProxyEnv()", () => {
    const proxyEnvKeys = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] as const;
    const originalProxyEnv = new Map<string, string | undefined>();

    beforeEach(() => {
      for (const proxyEnvKey of proxyEnvKeys) {
        originalProxyEnv.set(proxyEnvKey, process.env[proxyEnvKey]);
        delete process.env[proxyEnvKey];
      }
    });

    afterEach(() => {
      for (const proxyEnvKey of proxyEnvKeys) {
        const originalValue = originalProxyEnv.get(proxyEnvKey);
        if (originalValue === undefined) {
          delete process.env[proxyEnvKey];
        } else {
          process.env[proxyEnvKey] = originalValue;
        }
      }
      originalProxyEnv.clear();
    });

    it("temporarily seeds proxy env vars when only the VS Code proxy is configured", async () => {
      const mod = loadModule();
      await mod.withTemporarySigstoreProxyEnv("http://proxy.internal:3128", async () => {
        for (const proxyEnvKey of proxyEnvKeys) {
          expect(process.env[proxyEnvKey]).to.equal("http://proxy.internal:3128");
        }
      });

      for (const proxyEnvKey of proxyEnvKeys) {
        expect(process.env[proxyEnvKey]).to.equal(undefined);
      }
    });

    it("does not override an existing proxy env var", async () => {
      const mod = loadModule();
      process.env.HTTPS_PROXY = "http://existing.proxy:8080";

      await mod.withTemporarySigstoreProxyEnv("http://proxy.internal:3128", async () => {
        expect(process.env.HTTPS_PROXY).to.equal("http://existing.proxy:8080");
        expect(process.env.HTTP_PROXY).to.equal("http://proxy.internal:3128");
      });

      expect(process.env.HTTPS_PROXY).to.equal("http://existing.proxy:8080");
      expect(process.env.HTTP_PROXY).to.equal(undefined);
    });
  });

  describe("detectPlatform()", () => {
    it("returns correct asset for linux-x64", () => {
      const mod = loadModule();
      // Override process.platform and process.arch for testing
      const origPlatform = Object.getOwnPropertyDescriptor(process, "platform");
      const origArch = Object.getOwnPropertyDescriptor(process, "arch");

      Object.defineProperty(process, "platform", { value: "linux", configurable: true });
      Object.defineProperty(process, "arch", { value: "x64", configurable: true });

      try {
        const result = mod.detectPlatform();
        expect(result.asset).to.equal("sqry-linux-x86_64-musl");
        expect(result.binaryName).to.equal("sqry");
        expect(result.archive).to.equal(undefined);
      } finally {
        if (origPlatform) Object.defineProperty(process, "platform", origPlatform);
        if (origArch) Object.defineProperty(process, "arch", origArch);
      }
    });

    it("returns correct asset for win32-x64", () => {
      const mod = loadModule();
      const origPlatform = Object.getOwnPropertyDescriptor(process, "platform");
      const origArch = Object.getOwnPropertyDescriptor(process, "arch");

      Object.defineProperty(process, "platform", { value: "win32", configurable: true });
      Object.defineProperty(process, "arch", { value: "x64", configurable: true });

      try {
        const result = mod.detectPlatform();
        expect(result.asset).to.equal("sqry-{version}-windows-x86_64.zip");
        expect(result.binaryName).to.equal("sqry.exe");
        expect(result.archive).to.deep.equal({ format: "zip", memberBinary: "sqry.exe" });
      } finally {
        if (origPlatform) Object.defineProperty(process, "platform", origPlatform);
        if (origArch) Object.defineProperty(process, "arch", origArch);
      }
    });

    it("returns correct asset for darwin-arm64", () => {
      const mod = loadModule();
      const origPlatform = Object.getOwnPropertyDescriptor(process, "platform");
      const origArch = Object.getOwnPropertyDescriptor(process, "arch");

      Object.defineProperty(process, "platform", { value: "darwin", configurable: true });
      Object.defineProperty(process, "arch", { value: "arm64", configurable: true });

      try {
        const result = mod.detectPlatform();
        expect(result.asset).to.equal("sqry-macos-arm64");
        expect(result.binaryName).to.equal("sqry");
      } finally {
        if (origPlatform) Object.defineProperty(process, "platform", origPlatform);
        if (origArch) Object.defineProperty(process, "arch", origArch);
      }
    });

    it("returns correct asset for darwin-x64", () => {
      const mod = loadModule();
      const origPlatform = Object.getOwnPropertyDescriptor(process, "platform");
      const origArch = Object.getOwnPropertyDescriptor(process, "arch");

      Object.defineProperty(process, "platform", { value: "darwin", configurable: true });
      Object.defineProperty(process, "arch", { value: "x64", configurable: true });

      try {
        const result = mod.detectPlatform();
        expect(result.asset).to.equal("sqry-macos-x86_64");
        expect(result.binaryName).to.equal("sqry");
      } finally {
        if (origPlatform) Object.defineProperty(process, "platform", origPlatform);
        if (origArch) Object.defineProperty(process, "arch", origArch);
      }
    });

    it("returns correct asset for linux-arm64", () => {
      const mod = loadModule();
      const origPlatform = Object.getOwnPropertyDescriptor(process, "platform");
      const origArch = Object.getOwnPropertyDescriptor(process, "arch");

      Object.defineProperty(process, "platform", { value: "linux", configurable: true });
      Object.defineProperty(process, "arch", { value: "arm64", configurable: true });

      try {
        const result = mod.detectPlatform();
        expect(result.asset).to.equal("sqry-linux-arm64-musl");
        expect(result.binaryName).to.equal("sqry");
        expect(result.archive).to.equal(undefined);
      } finally {
        if (origPlatform) Object.defineProperty(process, "platform", origPlatform);
        if (origArch) Object.defineProperty(process, "arch", origArch);
      }
    });
  });

  describe("extractArchiveMembers()", () => {
    let tmpDir: string;
    beforeEach(() => { tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "sqry-zip-")); });
    afterEach(() => { fs.rmSync(tmpDir, { recursive: true, force: true }); });

    it("extracts the launch binary + DLLs and skips other executables", () => {
      const { zipSync } = require("fflate");
      const enc = (s: string) => new TextEncoder().encode(s);
      const zipped = zipSync({
        "sqry.exe": enc("SQRY-BINARY"),
        "libstdc++-6.dll": enc("STDCPP"),
        "libgcc_s_seh-1.dll": enc("GCC"),
        "libwinpthread-1.dll": enc("PTHREAD"),
        "sqry-mcp.exe": enc("MCP-BINARY"),   // sibling exe: must be skipped
      });
      const zipPath = path.join(tmpDir, "artifact.zip");
      fs.writeFileSync(zipPath, Buffer.from(zipped));
      const dest = path.join(tmpDir, "out");
      fs.mkdirSync(dest);
      const { channel } = createOutputChannel();

      const mod = loadModule();
      mod.extractArchiveMembers(zipPath, dest, "sqry.exe", channel);

      const written = fs.readdirSync(dest).sort();
      expect(written).to.deep.equal([
        "libgcc_s_seh-1.dll", "libstdc++-6.dll", "libwinpthread-1.dll", "sqry.exe",
      ]);
      expect(fs.readFileSync(path.join(dest, "sqry.exe"), "utf-8")).to.equal("SQRY-BINARY");
      expect(fs.existsSync(path.join(dest, "sqry-mcp.exe"))).to.equal(false);
    });

    it("throws when the archive lacks the expected binary", () => {
      const { zipSync } = require("fflate");
      const zipped = zipSync({ "libstdc++-6.dll": new TextEncoder().encode("x") });
      const zipPath = path.join(tmpDir, "artifact.zip");
      fs.writeFileSync(zipPath, Buffer.from(zipped));
      const { channel } = createOutputChannel();
      const mod = loadModule();
      expect(() => mod.extractArchiveMembers(zipPath, tmpDir, "sqry.exe", channel))
        .to.throw(/did not contain the expected binary/);
    });
  });

  describe("parseChecksumForAsset()", () => {
    const sampleChecksums = [
      "abc123def456789012345678901234567890123456789012345678901234abcd  sqry-linux-x86_64",
      "def456789012345678901234567890123456789012345678901234567890abcd  sqry-windows-x86_64.exe",
      "789012345678901234567890123456789012345678901234567890abcdef1234  sqry-macos-arm64",
    ].join("\n");

    it("extracts correct hash for linux asset", () => {
      const mod = loadModule();
      const hash = mod.parseChecksumForAsset(sampleChecksums, "sqry-linux-x86_64");
      expect(hash).to.equal("abc123def456789012345678901234567890123456789012345678901234abcd");
    });

    it("extracts correct hash for windows asset", () => {
      const mod = loadModule();
      const hash = mod.parseChecksumForAsset(sampleChecksums, "sqry-windows-x86_64.exe");
      expect(hash).to.equal("def456789012345678901234567890123456789012345678901234567890abcd");
    });

    it("extracts correct hash for macos asset", () => {
      const mod = loadModule();
      const hash = mod.parseChecksumForAsset(sampleChecksums, "sqry-macos-arm64");
      expect(hash).to.equal("789012345678901234567890123456789012345678901234567890abcdef1234");
    });

    it("throws on missing asset entry", () => {
      const mod = loadModule();
      expect(() => mod.parseChecksumForAsset(sampleChecksums, "sqry-nonexistent")).to.throw(
        'Checksum not found for asset "sqry-nonexistent"'
      );
    });

    it("throws on empty checksum file", () => {
      const mod = loadModule();
      expect(() => mod.parseChecksumForAsset("", "sqry-linux-x86_64")).to.throw(
        "Checksum not found"
      );
    });

    it("handles sha256sum format with asterisk prefix", () => {
      const mod = loadModule();
      const content = "abc123def456789012345678901234567890123456789012345678901234abcd *sqry-linux-x86_64";
      const hash = mod.parseChecksumForAsset(content, "sqry-linux-x86_64");
      expect(hash).to.equal("abc123def456789012345678901234567890123456789012345678901234abcd");
    });
  });

  describe("parseContentLengthHeader()", () => {
    it("parses a numeric header string", () => {
      const mod = loadModule();
      expect(mod.parseContentLengthHeader("123")).to.equal(123);
    });

    it("uses the first value from an array header", () => {
      const mod = loadModule();
      expect(mod.parseContentLengthHeader(["456", "789"])).to.equal(456);
    });

    it("defaults to zero for a missing header", () => {
      const mod = loadModule();
      expect(mod.parseContentLengthHeader(undefined)).to.equal(0);
    });
  });

  describe("certificate identity candidates", () => {
    it("keeps the current provenance identity synchronized with the public workflow control surface", () => {
      const mod = loadModule();
      const repoRoot = path.resolve(__dirname, "..", "..");
      const publicWorkflowCandidates = [
        path.join(repoRoot, ".github", "workflows-public", "release-distribute.yml"),
        path.join(repoRoot, ".github", "workflows", "release-distribute.yml"),
      ];
      const identities = mod.getCertificateIdentityCandidates("8.0.2");

      const workflowPath = publicWorkflowCandidates.find((candidate) => fs.existsSync(candidate));
      expect(workflowPath, "current public release workflow control surface must exist").to.not.equal(
        undefined,
      );
      // The main-only allowlist is only sound because the distribution workflow
      // refuses to run on any ref but refs/heads/main (a workflow_dispatch from a
      // tag would otherwise attest a tag identity this allowlist rejects). Assert
      // that control-surface guard exists so the two cannot drift apart.
      const workflowSource = fs.readFileSync(workflowPath as string, "utf-8");
      expect(workflowSource).to.contain("Enforce main-branch signing identity");
      expect(workflowSource).to.match(
        /GITHUB_REF["}\s]*!=\s*"refs\/heads\/main"|"refs\/heads\/main"\s*\]\]/,
      );
      // Every real release bundle (v7 through v29) is signed on `@refs/heads/main`,
      // never on a tag ref, so only the main-branch identities are trusted.
      expect(identities[0]).to.equal(
        "https://github.com/verivus-oss/sqry/.github/workflows/release-distribute.yml@refs/heads/main",
      );
      expect(identities[1]).to.equal(
        "https://github.com/verivus-oss/sqry/.github/workflows/oss-distribute.yml@refs/heads/main",
      );
      for (const identity of identities) {
        expect(identity).to.match(
          /^https:\/\/github\.com\/verivus-oss\/sqry\/\.github\/workflows\/(release-distribute|oss-distribute)\.yml@refs\/heads\/main$/,
          `unexpectedly broad provenance identity: ${identity}`,
        );
      }
    });

    it("trusts only the main-branch identities of the two distribution workflows", () => {
      const mod = loadModule();
      // No tag-ref candidates: the Stage 6 workflow runs via repository_dispatch
      // on the default branch, so a tag identity never matches a real bundle.
      expect(mod.getCertificateIdentityCandidates("8.0.2")).to.deep.equal([
        "https://github.com/verivus-oss/sqry/.github/workflows/release-distribute.yml@refs/heads/main",
        "https://github.com/verivus-oss/sqry/.github/workflows/oss-distribute.yml@refs/heads/main",
      ]);
    });

    it("does not vary the identity set with the release version", () => {
      const mod = loadModule();
      // Identity is version-independent by design; the asset-to-version binding
      // is enforced separately by the DSSE subject-digest check.
      expect(mod.getCertificateIdentityCandidates("8.0.2")).to.deep.equal(
        mod.getCertificateIdentityCandidates("29.0.6"),
      );
    });
  });

  describe("verifyCosignBundleWithIdentities()", () => {
    it("falls back from the current workflow identity to the legacy one", async () => {
      const mod = loadModule();
      const { channel } = createOutputChannel();
      const attempted: string[] = [];

      // Older pinned versions were signed by the legacy `oss-distribute.yml`, so
      // when the current `release-distribute.yml` identity does not verify the
      // check must fall through to the legacy identity rather than fail.
      await mod.verifyCosignBundleWithIdentities(
        mod.getCertificateIdentityCandidates("8.0.2"),
        channel,
        async (identity: string) => {
          attempted.push(identity);
          if (identity.includes("oss-distribute.yml@refs/heads/main")) {
            return;
          }
          throw new Error(`certificate identity error - expected legacy, got ${identity}`);
        },
      );

      expect(attempted).to.include(
        "https://github.com/verivus-oss/sqry/.github/workflows/release-distribute.yml@refs/heads/main",
      );
      expect(attempted.at(-1)).to.equal(
        "https://github.com/verivus-oss/sqry/.github/workflows/oss-distribute.yml@refs/heads/main",
      );
    });

    it("succeeds immediately when the first identity matches", async () => {
      const mod = loadModule();
      const { channel } = createOutputChannel();
      const attempted: string[] = [];

      await mod.verifyCosignBundleWithIdentities(
        ["https://github.com/verivus-oss/sqry/.github/workflows/oss-distribute.yml@refs/heads/main"],
        channel,
        async (identity: string) => {
          attempted.push(identity);
        },
      );

      expect(attempted).to.deep.equal([
        "https://github.com/verivus-oss/sqry/.github/workflows/oss-distribute.yml@refs/heads/main",
      ]);
    });

    it("fails closed when no allowlisted identity verifies", async () => {
      const mod = loadModule();
      const { channel } = createOutputChannel();

      try {
        await mod.verifyCosignBundleWithIdentities(
          mod.getCertificateIdentityCandidates("8.0.2"),
          channel,
          async (identity: string) => {
            throw new Error(`certificate identity error for ${identity}`);
          },
        );
        expect.fail("Expected verification failure");
      } catch (error) {
        expect((error as Error).message).to.contain("Cosign verification failed for all allowlisted identities");
        expect((error as Error).message).to.contain("release-distribute.yml@refs/heads/main");
        expect((error as Error).message).to.contain("oss-distribute.yml@refs/heads/main");
      }
    });
  });

  describe("verifyAttestationSubject()", () => {
    it("accepts a DSSE subject matching the release asset digest", () => {
      const mod = loadModule();
      const bundle = createAttestationBundle([
        {
          name: "sqry-linux-x86_64",
          digest: {
            sha256: "D14809155BA2475E1A2967E40031E2BF3DC69F3FBA64450B6F5BEFE2F9457D9E",
          },
        },
      ]);

      mod.verifyAttestationSubject(
        bundle,
        "sqry-linux-x86_64",
        "d14809155ba2475e1a2967e40031e2bf3dc69f3fba64450b6f5befe2f9457d9e",
      );
    });

    it("rejects a DSSE subject digest mismatch", () => {
      const mod = loadModule();
      const bundle = createAttestationBundle([
        {
          name: "sqry-linux-x86_64",
          digest: {
            sha256: "d14809155ba2475e1a2967e40031e2bf3dc69f3fba64450b6f5befe2f9457d9e",
          },
        },
      ]);

      expect(() => {
        mod.verifyAttestationSubject(
          bundle,
          "sqry-linux-x86_64",
          "0000000000000000000000000000000000000000000000000000000000000000",
        );
      }).to.throw("Sigstore DSSE subject digest mismatch for sqry-linux-x86_64");
    });

    it("rejects a DSSE bundle that does not cover the requested release asset", () => {
      const mod = loadModule();
      const bundle = createAttestationBundle([
        {
          name: "sqry-macos-arm64",
          digest: {
            sha256: "d14809155ba2475e1a2967e40031e2bf3dc69f3fba64450b6f5befe2f9457d9e",
          },
        },
      ]);

      expect(() => {
        mod.verifyAttestationSubject(
          bundle,
          "sqry-linux-x86_64",
          "d14809155ba2475e1a2967e40031e2bf3dc69f3fba64450b6f5befe2f9457d9e",
        );
      }).to.throw("Sigstore DSSE bundle does not attest release asset sqry-linux-x86_64");
    });
  });

  describe("verifyCosignBundle()", () => {
    let tmpDir: string;

    beforeEach(() => {
      tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "sqry-test-"));
    });

    afterEach(() => {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    });

    it("verifies release-wide DSSE attestations without passing binary bytes as payload", async () => {
      const binaryPath = path.join(tmpDir, "sqry-linux-x86_64");
      fs.writeFileSync(binaryPath, "release binary");
      const expectedSha256 = crypto.createHash("sha256").update("release binary").digest("hex");

      const bundlePath = path.join(tmpDir, "release-artifacts.attestation.json");
      fs.writeFileSync(
        bundlePath,
        JSON.stringify(createAttestationBundle([
          {
            name: "sqry-linux-x86_64",
            digest: {
              sha256: expectedSha256,
            },
          },
        ])),
      );

      const verifyCalls: unknown[][] = [];
      const mod = loadModule({
        sigstore: {
          verify: async (...args: unknown[]) => {
            verifyCalls.push(args);
          },
        },
      });
      const { channel } = createOutputChannel();

      await mod.verifyCosignBundle(
        binaryPath,
        bundlePath,
        "19.0.4",
        channel,
        { fsPath: tmpDir },
        "sqry-linux-x86_64",
        expectedSha256,
      );

      expect(verifyCalls).to.have.length(1);
      expect(verifyCalls[0]).to.have.length(2);
      expect(Buffer.isBuffer(verifyCalls[0][1])).to.equal(false);
      expect(verifyCalls[0][1]).to.include({
        certificateIssuer: "https://token.actions.githubusercontent.com",
        certificateIdentityURI: "https://github.com/verivus-oss/sqry/.github/workflows/release-distribute.yml@refs/heads/main",
      });
    });

    const writeCosignBundle = (dir: string) => {
      const binaryPath = path.join(dir, "sqry-linux-x86_64");
      fs.writeFileSync(binaryPath, "release binary");
      const sha256 = crypto.createHash("sha256").update("release binary").digest("hex");
      const bundlePath = path.join(dir, "sqry-linux-x86_64.bundle");
      // Non-DSSE cosign bundle so verification routes through sigstore.verify(bundle, bytes).
      fs.writeFileSync(bundlePath, JSON.stringify({
        mediaType: "application/vnd.dev.sigstore.bundle.v0.3+json",
        verificationMaterial: {},
        messageSignature: {},
      }));
      return { binaryPath, bundlePath, sha256 };
    };

    it("degrades to SHA-256 acceptance on a persistent Sigstore trust-root failure when the checksum was verified", async () => {
      const { binaryPath, bundlePath, sha256 } = writeCosignBundle(tmpDir);
      const mod = loadModule({
        sigstore: { verify: async () => { throw new Error("root was signed by 0/3 keys"); } },
      });
      const { channel, lines } = createOutputChannel();

      // Must NOT throw: SHA-256 was already verified, so the persistent TUF
      // infrastructure failure degrades to acceptance with a warning.
      await mod.verifyCosignBundle(
        binaryPath, bundlePath, "28.0.1", channel, { fsPath: tmpDir }, "sqry-linux-x86_64", sha256,
      );
      expect(lines.join("\n")).to.match(/provenance could NOT be verified/i);
      expect(lines.join("\n")).to.match(/SHA-256 was verified/i);
    });

    it("rejects on a persistent Sigstore trust-root failure when there is no checksum anchor", async () => {
      const { binaryPath, bundlePath } = writeCosignBundle(tmpDir);
      const mod = loadModule({
        sigstore: { verify: async () => { throw new Error("root was signed by 0/3 keys"); } },
      });
      const { channel } = createOutputChannel();
      let threw = false;
      try {
        // No expectedSha256 argument -> no integrity anchor -> stay fatal.
        await mod.verifyCosignBundle(binaryPath, bundlePath, "28.0.1", channel, { fsPath: tmpDir }, "sqry-linux-x86_64");
      } catch (error) {
        threw = true;
        expect((error as Error).message).to.match(/Download rejected/);
      }
      expect(threw).to.equal(true);
    });

    it("still rejects a genuine signature/identity mismatch even with a verified checksum", async () => {
      const { binaryPath, bundlePath, sha256 } = writeCosignBundle(tmpDir);
      const mod = loadModule({
        sigstore: { verify: async () => { throw new Error("certificate identity error"); } },
      });
      const { channel } = createOutputChannel();
      let threw = false;
      try {
        await mod.verifyCosignBundle(
          binaryPath, bundlePath, "28.0.1", channel, { fsPath: tmpDir }, "sqry-linux-x86_64", sha256,
        );
      } catch (error) {
        threw = true;
        expect((error as Error).message).to.match(/Download rejected/);
      }
      expect(threw).to.equal(true);
    });
  });

  describe("isRecoverableSigstoreTufError()", () => {
    it("classifies stale TUF root errors as retryable", () => {
      const mod = loadModule();
      expect(mod.isRecoverableSigstoreTufError(new Error("root was signed by 0/3 keys"))).to.equal(true);
      expect(mod.isRecoverableSigstoreTufError(new Error("certificate identity error"))).to.equal(false);
    });
  });

  describe("verifySha256()", () => {
    let tmpDir: string;

    beforeEach(() => {
      tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "sqry-test-"));
    });

    afterEach(() => {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    });

    it("passes with matching hash", async () => {
      const mod = loadModule();
      const content = "hello world";
      const filePath = path.join(tmpDir, "test.bin");
      fs.writeFileSync(filePath, content);

      const expectedHash = crypto.createHash("sha256").update(content).digest("hex");
      await mod.verifySha256(filePath, expectedHash);
      // Reaching here without throwing proves the hash matched
      expect(true).to.equal(true);
    });

    it("hard-fails with mismatching hash", async () => {
      const mod = loadModule();
      const filePath = path.join(tmpDir, "test.bin");
      fs.writeFileSync(filePath, "hello world");

      const wrongHash = "0000000000000000000000000000000000000000000000000000000000000000";
      try {
        await mod.verifySha256(filePath, wrongHash);
        expect.fail("Expected rejection");
      } catch (error) {
        expect((error as Error).message).to.contain("SHA256 checksum mismatch");
      }
    });

    it("handles case-insensitive hash comparison", async () => {
      const mod = loadModule();
      const content = "test data";
      const filePath = path.join(tmpDir, "test.bin");
      fs.writeFileSync(filePath, content);

      const expectedHash = crypto.createHash("sha256").update(content).digest("hex").toUpperCase();
      await mod.verifySha256(filePath, expectedHash);
      // Reaching here without throwing proves case-insensitive comparison works
      expect(true).to.equal(true);
    });
  });

  describe("isAllowedHost()", () => {
    it("accepts github.com", () => {
      const mod = loadModule();
      expect(mod.isAllowedHost("github.com")).to.be.true;
    });

    it("accepts objects.githubusercontent.com", () => {
      const mod = loadModule();
      expect(mod.isAllowedHost("objects.githubusercontent.com")).to.be.true;
    });

    it("accepts github-releases.githubusercontent.com", () => {
      const mod = loadModule();
      expect(mod.isAllowedHost("github-releases.githubusercontent.com")).to.be.true;
    });

    it("rejects evil.com", () => {
      const mod = loadModule();
      expect(mod.isAllowedHost("evil.com")).to.be.false;
    });

    it("rejects github.com.evil.com", () => {
      const mod = loadModule();
      expect(mod.isAllowedHost("github.com.evil.com")).to.be.false;
    });

    it("rejects bare githubusercontent.com (no subdomain)", () => {
      const mod = loadModule();
      expect(mod.isAllowedHost("githubusercontent.com")).to.be.false;
    });

    it("rejects deeply nested subdomains", () => {
      const mod = loadModule();
      // sub.sub.githubusercontent.com should fail — only single-label prefix allowed
      expect(mod.isAllowedHost("a.b.githubusercontent.com")).to.be.false;
    });
  });

  describe("lockfile management", () => {
    let tmpDir: string;
    let storageUri: { fsPath: string };

    beforeEach(() => {
      tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "sqry-lock-test-"));
      storageUri = { fsPath: tmpDir };
    });

    afterEach(() => {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    });

    it("acquires lock successfully when no lock exists", () => {
      const mod = loadModule();
      expect(mod.acquireLock(storageUri)).to.be.true;
      expect(fs.existsSync(path.join(tmpDir, "download.lock"))).to.be.true;
    });

    it("fails to acquire lock when another window holds it", () => {
      const mod = loadModule();
      expect(mod.acquireLock(storageUri)).to.be.true;
      expect(mod.acquireLock(storageUri)).to.be.false;
    });

    it("acquires lock when stale lock exists (>10 min)", () => {
      const mod = loadModule();
      const lockPath = path.join(tmpDir, "download.lock");
      fs.writeFileSync(lockPath, "stale");
      // Set mtime to 11 minutes ago
      const staleTime = new Date(Date.now() - 11 * 60 * 1000);
      fs.utimesSync(lockPath, staleTime, staleTime);

      expect(mod.acquireLock(storageUri)).to.be.true;
    });

    it("releases lock", () => {
      const mod = loadModule();
      mod.acquireLock(storageUri);
      mod.releaseLock(storageUri);
      expect(fs.existsSync(path.join(tmpDir, "download.lock"))).to.be.false;
    });
  });

  describe("cleanupOldVersions()", () => {
    let tmpDir: string;
    let storageUri: { fsPath: string };

    beforeEach(() => {
      tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "sqry-cleanup-test-"));
      storageUri = { fsPath: tmpDir };
    });

    afterEach(() => {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    });

    it("retains exactly 2 versions (current + N-1)", () => {
      const mod = loadModule();
      const binDir = path.join(tmpDir, "bin");
      // Create 4 version dirs
      for (const v of ["v4.5.7", "v4.5.8", "v4.5.9", "v4.5.10"]) {
        fs.mkdirSync(path.join(binDir, v), { recursive: true });
        fs.writeFileSync(path.join(binDir, v, "sqry"), "binary");
      }

      mod.cleanupOldVersions(storageUri, "4.5.10");

      const remaining = fs.readdirSync(binDir).sort();
      // Current (v4.5.10) is always kept, plus one more for rollback
      expect(remaining).to.include("v4.5.10");
      expect(remaining.length).to.equal(2);
    });

    it("keeps current version even if it is the only one", () => {
      const mod = loadModule();
      const binDir = path.join(tmpDir, "bin");
      fs.mkdirSync(path.join(binDir, "v4.5.10"), { recursive: true });

      mod.cleanupOldVersions(storageUri, "4.5.10");

      const remaining = fs.readdirSync(binDir);
      expect(remaining).to.deep.equal(["v4.5.10"]);
    });

    it("handles missing bin directory gracefully", () => {
      const mod = loadModule();
      // Should not throw when bin directory doesn't exist
      mod.cleanupOldVersions(storageUri, "4.5.10");
      expect(fs.existsSync(path.join(storageUri.fsPath, "bin"))).to.equal(false);
    });
  });

  describe("getBinaryVersion()", () => {
    it("reads binaryVersion from package.json", () => {
      const mod = loadModule();
      // This reads from the actual package.json since __dirname in the module
      // points to src/ (or the compiled output). We test the real value.
      const version = mod.getBinaryVersion();
      expect(version).to.be.a("string");
      expect(version).to.match(/^\d+\.\d+\.\d+/);
    });
  });

  describe("verifyCosignBundle()", () => {
    it("fails closed when the attestation bundle file is missing", async () => {
      const mod = loadModule();
      const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "sqry-bundle-missing-"));
      const binaryPath = path.join(tmpDir, "sqry");
      fs.writeFileSync(binaryPath, "binary");
      const missingBundlePath = path.join(tmpDir, "release-artifacts.attestation.json");

      try {
        await mod.verifyCosignBundle(binaryPath, missingBundlePath, "8.0.2", {
          appendLine: () => {},
        }, { fsPath: tmpDir });
        expect.fail("Expected missing bundle failure");
      } catch (error) {
        expect((error as Error).message).to.equal(
          "Cosign signature bundle file is missing. Binary provenance cannot be verified.",
        );
      } finally {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      }
    });
  });

  describe("applyBinaryPermissions()", () => {
    it("applies restrictive permissions on unix platforms", () => {
      let chmodArgs: [string, number] | null = null;
      const mod = loadModule({
        "node:fs": {
          ...fs,
          chmodSync: (targetPath: string, mode: number) => {
            chmodArgs = [targetPath, mode];
          },
        },
      });

      mod.applyBinaryPermissions("/tmp/sqry", "linux");

      expect(chmodArgs).to.deep.equal(["/tmp/sqry", 0o700]);
    });

    it("skips chmod on windows", () => {
      let chmodCalled = false;
      const mod = loadModule({
        "node:fs": {
          ...fs,
          chmodSync: () => {
            chmodCalled = true;
          },
        },
      });

      mod.applyBinaryPermissions("C:\\sqry.exe", "win32");

      expect(chmodCalled).to.equal(false);
    });
  });

  describe("describePreflightError()", () => {
    it("returns the message for Error instances", () => {
      const mod = loadModule();
      expect(mod.describePreflightError(new Error("preflight failed"))).to.equal("preflight failed");
    });

    it("stringifies non-Error values", () => {
      const mod = loadModule();
      expect(mod.describePreflightError({ code: 1 })).to.equal("[object Object]");
    });
  });
});
