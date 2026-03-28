import { expect } from "chai";
import { exportAsJson, exportAsMarkdown, exportAsCsv } from "../src/exportResults";
import { SqrySymbolResult } from "../src/types";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const SYMBOL_A: SqrySymbolResult = {
  name: "parseInput",
  kind: "function",
  filePath: "/project/src/parser.rs",
  startLine: 10,
  language: "rust",
};

const SYMBOL_B: SqrySymbolResult = {
  name: "GraphBuilder",
  kind: "class",
  filePath: "/project/src/graph.rs",
  startLine: 1,
  language: "rust",
};

const SYMBOL_SPARSE: SqrySymbolResult = {
  name: "anonymousFn",
  filePath: "/project/src/lib.rs",
  startLine: 42,
  // kind and language are intentionally omitted
};

const TWO_SYMBOLS = [SYMBOL_A, SYMBOL_B];

// ---------------------------------------------------------------------------
// exportAsJson
// ---------------------------------------------------------------------------

describe("exportResults — exportAsJson", () => {
  it("produces valid JSON", () => {
    const output = exportAsJson(TWO_SYMBOLS);
    expect(() => JSON.parse(output)).not.to.throw();
  });

  it("output array length matches input symbol count", () => {
    const output = exportAsJson(TWO_SYMBOLS);
    const parsed: unknown[] = JSON.parse(output) as unknown[];
    expect(parsed).to.have.length(2);
  });

  it("each entry contains the expected fields", () => {
    const output = exportAsJson([SYMBOL_A]);
    const parsed = JSON.parse(output) as Array<Record<string, unknown>>;
    const entry = parsed[0];
    expect(entry).to.have.property("name", "parseInput");
    expect(entry).to.have.property("kind", "function");
    expect(entry).to.have.property("file", "/project/src/parser.rs");
    expect(entry).to.have.property("line", 10);
    expect(entry).to.have.property("language", "rust");
  });

  it("uses empty string for missing kind and language", () => {
    const output = exportAsJson([SYMBOL_SPARSE]);
    const parsed = JSON.parse(output) as Array<Record<string, unknown>>;
    const entry = parsed[0];
    expect(entry).to.have.property("kind", "");
    expect(entry).to.have.property("language", "");
  });

  it("returns an empty JSON array for empty input", () => {
    const output = exportAsJson([]);
    const parsed: unknown[] = JSON.parse(output) as unknown[];
    expect(parsed).to.deep.equal([]);
  });
});

// ---------------------------------------------------------------------------
// exportAsMarkdown
// ---------------------------------------------------------------------------

describe("exportResults — exportAsMarkdown", () => {
  it("first line is a pipe-delimited header row", () => {
    const lines = exportAsMarkdown(TWO_SYMBOLS).split("\n");
    expect(lines[0]).to.match(/^\|.*Name.*\|.*Kind.*\|.*File.*\|.*Line.*\|.*Language.*\|$/);
  });

  it("second line is a separator row with dashes", () => {
    const lines = exportAsMarkdown(TWO_SYMBOLS).split("\n");
    expect(lines[1]).to.match(/^\|[-| ]+\|$/);
  });

  it("subsequent lines correspond to each symbol", () => {
    const lines = exportAsMarkdown(TWO_SYMBOLS).split("\n");
    // 2 header/separator lines + 2 data lines
    expect(lines).to.have.length(4);
  });

  it("data rows contain symbol name and kind", () => {
    const output = exportAsMarkdown([SYMBOL_A]);
    expect(output).to.include("parseInput");
    expect(output).to.include("function");
  });

  it("returns only header and separator for empty input", () => {
    const lines = exportAsMarkdown([]).split("\n");
    expect(lines).to.have.length(2);
    expect(lines[0]).to.include("Name");
    expect(lines[1]).to.include("---");
  });
});

// ---------------------------------------------------------------------------
// exportAsCsv
// ---------------------------------------------------------------------------

describe("exportResults — exportAsCsv", () => {
  it("first line is the header row", () => {
    const lines = exportAsCsv(TWO_SYMBOLS).split("\n");
    expect(lines[0]).to.equal("Name,Kind,File,Line,Language");
  });

  it("number of data lines matches symbol count", () => {
    const lines = exportAsCsv(TWO_SYMBOLS).split("\n");
    // 1 header + 2 data lines
    expect(lines).to.have.length(3);
  });

  it("data rows contain correct field values", () => {
    const lines = exportAsCsv([SYMBOL_A]).split("\n");
    const dataLine = lines[1];
    expect(dataLine).to.include("parseInput");
    expect(dataLine).to.include("function");
    expect(dataLine).to.include("/project/src/parser.rs");
    expect(dataLine).to.include("10");
    expect(dataLine).to.include("rust");
  });

  it("returns only the header line for empty input", () => {
    const lines = exportAsCsv([]).split("\n");
    expect(lines).to.have.length(1);
    expect(lines[0]).to.equal("Name,Kind,File,Line,Language");
  });

  it("escapes commas in field values with RFC 4180 quoting", () => {
    const symbol: SqrySymbolResult = {
      name: "a,b",
      kind: "function",
      filePath: "/project/src.rs",
      startLine: 1,
      language: "rust",
    };
    const lines = exportAsCsv([symbol]).split("\n");
    expect(lines[1]).to.include('"a,b"');
  });

  it("escapes double-quotes in field values by doubling them", () => {
    const symbol: SqrySymbolResult = {
      name: 'say"hello"',
      kind: "function",
      filePath: "/project/src.rs",
      startLine: 1,
      language: "rust",
    };
    const lines = exportAsCsv([symbol]).split("\n");
    expect(lines[1]).to.include('"say""hello"""');
  });

  it("escapes newlines in field values with RFC 4180 quoting", () => {
    const symbol: SqrySymbolResult = {
      name: "line1\nline2",
      kind: "function",
      filePath: "/project/src.rs",
      startLine: 1,
      language: "rust",
    };
    const output = exportAsCsv([symbol]);
    // The entire field must be quoted
    expect(output).to.include('"line1\nline2"');
  });

  it("does not quote plain values without special characters", () => {
    const lines = exportAsCsv([SYMBOL_A]).split("\n");
    const dataLine = lines[1];
    // Plain value should not be wrapped in extra quotes
    expect(dataLine.startsWith('"')).to.equal(false);
  });
});
