import { expect } from "chai";
import { promisify } from "util";
import { execFile } from "child_process";
import * as path from "path";
import { parseQueryOutput, parseSearchOutput } from "../src/parsers";

const execFileAsync = promisify(execFile);
const stubPath = path.join(__dirname, "stubs", "sqryStub.js");

describe("parsers", () => {
  it("parses query JSON from stub", async () => {
    const { stdout } = await execFileAsync(process.execPath, [
      stubPath,
      "query",
      "--json",
      "callers:helper",
    ]);

    const result = parseQueryOutput(stdout.trim());
    expect(result.symbols).to.have.length(1);
    expect(result.symbols[0].qualifiedName).to.equal("Service.process");
  });

  it("parses search events from stub", async () => {
    const { stdout } = await execFileAsync(process.execPath, [
      stubPath,
      "search",
      "--json",
      "helper",
    ]);

    const result = parseSearchOutput(stdout);
    expect(result.textMatches).to.have.length(2);
    expect(result.textMatches[0].path).to.contain("sample.cpp");
  });
});
