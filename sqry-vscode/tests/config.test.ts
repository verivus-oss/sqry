import { expect } from "chai";
// eslint-disable-next-line @typescript-eslint/no-var-requires
const proxyquire = require("proxyquire").noCallThru();

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

describe("config", () => {
  beforeEach(() => {
    process.env.HOME = HOME;
    process.env.USERPROFILE = HOME;
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

    const path = await module.resolveBinary("sqry");
    expect(path).to.equal("/mock/path/sqry");
  });

  it("throws when binary missing", async () => {
    const module = loadConfig({
      which: async () => {
        throw new Error("not found");
      },
    });

    try {
      await module.resolveBinary("missing");
      expect.fail("Expected rejection");
    } catch (error) {
      expect((error as Error).message).to.contain(
        "Unable to locate sqry binary",
      );
    }
  });
});
