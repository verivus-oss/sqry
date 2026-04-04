import { SqrySymbolResult } from "./types";

export type SortOrder = "default" | "name" | "file" | "kind" | "line";

export interface ActiveFilters {
  languages: Set<string>;
  kinds: Set<string>;
  pathGlob: string | null;
}

/**
 * Apply language, kind, and path filters to a list of symbols.
 * An empty Set for languages/kinds means "no filter" (all pass).
 */
export function applyFilters(
  symbols: SqrySymbolResult[],
  filters: ActiveFilters,
): SqrySymbolResult[] {
  let filtered = symbols;

  if (filters.languages.size > 0) {
    filtered = filtered.filter(
      (s) => s.language !== undefined && filters.languages.has(s.language),
    );
  }

  if (filters.kinds.size > 0) {
    filtered = filtered.filter(
      (s) => s.kind !== undefined && filters.kinds.has(s.kind),
    );
  }

  if (filters.pathGlob !== null && filters.pathGlob.length > 0) {
    const glob = filters.pathGlob;
    filtered = filtered.filter((s) => s.filePath.includes(glob));
  }

  return filtered;
}

/**
 * Sort a list of symbols by the given order.
 * Returns a new array; does not mutate the input.
 */
export function sortSymbols(
  symbols: SqrySymbolResult[],
  order: SortOrder,
): SqrySymbolResult[] {
  if (order === "default") {
    return symbols;
  }

  const sorted = [...symbols];

  switch (order) {
    case "name":
      sorted.sort((a, b) => a.name.localeCompare(b.name));
      break;
    case "file":
      sorted.sort((a, b) => a.filePath.localeCompare(b.filePath));
      break;
    case "kind":
      sorted.sort((a, b) => (a.kind ?? "").localeCompare(b.kind ?? ""));
      break;
    case "line":
      sorted.sort((a, b) => (a.startLine ?? 0) - (b.startLine ?? 0));
      break;
  }

  return sorted;
}

/**
 * Apply filters then sort.
 */
export function applyFiltersAndSort(
  symbols: SqrySymbolResult[],
  filters: ActiveFilters,
  order: SortOrder,
): SqrySymbolResult[] {
  return sortSymbols(applyFilters(symbols, filters), order);
}

/**
 * Build a human-readable summary string for the current filter state.
 * Returns empty string when no filters are active.
 */
export function buildFilterSummary(filters: ActiveFilters): string {
  const parts: string[] = [];

  if (filters.languages.size > 0) {
    parts.push([...filters.languages].sort((a, b) => a.localeCompare(b)).join(", "));
  }

  if (filters.kinds.size > 0) {
    parts.push([...filters.kinds].sort((a, b) => a.localeCompare(b)).join(", "));
  }

  if (filters.pathGlob !== null && filters.pathGlob.length > 0) {
    parts.push(`path:${filters.pathGlob}`);
  }

  if (parts.length === 0) {
    return "";
  }

  return `Filtered: ${parts.join(" | ")}`;
}

/**
 * Extract unique languages from a symbol list, sorted alphabetically.
 */
export function extractLanguages(symbols: SqrySymbolResult[]): string[] {
  const langs = new Set<string>();
  for (const s of symbols) {
    if (s.language) {
      langs.add(s.language);
    }
  }
  return [...langs].sort((a, b) => a.localeCompare(b));
}

/**
 * Extract unique kinds from a symbol list, sorted alphabetically.
 */
export function extractKinds(symbols: SqrySymbolResult[]): string[] {
  const kinds = new Set<string>();
  for (const s of symbols) {
    if (s.kind) {
      kinds.add(s.kind);
    }
  }
  return [...kinds].sort((a, b) => a.localeCompare(b));
}

/**
 * Create a default (empty) ActiveFilters object.
 */
export function makeDefaultFilters(): ActiveFilters {
  return { languages: new Set(), kinds: new Set(), pathGlob: null };
}
