import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { expect } from "chai";
import proxyquireModule from "proxyquire";

const proxyquire = proxyquireModule.noCallThru();

// Mutable per-test workspace state the stub reads.
let workspaceFolders: Array<{ uri: { scheme: string; fsPath: string } }> = [];
const warnings: string[] = [];

class FakePosition {
  constructor(public readonly line: number, public readonly character: number) {}
}
class FakeRange {
  constructor(public readonly start: unknown, public readonly end?: unknown) {}
}
class FakeSelection {
  constructor(public readonly anchor: unknown, public readonly active: unknown) {}
}

const vscodeStub = {
  __esModule: true,
  ViewColumn: { One: 1 },
  Uri: {
    file: (p: string) => ({ fsPath: p, scheme: "file", toString: () => `file://${p}` }),
  },
  Position: FakePosition,
  Range: FakeRange,
  Selection: FakeSelection,
  workspace: {
    get workspaceFolders() {
      return workspaceFolders.length > 0 ? workspaceFolders : undefined;
    },
    openTextDocument: async (uri: unknown) => ({ uri }),
  },
  window: {
    showWarningMessage: (message: string) => {
      warnings.push(message);
      return Promise.resolve(undefined);
    },
    showTextDocument: async () => ({
      selection: undefined as unknown,
      revealRange: () => {},
    }),
  },
};

function loadModule() {
  return proxyquire("../src/workspaceGuard", { vscode: vscodeStub });
}

function setWorkspace(...roots: string[]): void {
  workspaceFolders = roots.map((root) => ({ uri: { scheme: "file", fsPath: root } }));
}

describe("workspaceGuard", () => {
  let root: string;

  beforeEach(() => {
    warnings.length = 0;
    root = fs.mkdtempSync(path.join(os.tmpdir(), "sqry-ws-guard-"));
    setWorkspace(root);
  });

  afterEach(() => {
    fs.rmSync(root, { recursive: true, force: true });
    workspaceFolders = [];
  });

  describe("resolveWithinWorkspace", () => {
    it("accepts a file inside a workspace folder", () => {
      const mod = loadModule();
      const inside = path.join(root, "src", "lib.ts");
      fs.mkdirSync(path.dirname(inside), { recursive: true });
      fs.writeFileSync(inside, "export {};");
      const uri = mod.resolveWithinWorkspace(inside);
      expect(uri, "in-workspace file should resolve").to.not.equal(undefined);
      expect(fs.realpathSync.native(uri.fsPath)).to.equal(fs.realpathSync.native(inside));
    });

    it("accepts the workspace root itself", () => {
      const mod = loadModule();
      expect(mod.resolveWithinWorkspace(root)).to.not.equal(undefined);
    });

    it("rejects an absolute path outside every workspace folder", () => {
      const mod = loadModule();
      const outside = fs.mkdtempSync(path.join(os.tmpdir(), "sqry-outside-"));
      try {
        expect(mod.resolveWithinWorkspace(path.join(outside, "secret.txt"))).to.equal(undefined);
      } finally {
        fs.rmSync(outside, { recursive: true, force: true });
      }
    });

    it("rejects a `..` traversal that climbs out of the workspace", () => {
      const mod = loadModule();
      // A path that lexically escapes the root, e.g. /ws/../../etc/passwd.
      const escaping = path.join(root, "..", "..", "etc", "passwd");
      expect(mod.resolveWithinWorkspace(escaping)).to.equal(undefined);
    });

    it("rejects a symlink that points outside the workspace", () => {
      const mod = loadModule();
      const outside = fs.mkdtempSync(path.join(os.tmpdir(), "sqry-symlink-target-"));
      const secret = path.join(outside, "secret.txt");
      fs.writeFileSync(secret, "top secret");
      const link = path.join(root, "link.txt");
      try {
        fs.symlinkSync(secret, link);
      } catch {
        // Some CI filesystems disallow symlinks; skip in that case.
        fs.rmSync(outside, { recursive: true, force: true });
        return;
      }
      try {
        // The link lives inside the workspace, but realpath resolves outside it.
        expect(mod.resolveWithinWorkspace(link)).to.equal(undefined);
      } finally {
        fs.rmSync(outside, { recursive: true, force: true });
      }
    });

    it("rejects everything when no workspace folder is open", () => {
      const mod = loadModule();
      workspaceFolders = [];
      const inside = path.join(root, "src.ts");
      fs.writeFileSync(inside, "export {};");
      expect(mod.resolveWithinWorkspace(inside)).to.equal(undefined);
    });

    it("rejects an empty path", () => {
      const mod = loadModule();
      expect(mod.resolveWithinWorkspace("")).to.equal(undefined);
    });
  });

  describe("openFileWithinWorkspace", () => {
    it("warns and does not open a file outside the workspace", async () => {
      const mod = loadModule();
      const outside = fs.mkdtempSync(path.join(os.tmpdir(), "sqry-open-outside-"));
      try {
        const editor = await mod.openFileWithinWorkspace(path.join(outside, "x.ts"));
        expect(editor).to.equal(undefined);
        expect(warnings, "a warning should be shown").to.have.length(1);
        expect(warnings[0]).to.contain("outside the current workspace");
      } finally {
        fs.rmSync(outside, { recursive: true, force: true });
      }
    });

    it("opens a file inside the workspace", async () => {
      const mod = loadModule();
      const inside = path.join(root, "a.ts");
      fs.writeFileSync(inside, "export {};");
      const editor = await mod.openFileWithinWorkspace(inside, {
        selection: { startLine: 3 },
      });
      expect(editor, "in-workspace file should open").to.not.equal(undefined);
      expect(warnings).to.have.length(0);
    });
  });

  describe("resolveWithinWorkspace - additional containment cases", () => {
    it("does not treat a sibling folder with a shared prefix as inside", () => {
      const mod = loadModule();
      // `/tmp/root` must NOT contain `/tmp/root-evil` (path.relative, not prefix).
      const evil = `${root}-evil`;
      fs.mkdirSync(evil, { recursive: true });
      try {
        const target = path.join(evil, "secret.ts");
        fs.writeFileSync(target, "export {};");
        expect(mod.resolveWithinWorkspace(target)).to.equal(undefined);
      } finally {
        fs.rmSync(evil, { recursive: true, force: true });
      }
    });

    it("accepts a file inside any of several workspace roots", () => {
      const mod = loadModule();
      const second = fs.mkdtempSync(path.join(os.tmpdir(), "sqry-ws-second-"));
      setWorkspace(root, second);
      try {
        const inSecond = path.join(second, "b.ts");
        fs.writeFileSync(inSecond, "export {};");
        expect(mod.resolveWithinWorkspace(inSecond)).to.not.equal(undefined);
      } finally {
        fs.rmSync(second, { recursive: true, force: true });
      }
    });

    it("ignores non-file workspace folders", () => {
      const mod = loadModule();
      workspaceFolders = [{ uri: { scheme: "vscode-remote", fsPath: root } }];
      const inside = path.join(root, "a.ts");
      fs.writeFileSync(inside, "export {};");
      expect(mod.resolveWithinWorkspace(inside)).to.equal(undefined);
    });

    it("rejects a symlinked parent even when the final component does not exist", () => {
      const mod = loadModule();
      const outside = fs.mkdtempSync(path.join(os.tmpdir(), "sqry-symlink-parent-"));
      const linkDir = path.join(root, "escape");
      try {
        fs.symlinkSync(outside, linkDir, "dir");
      } catch {
        fs.rmSync(outside, { recursive: true, force: true });
        return; // filesystem disallows symlinks
      }
      try {
        // `escape` -> /outside; the leaf `missing.ts` does not exist. A lexical
        // resolve would keep it "inside"; the guard must realpath the existing
        // parent and reject.
        const escaping = path.join(linkDir, "missing.ts");
        expect(mod.resolveWithinWorkspace(escaping)).to.equal(undefined);
      } finally {
        fs.rmSync(outside, { recursive: true, force: true });
      }
    });
  });

  describe("resolveUriWithinWorkspace", () => {
    it("rejects a non-file URI scheme outright", () => {
      const mod = loadModule();
      expect(
        mod.resolveUriWithinWorkspace({ scheme: "untitled", fsPath: path.join(root, "a.ts") }),
      ).to.equal(undefined);
    });

    it("accepts an in-workspace file URI", () => {
      const mod = loadModule();
      const inside = path.join(root, "a.ts");
      fs.writeFileSync(inside, "export {};");
      expect(
        mod.resolveUriWithinWorkspace({ scheme: "file", fsPath: inside }),
      ).to.not.equal(undefined);
    });

    it("accepts an in-workspace file URI whose scheme is uppercase", () => {
      // RFC 3986 makes the scheme case-insensitive, and `URI.parse` keeps
      // whatever case it was given, so `FILE:///...` reaches the guard with an
      // uppercase scheme. Comparing case-sensitively would drop a legitimate
      // in-workspace location.
      const mod = loadModule();
      const inside = path.join(root, "upper.ts");
      fs.writeFileSync(inside, "export {};");
      for (const scheme of ["FILE", "File"]) {
        expect(
          mod.resolveUriWithinWorkspace({ scheme, fsPath: inside }),
          `scheme ${scheme} should be accepted`,
        ).to.not.equal(undefined);
      }
    });

    it("still rejects a non-file scheme regardless of case", () => {
      const mod = loadModule();
      for (const scheme of ["UNTITLED", "Untitled", "VSCODE-REMOTE"]) {
        expect(
          mod.resolveUriWithinWorkspace({ scheme, fsPath: path.join(root, "a.ts") }),
          `scheme ${scheme} should be rejected`,
        ).to.equal(undefined);
      }
    });
  });
});
