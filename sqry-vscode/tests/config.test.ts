import { expect } from "chai";
import * as path from "node:path";
import proxyquireModule from "proxyquire";

const proxyquire = proxyquireModule.noCallThru();

const HOME = "/home/tester";

function loadConfig(overrides: Record<string, unknown>) {
  const vscodeStub = {
    __esModule: true,
    workspace: {
      getConfiguration: () => ({
        get: (_key: string, fallback: unknown) => fallback,
      }),
    },
  };

  return proxyquire("../src/config", {
    vscode: vscodeStub,
    ...overrides,
  });
}

/** Helper to build a loadConfig with controlled fs.stat + fs.access behavior. */
function loadConfigWithProbes(
  whichFn: (binary: string) => Promise<string>,
  existingFiles: Set<string>,
) {
  return loadConfig({
    which: whichFn,
    "node:fs": {
      promises: {
        stat: async (filePath: string) => {
          if (existingFiles.has(filePath)) {
            return { isFile: () => true };
          }
          throw new Error(`ENOENT: ${filePath}`);
        },
        access: async (filePath: string) => {
          if (existingFiles.has(filePath)) {
            return;
          }
          throw new Error(`EACCES: ${filePath}`);
        },
      },
      constants: { X_OK: 1 },
    },
  });
}

const whichNotFound = async () => { throw new Error("not found"); };

describe("config", () => {
  beforeEach(() => {
    process.env.HOME = HOME;
    process.env.USERPROFILE = HOME;
    delete process.env.CARGO_HOME;
  });

  it("expands tilde paths", () => {
    const module = loadConfig({ which: async (binary: string) => binary });
    expect(module.expandTilde("~/bin/sqry")).to.equal(`${HOME}/bin/sqry`);
    expect(module.expandTilde("/usr/bin/sqry")).to.equal("/usr/bin/sqry");
  });

  it("resolves binaries via which", async () => {
    const module = loadConfig({
      which: async (binary: string) => `/mock/path/${binary}`,
    });

    const resolved = await module.resolveBinary("sqry");
    expect(resolved).to.equal("/mock/path/sqry");
  });

  it("which() success skips probes", async () => {
    const module = loadConfigWithProbes(
      async (binary: string) => `/usr/bin/${binary}`,
      new Set([`${HOME}/.cargo/bin/sqry`]),
    );

    // Should use which() result, not the probe path
    const resolved = await module.resolveBinary("sqry");
    expect(resolved).to.equal("/usr/bin/sqry");
  });

  it("bare-command config probes common locations on POSIX", async () => {
    const cargoPath = path.join(HOME, ".cargo", "bin", "sqry");
    const module = loadConfigWithProbes(
      whichNotFound,
      new Set([cargoPath]),
    );

    const resolved = await module.resolveBinary("sqry");
    expect(resolved).to.equal(cargoPath);
  });

  it("$CARGO_HOME overrides default cargo path", async () => {
    process.env.CARGO_HOME = "/custom/cargo";
    const customCargoPath = "/custom/cargo/bin/sqry";
    const module = loadConfigWithProbes(
      whichNotFound,
      new Set([customCargoPath]),
    );

    const resolved = await module.resolveBinary("sqry");
    expect(resolved).to.equal(customCargoPath);
  });

  it("explicit configured path does not trigger probes", async () => {
    const cargoPath = path.join(HOME, ".cargo", "bin", "sqry");
    const module = loadConfigWithProbes(
      whichNotFound,
      new Set([cargoPath]),
    );

    // User explicitly set sqry.path to a non-default value
    try {
      await module.resolveBinary("/opt/custom/sqry");
      expect.fail("Expected rejection");
    } catch (error) {
      expect((error as Error).message).to.contain("Unable to locate sqry binary");
    }
  });

  it("probe order is deterministic", () => {
    const module = loadConfig({ which: async (binary: string) => binary });
    const paths = module.getCommonBinaryPaths();
    // First entry should be cargo bin
    expect(paths[0]).to.contain(".cargo");
    // Second should be .local/bin
    expect(paths[1]).to.contain(".local");
    // Third should be /usr/local/bin
    expect(paths[2]).to.equal("/usr/local/bin/sqry");
  });

  it("fallback binary is used when probes fail", async () => {
    const fallbackPath = "/tmp/downloaded/sqry";
    const module = loadConfigWithProbes(
      whichNotFound,
      new Set([fallbackPath]),  // only the fallback exists
    );

    const resolved = await module.resolveBinary("sqry", fallbackPath);
    expect(resolved).to.equal(fallbackPath);
  });

  it("throws when binary missing and no fallback", async () => {
    const module = loadConfigWithProbes(whichNotFound, new Set());

    try {
      await module.resolveBinary("missing");
      expect.fail("Expected rejection");
    } catch (error) {
      expect((error as Error).message).to.contain(
        "Unable to locate sqry binary",
      );
    }
  });

  it("probes ~/.local/bin when cargo path missing", async () => {
    const localPath = path.join(HOME, ".local", "bin", "sqry");
    const module = loadConfigWithProbes(
      whichNotFound,
      new Set([localPath]),
    );

    const resolved = await module.resolveBinary("sqry");
    expect(resolved).to.equal(localPath);
  });

  it("expandTilde only expands ~/... not ~otheruser/...", () => {
    const module = loadConfig({ which: async (binary: string) => binary });
    // ~/bin should expand
    expect(module.expandTilde("~/bin/sqry")).to.equal(`${HOME}/bin/sqry`);
    // bare ~ should expand
    expect(module.expandTilde("~")).to.equal(HOME);
    // ~otheruser should NOT expand
    expect(module.expandTilde("~otheruser/bin/sqry")).to.equal("~otheruser/bin/sqry");
    // absolute path untouched
    expect(module.expandTilde("/usr/bin/sqry")).to.equal("/usr/bin/sqry");
  });

  describe("Windows paths", () => {
    it("getCommonBinaryPaths uses .exe on Windows", () => {
      const originalPlatform = Object.getOwnPropertyDescriptor(process, "platform");
      Object.defineProperty(process, "platform", { value: "win32" });
      try {
        const module = loadConfig({ which: async (binary: string) => binary });
        const paths = module.getCommonBinaryPaths();
        for (const p of paths) {
          expect(p).to.match(/\.exe$/, `Expected .exe suffix: ${p}`);
        }
        expect(paths.length).to.be.greaterThan(0);
      } finally {
        if (originalPlatform) {
          Object.defineProperty(process, "platform", originalPlatform);
        }
      }
    });

    it("getCommonBinaryPaths respects CARGO_HOME on Windows", () => {
      const originalPlatform = Object.getOwnPropertyDescriptor(process, "platform");
      Object.defineProperty(process, "platform", { value: "win32" });
      process.env.CARGO_HOME = "D:\\custom\\cargo";
      try {
        const module = loadConfig({ which: async (binary: string) => binary });
        const paths = module.getCommonBinaryPaths();
        expect(paths[0]).to.equal(path.join("D:\\custom\\cargo", "bin", "sqry.exe"));
      } finally {
        delete process.env.CARGO_HOME;
        if (originalPlatform) {
          Object.defineProperty(process, "platform", originalPlatform);
        }
      }
    });

    it("getCommonBinaryPaths includes LOCALAPPDATA on Windows", () => {
      const originalPlatform = Object.getOwnPropertyDescriptor(process, "platform");
      Object.defineProperty(process, "platform", { value: "win32" });
      process.env.LOCALAPPDATA = "C:\\Users\\test\\AppData\\Local";
      try {
        const module = loadConfig({ which: async (binary: string) => binary });
        const paths = module.getCommonBinaryPaths();
        const localAppDataPath = paths.find((p: string) => p.includes("AppData"));
        expect(localAppDataPath).to.equal(
          path.join("C:\\Users\\test\\AppData\\Local", "sqry", "sqry.exe"),
        );
      } finally {
        delete process.env.LOCALAPPDATA;
        if (originalPlatform) {
          Object.defineProperty(process, "platform", originalPlatform);
        }
      }
    });

    it("isExecutableFile accepts .exe on Windows probe", async () => {
      const originalPlatform = Object.getOwnPropertyDescriptor(process, "platform");
      Object.defineProperty(process, "platform", { value: "win32" });
      // Set CARGO_HOME so getCommonBinaryPaths() generates a known path
      process.env.CARGO_HOME = "C:\\Users\\test\\.cargo";
      try {
        const exePath = path.join("C:\\Users\\test\\.cargo", "bin", "sqry.exe");
        const module = loadConfigWithProbes(
          whichNotFound,
          new Set([exePath]),
        );

        const resolved = await module.resolveBinary("sqry");
        expect(resolved).to.equal(exePath);
      } finally {
        delete process.env.CARGO_HOME;
        if (originalPlatform) {
          Object.defineProperty(process, "platform", originalPlatform);
        }
      }
    });
  });
});
