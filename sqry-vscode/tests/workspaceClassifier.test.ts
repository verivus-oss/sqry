import { expect } from "chai";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import {
  buildClassificationScaffold,
  buildWorkspaceInitializationPayload,
  DEFAULT_CLASSIFICATION_SCAFFOLD,
  isFolderExcluded,
  parseWorkspaceFile,
  readWorkspaceFile,
  resolveWorkspaceFilePath,
  stripJsonComments,
} from "../src/workspaceClassifier";

describe("workspaceClassifier — resolveWorkspaceFilePath", () => {
  it("returns null when no workspace file is open", () => {
    expect(resolveWorkspaceFilePath(undefined)).to.equal(null);
  });

  it("returns null for untitled: scheme URIs", () => {
    expect(resolveWorkspaceFilePath("untitled:Workspace-1")).to.equal(null);
  });

  it("returns the absolute path otherwise", () => {
    const result = resolveWorkspaceFilePath("/tmp/example.code-workspace");
    expect(result).to.equal("/tmp/example.code-workspace");
  });
});

describe("workspaceClassifier — stripJsonComments", () => {
  it("strips line comments", () => {
    const stripped = stripJsonComments(`{ "k": 1 // trailing\n}`);
    expect(stripped).to.equal(`{ "k": 1 \n}`);
  });

  it("strips block comments", () => {
    const stripped = stripJsonComments(`{ /* block */ "k": 1 }`);
    expect(stripped).to.equal(`{  "k": 1 }`);
  });

  it("preserves comments inside strings", () => {
    const stripped = stripJsonComments(`{ "k": "// not a comment" }`);
    expect(stripped).to.equal(`{ "k": "// not a comment" }`);
  });

  it("handles escaped quotes inside strings", () => {
    const stripped = stripJsonComments(`{ "k": "with \\"quote\\" // x" }`);
    expect(stripped).to.equal(`{ "k": "with \\"quote\\" // x" }`);
  });
});

describe("workspaceClassifier — parseWorkspaceFile", () => {
  it("parses folders array", () => {
    const parsed = parseWorkspaceFile(`{ "folders": [{ "path": "frontend", "name": "FE" }] }`);
    expect(parsed.folders).to.have.length(1);
    expect(parsed.folders[0].path).to.equal("frontend");
    expect(parsed.folders[0].name).to.equal("FE");
    expect(parsed.classification).to.equal(null);
  });

  it("parses sqry.workspace block", () => {
    const parsed = parseWorkspaceFile(`{
      "folders": [],
      "sqry.workspace": {
        "sourceRoots": ["/a", "/b"],
        "exclusions": ["docs/**"],
        "memberFolders": ["/c"],
        "projectRootMode": "explicit"
      }
    }`);
    expect(parsed.classification?.sourceRoots).to.deep.equal(["/a", "/b"]);
    expect(parsed.classification?.exclusions).to.deep.equal(["docs/**"]);
    expect(parsed.classification?.memberFolders).to.deep.equal(["/c"]);
    expect(parsed.classification?.projectRootMode).to.equal("explicit");
  });

  it("ignores non-string entries inside the arrays", () => {
    const parsed = parseWorkspaceFile(`{
      "folders": [{ "path": "x" }, { "no": "path" }],
      "sqry.workspace": { "sourceRoots": ["a", 42, "b"] }
    }`);
    expect(parsed.folders.map((f) => f.path)).to.deep.equal(["x"]);
    expect(parsed.classification?.sourceRoots).to.deep.equal(["a", "b"]);
  });

  it("falls back to defaults for malformed projectRootMode", () => {
    const parsed = parseWorkspaceFile(`{ "sqry.workspace": { "projectRootMode": "bogus" } }`);
    expect(parsed.classification?.projectRootMode).to.equal(undefined);
  });

  it("tolerates JSON with comments", () => {
    const parsed = parseWorkspaceFile(`{
      // top-level comment
      "folders": [{ "path": "f" }] /* trailing */
    }`);
    expect(parsed.folders[0].path).to.equal("f");
  });
});

describe("workspaceClassifier — readWorkspaceFile", () => {
  it("returns null on ENOENT", () => {
    expect(readWorkspaceFile("/no-such-path/missing.code-workspace")).to.equal(null);
  });

  it("reads and parses an on-disk file", () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "sqry-classifier-"));
    const file = path.join(tmp, "test.code-workspace");
    fs.writeFileSync(file, `{ "folders": [{ "path": "x" }] }`);
    try {
      const parsed = readWorkspaceFile(file);
      expect(parsed?.folders[0].path).to.equal("x");
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });
});

describe("workspaceClassifier — buildClassificationScaffold", () => {
  it("seeds the scaffold for an empty input", () => {
    const { content, alreadyHadBlock } = buildClassificationScaffold(null);
    expect(alreadyHadBlock).to.equal(false);
    const parsed = JSON.parse(content);
    expect(parsed["sqry.workspace"]).to.deep.equal({
      sourceRoots: [],
      exclusions: [],
      memberFolders: [],
      projectRootMode: DEFAULT_CLASSIFICATION_SCAFFOLD.projectRootMode,
    });
    expect(parsed.folders).to.deep.equal([]);
  });

  it("adds the scaffold without clobbering existing top-level keys", () => {
    const input = JSON.stringify({
      folders: [{ path: "a" }],
      settings: { "editor.tabSize": 2 },
    });
    const { content, alreadyHadBlock } = buildClassificationScaffold(input);
    expect(alreadyHadBlock).to.equal(false);
    const parsed = JSON.parse(content);
    expect(parsed.folders).to.deep.equal([{ path: "a" }]);
    expect(parsed.settings).to.deep.equal({ "editor.tabSize": 2 });
    expect(parsed["sqry.workspace"]).to.deep.equal({
      sourceRoots: [],
      exclusions: [],
      memberFolders: [],
      projectRootMode: "gitRoot",
    });
  });

  it("does not re-scaffold when the block is already present", () => {
    const input = JSON.stringify({
      folders: [],
      "sqry.workspace": { sourceRoots: ["/already/set"] },
    });
    const { content, alreadyHadBlock } = buildClassificationScaffold(input);
    expect(alreadyHadBlock).to.equal(true);
    const parsed = JSON.parse(content);
    expect(parsed["sqry.workspace"].sourceRoots).to.deep.equal(["/already/set"]);
  });

  it("refuses to overwrite malformed JSON", () => {
    const input = "{ this is not json";
    const { content, alreadyHadBlock } = buildClassificationScaffold(input);
    expect(alreadyHadBlock).to.equal(true);
    expect(content).to.equal(input);
  });
});

describe("workspaceClassifier — isFolderExcluded", () => {
  it("returns false for an empty exclusion list", () => {
    expect(isFolderExcluded("/repo/src", [])).to.equal(false);
  });

  it("matches exact basenames", () => {
    expect(isFolderExcluded("/repo/docs", ["docs"])).to.equal(true);
    expect(isFolderExcluded("/repo/src", ["docs"])).to.equal(false);
  });

  it("supports `*` (single segment) and `**` (any segments) globs", () => {
    expect(isFolderExcluded("/repo/docs/api", ["docs/**"])).to.equal(true);
    expect(isFolderExcluded("/repo/docs", ["docs/*"])).to.equal(false);
    expect(isFolderExcluded("/repo/api/docs", ["**/docs"])).to.equal(true);
  });

  it("treats backslashes the same as forward slashes (Windows paths)", () => {
    expect(isFolderExcluded("C:\\repo\\docs", ["docs"])).to.equal(true);
  });
});

// ---------------------------------------------------------------------------
// STEP_5 codex iter1 MAJOR 2 — `extension.ts` activation must parse the
// `.code-workspace` and forward the parsed/classified OBJECT (NOT the
// path string) under `initializationOptions.sqry.workspace`.
//
// `extension.ts` calls `buildWorkspaceInitializationPayload` exactly
// once at activation; testing the helper guarantees the activation
// path obeys the contract. The matching round-trip on `SqryClient`
// (the receiver) is covered by `multi_root.test.ts`.
// ---------------------------------------------------------------------------

describe("workspaceClassifier — buildWorkspaceInitializationPayload (STEP_5 codex iter1 MAJOR 2)", () => {
  let scratchDir: string;

  beforeEach(() => {
    scratchDir = fs.mkdtempSync(path.join(os.tmpdir(), "sqry-init-payload-"));
  });

  afterEach(() => {
    fs.rmSync(scratchDir, { recursive: true, force: true });
  });

  it("returns an object (NOT the path string) when the .code-workspace exists", () => {
    const wsPath = path.join(scratchDir, "proj.code-workspace");
    fs.writeFileSync(
      wsPath,
      JSON.stringify({
        folders: [{ path: "./repo-a", name: "Repo A" }, { path: "./repo-b" }],
        "sqry.workspace": {
          sourceRoots: ["./repo-a"],
          memberFolders: ["./repo-b"],
          projectRootMode: "gitRoot",
        },
      }),
    );
    const payload = buildWorkspaceInitializationPayload(wsPath);
    expect(payload).to.not.equal(null);
    // The payload MUST be an object, not the file path string.
    expect(typeof payload).to.equal("object");
    expect(payload).to.not.equal(wsPath);
    expect(payload!.folders).to.deep.equal([
      { path: "./repo-a", name: "Repo A" },
      { path: "./repo-b", name: undefined },
    ]);
    expect(payload!.classification).to.deep.equal({
      sourceRoots: ["./repo-a"],
      memberFolders: ["./repo-b"],
      projectRootMode: "gitRoot",
    });
  });

  it("returns the parsed shape with classification=null when the block is absent", () => {
    const wsPath = path.join(scratchDir, "no-block.code-workspace");
    fs.writeFileSync(wsPath, JSON.stringify({ folders: [{ path: "./only-folder" }] }));
    const payload = buildWorkspaceInitializationPayload(wsPath);
    expect(payload).to.not.equal(null);
    expect(payload!.folders).to.deep.equal([{ path: "./only-folder", name: undefined }]);
    expect(payload!.classification).to.equal(null);
  });

  it("returns null when the file does not exist (caller substitutes a default empty payload)", () => {
    const missing = path.join(scratchDir, "does-not-exist.code-workspace");
    expect(buildWorkspaceInitializationPayload(missing)).to.equal(null);
  });

  it("propagates JSON parse errors so the activation path can log them", () => {
    const wsPath = path.join(scratchDir, "broken.code-workspace");
    fs.writeFileSync(wsPath, "{ this is not json");
    expect(() => buildWorkspaceInitializationPayload(wsPath)).to.throw();
  });

  it("payload is structurally distinct from the path string (negative-case for the iter1 bug)", () => {
    // Defence-in-depth: this regression-style test makes the contract
    // explicit. The iter1 bug shipped `workspace: <path>`; if a future
    // refactor reintroduces the path-as-workspace shape this test will
    // fail.
    const wsPath = path.join(scratchDir, "foo.code-workspace");
    fs.writeFileSync(wsPath, JSON.stringify({ folders: [] }));
    const payload = buildWorkspaceInitializationPayload(wsPath);
    expect(typeof payload).to.equal("object");
    expect(payload).to.have.property("folders");
    expect(payload).to.have.property("classification");
    // The payload is never equal to the path string under any
    // serialization comparison.
    expect(JSON.stringify(payload)).to.not.equal(JSON.stringify(wsPath));
  });
});

// Reuse imports above (DEFAULT_CLASSIFICATION_SCAFFOLD is exercised in
// the buildClassificationScaffold suite).
void DEFAULT_CLASSIFICATION_SCAFFOLD;
