import { expect } from "chai";
import {
  addToHistory,
  clearHistory,
  formatRelativeTime,
  SearchHistoryEntry,
} from "../src/searchHistory";

describe("searchHistory", () => {
  describe("addToHistory", () => {
    it("appends entry to empty list", () => {
      const result = addToHistory([], "kind:function");
      expect(result).to.have.length(1);
      expect(result[0].query).to.equal("kind:function");
      expect(result[0].timestamp).to.be.a("number");
    });

    it("prepends new entry to front of list", () => {
      const existing: SearchHistoryEntry[] = [
        { query: "callers:parse", timestamp: 1000 },
      ];
      const result = addToHistory(existing, "kind:function");
      expect(result).to.have.length(2);
      expect(result[0].query).to.equal("kind:function");
      expect(result[1].query).to.equal("callers:parse");
    });

    it("deduplicates: moves existing query to top", () => {
      const existing: SearchHistoryEntry[] = [
        { query: "kind:function", timestamp: 3000 },
        { query: "callers:parse", timestamp: 2000 },
        { query: "returns:Result", timestamp: 1000 },
      ];
      const result = addToHistory(existing, "callers:parse");
      expect(result).to.have.length(3);
      expect(result[0].query).to.equal("callers:parse");
      expect(result[1].query).to.equal("kind:function");
      expect(result[2].query).to.equal("returns:Result");
    });

    it("does not create duplicates", () => {
      const existing: SearchHistoryEntry[] = [
        { query: "kind:function", timestamp: 2000 },
        { query: "callers:parse", timestamp: 1000 },
      ];
      const result = addToHistory(existing, "kind:function");
      const queries = result.map((entry) => entry.query);
      const unique = new Set(queries);
      expect(unique.size).to.equal(queries.length);
    });

    it("limits to max 20 entries by default", () => {
      const existing: SearchHistoryEntry[] = [];
      for (let i = 0; i < 25; i++) {
        existing.push({ query: `query_${i}`, timestamp: i });
      }
      const result = addToHistory(existing, "new_query");
      expect(result).to.have.length(20);
      expect(result[0].query).to.equal("new_query");
    });

    it("drops oldest entries when exceeding limit", () => {
      const existing: SearchHistoryEntry[] = [];
      for (let i = 0; i < 20; i++) {
        existing.push({ query: `query_${i}`, timestamp: i });
      }
      const result = addToHistory(existing, "new_query");
      expect(result).to.have.length(20);
      expect(result[0].query).to.equal("new_query");
      // The last entry (oldest) should be query_18 (query_19 was index 19, now at index 20 = dropped)
      expect(result[result.length - 1].query).to.equal("query_18");
    });

    it("respects custom maxSize", () => {
      const existing: SearchHistoryEntry[] = [
        { query: "a", timestamp: 3 },
        { query: "b", timestamp: 2 },
        { query: "c", timestamp: 1 },
      ];
      const result = addToHistory(existing, "d", 2);
      expect(result).to.have.length(2);
      expect(result[0].query).to.equal("d");
      expect(result[1].query).to.equal("a");
    });

    it("does not mutate the input array", () => {
      const existing: SearchHistoryEntry[] = [
        { query: "callers:parse", timestamp: 1000 },
      ];
      const copy = [...existing];
      addToHistory(existing, "kind:function");
      expect(existing).to.deep.equal(copy);
    });

    it("updates timestamp when moving existing query to top", () => {
      const existing: SearchHistoryEntry[] = [
        { query: "callers:parse", timestamp: 1000 },
      ];
      const result = addToHistory(existing, "callers:parse");
      expect(result).to.have.length(1);
      expect(result[0].query).to.equal("callers:parse");
      expect(result[0].timestamp).to.be.greaterThan(1000);
    });

    it("includes timestamp as a number", () => {
      const result = addToHistory([], "kind:function");
      expect(result[0].timestamp).to.be.a("number");
      expect(result[0].timestamp).to.be.greaterThan(0);
    });
  });

  describe("clearHistory", () => {
    it("returns an empty array", () => {
      const result = clearHistory();
      expect(result).to.deep.equal([]);
    });
  });

  describe("formatRelativeTime", () => {
    it("shows 'just now' for recent timestamps", () => {
      const result = formatRelativeTime(Date.now() - 10_000);
      expect(result).to.equal("just now");
    });

    it("shows minutes for timestamps under an hour", () => {
      const result = formatRelativeTime(Date.now() - 5 * 60 * 1000);
      expect(result).to.equal("5m ago");
    });

    it("shows hours for timestamps under a day", () => {
      const result = formatRelativeTime(Date.now() - 3 * 60 * 60 * 1000);
      expect(result).to.equal("3h ago");
    });

    it("shows days for older timestamps", () => {
      const result = formatRelativeTime(Date.now() - 2 * 24 * 60 * 60 * 1000);
      expect(result).to.equal("2d ago");
    });
  });
});
