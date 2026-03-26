import { expect } from "chai";
import {
  applyFilters,
  applyFiltersAndSort,
  buildFilterSummary,
  extractKinds,
  extractLanguages,
  makeDefaultFilters,
  sortSymbols,
  ActiveFilters,
} from "../src/filterSort";
import { SqrySymbolResult } from "../src/types";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

function makeSymbol(
  name: string,
  kind: string,
  language: string,
  filePath: string,
  startLine: number,
): SqrySymbolResult {
  return { name, kind, language, filePath, startLine };
}

const rustFn1 = makeSymbol("parse_input", "function", "rust", "/project/src/parser.rs", 10);
const rustFn2 = makeSymbol("build_graph", "function", "rust", "/project/src/graph.rs", 5);
const rustClass = makeSymbol("GraphBuilder", "class", "rust", "/project/src/graph.rs", 1);
const jsFn = makeSymbol("fetchData", "function", "javascript", "/project/web/api.js", 20);
const jsClass = makeSymbol("ApiClient", "class", "javascript", "/project/web/client.js", 3);

const MIXED: SqrySymbolResult[] = [rustFn1, rustFn2, rustClass, jsFn, jsClass];

// ---------------------------------------------------------------------------
// applyFilters — language filter
// ---------------------------------------------------------------------------

describe("filterSort — language filter", () => {
  it("returns only Rust results when filtering to Rust", () => {
    const filters: ActiveFilters = {
      languages: new Set(["rust"]),
      kinds: new Set(),
      pathGlob: null,
    };
    const result = applyFilters(MIXED, filters);
    expect(result).to.have.length(3);
    for (const s of result) {
      expect(s.language).to.equal("rust");
    }
  });

  it("returns only JS results when filtering to javascript", () => {
    const filters: ActiveFilters = {
      languages: new Set(["javascript"]),
      kinds: new Set(),
      pathGlob: null,
    };
    const result = applyFilters(MIXED, filters);
    expect(result).to.have.length(2);
    for (const s of result) {
      expect(s.language).to.equal("javascript");
    }
  });

  it("returns all results when language filter is empty", () => {
    const result = applyFilters(MIXED, makeDefaultFilters());
    expect(result).to.have.length(MIXED.length);
  });

  it("returns empty array when no symbols match the language filter", () => {
    const filters: ActiveFilters = {
      languages: new Set(["python"]),
      kinds: new Set(),
      pathGlob: null,
    };
    const result = applyFilters(MIXED, filters);
    expect(result).to.have.length(0);
  });
});

// ---------------------------------------------------------------------------
// applyFilters — kind filter
// ---------------------------------------------------------------------------

describe("filterSort — kind filter", () => {
  it("returns only functions when filtering to function", () => {
    const filters: ActiveFilters = {
      languages: new Set(),
      kinds: new Set(["function"]),
      pathGlob: null,
    };
    const result = applyFilters(MIXED, filters);
    expect(result).to.have.length(3);
    for (const s of result) {
      expect(s.kind).to.equal("function");
    }
  });

  it("returns only classes when filtering to class", () => {
    const filters: ActiveFilters = {
      languages: new Set(),
      kinds: new Set(["class"]),
      pathGlob: null,
    };
    const result = applyFilters(MIXED, filters);
    expect(result).to.have.length(2);
    for (const s of result) {
      expect(s.kind).to.equal("class");
    }
  });

  it("returns all results when kind filter is empty", () => {
    const result = applyFilters(MIXED, makeDefaultFilters());
    expect(result).to.have.length(MIXED.length);
  });
});

// ---------------------------------------------------------------------------
// applyFilters — combined language + kind
// ---------------------------------------------------------------------------

describe("filterSort — combined language and kind filter", () => {
  it("returns only Rust functions when both filters are set", () => {
    const filters: ActiveFilters = {
      languages: new Set(["rust"]),
      kinds: new Set(["function"]),
      pathGlob: null,
    };
    const result = applyFilters(MIXED, filters);
    expect(result).to.have.length(2);
    for (const s of result) {
      expect(s.language).to.equal("rust");
      expect(s.kind).to.equal("function");
    }
  });
});

// ---------------------------------------------------------------------------
// applyFilters — path glob
// ---------------------------------------------------------------------------

describe("filterSort — path glob filter", () => {
  it("filters by path substring", () => {
    const filters: ActiveFilters = {
      languages: new Set(),
      kinds: new Set(),
      pathGlob: "web",
    };
    const result = applyFilters(MIXED, filters);
    expect(result).to.have.length(2);
    for (const s of result) {
      expect(s.filePath).to.include("web");
    }
  });

  it("passes all when pathGlob is null", () => {
    const result = applyFilters(MIXED, makeDefaultFilters());
    expect(result).to.have.length(MIXED.length);
  });
});

// ---------------------------------------------------------------------------
// sortSymbols
// ---------------------------------------------------------------------------

describe("filterSort — sort by name", () => {
  it("sorts alphabetically by name ascending", () => {
    const result = sortSymbols(MIXED, "name");
    const names = result.map((s) => s.name);
    const sorted = [...names].sort((a, b) => a.localeCompare(b));
    expect(names).to.deep.equal(sorted);
  });

  it("does not mutate the input array", () => {
    const original = [...MIXED];
    sortSymbols(MIXED, "name");
    expect(MIXED).to.deep.equal(original);
  });
});

describe("filterSort — sort by file path", () => {
  it("sorts alphabetically by filePath ascending", () => {
    const result = sortSymbols(MIXED, "file");
    const paths = result.map((s) => s.filePath);
    const sorted = [...paths].sort((a, b) => a.localeCompare(b));
    expect(paths).to.deep.equal(sorted);
  });

  it("groups results by file when sorted by file", () => {
    const result = sortSymbols(MIXED, "file");
    // Symbols from the same file should be adjacent
    let prevPath = result[0].filePath;
    const seenPaths = new Set<string>([prevPath]);
    for (let i = 1; i < result.length; i++) {
      const currPath = result[i].filePath;
      if (currPath !== prevPath) {
        // Moving to a new path — must not have appeared before
        expect(seenPaths.has(currPath)).to.equal(false,
          `Path ${currPath} appeared non-contiguously`);
        seenPaths.add(currPath);
        prevPath = currPath;
      }
    }
  });
});

describe("filterSort — sort by kind", () => {
  it("sorts alphabetically by kind ascending", () => {
    const result = sortSymbols(MIXED, "kind");
    const kinds = result.map((s) => s.kind ?? "");
    const sorted = [...kinds].sort((a, b) => a.localeCompare(b));
    expect(kinds).to.deep.equal(sorted);
  });
});

describe("filterSort — sort by line", () => {
  it("sorts numerically by startLine ascending", () => {
    const result = sortSymbols(MIXED, "line");
    const lines = result.map((s) => s.startLine ?? 0);
    for (let i = 1; i < lines.length; i++) {
      expect(lines[i]).to.be.at.least(lines[i - 1]);
    }
  });
});

describe("filterSort — default sort order", () => {
  it("returns a copy with the same order as input", () => {
    const result = sortSymbols(MIXED, "default");
    expect(result.map((s) => s.name)).to.deep.equal(MIXED.map((s) => s.name));
  });
});

// ---------------------------------------------------------------------------
// clearFilters / makeDefaultFilters
// ---------------------------------------------------------------------------

describe("filterSort — makeDefaultFilters (clearFilters)", () => {
  it("produces empty sets and null pathGlob", () => {
    const filters = makeDefaultFilters();
    expect(filters.languages.size).to.equal(0);
    expect(filters.kinds.size).to.equal(0);
    expect(filters.pathGlob).to.be.null;
  });

  it("applyFilters with default filters returns all original results", () => {
    const filtered = applyFilters(MIXED, makeDefaultFilters());
    expect(filtered).to.have.length(MIXED.length);
  });
});

// ---------------------------------------------------------------------------
// buildFilterSummary
// ---------------------------------------------------------------------------

describe("filterSort — buildFilterSummary", () => {
  it("returns empty string when no filters are active", () => {
    expect(buildFilterSummary(makeDefaultFilters())).to.equal("");
  });

  it("includes language names when language filter is active", () => {
    const filters: ActiveFilters = {
      languages: new Set(["rust"]),
      kinds: new Set(),
      pathGlob: null,
    };
    const summary = buildFilterSummary(filters);
    expect(summary).to.include("rust");
    expect(summary).to.include("Filtered:");
  });

  it("includes kind when kind filter is active", () => {
    const filters: ActiveFilters = {
      languages: new Set(),
      kinds: new Set(["function"]),
      pathGlob: null,
    };
    const summary = buildFilterSummary(filters);
    expect(summary).to.include("function");
    expect(summary).to.include("Filtered:");
  });

  it("includes both language and kind when both filters are active", () => {
    const filters: ActiveFilters = {
      languages: new Set(["rust"]),
      kinds: new Set(["function"]),
      pathGlob: null,
    };
    const summary = buildFilterSummary(filters);
    expect(summary).to.include("rust");
    expect(summary).to.include("function");
  });

  it("includes path glob in summary", () => {
    const filters: ActiveFilters = {
      languages: new Set(),
      kinds: new Set(),
      pathGlob: "src",
    };
    const summary = buildFilterSummary(filters);
    expect(summary).to.include("path:src");
  });
});

// ---------------------------------------------------------------------------
// extractLanguages / extractKinds
// ---------------------------------------------------------------------------

describe("filterSort — extractLanguages", () => {
  it("returns sorted unique languages from symbol list", () => {
    const langs = extractLanguages(MIXED);
    expect(langs).to.deep.equal(["javascript", "rust"]);
  });

  it("excludes symbols without language", () => {
    const symbols: SqrySymbolResult[] = [
      { name: "foo", filePath: "/a.rs", startLine: 1 },
      { name: "bar", language: "rust", filePath: "/b.rs", startLine: 2 },
    ];
    const langs = extractLanguages(symbols);
    expect(langs).to.deep.equal(["rust"]);
  });

  it("returns empty array for empty input", () => {
    expect(extractLanguages([])).to.deep.equal([]);
  });
});

describe("filterSort — extractKinds", () => {
  it("returns sorted unique kinds from symbol list", () => {
    const kinds = extractKinds(MIXED);
    expect(kinds).to.deep.equal(["class", "function"]);
  });

  it("excludes symbols without kind", () => {
    const symbols: SqrySymbolResult[] = [
      { name: "foo", filePath: "/a.rs", startLine: 1 },
      { name: "bar", kind: "function", filePath: "/b.rs", startLine: 2 },
    ];
    const kinds = extractKinds(symbols);
    expect(kinds).to.deep.equal(["function"]);
  });

  it("returns empty array for empty input", () => {
    expect(extractKinds([])).to.deep.equal([]);
  });
});

// ---------------------------------------------------------------------------
// applyFiltersAndSort — integration
// ---------------------------------------------------------------------------

describe("filterSort — applyFiltersAndSort", () => {
  it("filters to Rust functions and sorts by name", () => {
    const filters: ActiveFilters = {
      languages: new Set(["rust"]),
      kinds: new Set(["function"]),
      pathGlob: null,
    };
    const result = applyFiltersAndSort(MIXED, filters, "name");
    expect(result).to.have.length(2);
    expect(result[0].name).to.equal("build_graph");
    expect(result[1].name).to.equal("parse_input");
  });
});
