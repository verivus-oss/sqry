import { SqrySymbolResult, SqryTextMatch, SqryResult } from "./types";

export function parseQueryOutput(stdout: string): SqryResult {
  try {
    const parsed = JSON.parse(stdout);
    const symbols = toSymbols(parsed);
    const textMatches = toTextMatches(parsed?.text_matches);

    return {
      symbols,
      textMatches,
      raw: parsed,
    };
  } catch (error) {
    throw new Error(
      `sqry returned invalid JSON. Ensure you are running the latest CLI. ${error}`,
    );
  }
}

export function parseSearchOutput(stdout: string): SqryResult {
  const events = stdout
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      try {
        return JSON.parse(line);
      } catch (error) {
        throw new Error(`Failed to parse sqry search event: ${error}`);
      }
    });

  const textMatches: SqryTextMatch[] = [];
  for (const searchEvent of events) {
    if (Array.isArray(searchEvent?.matches)) {
      textMatches.push(
        ...searchEvent.matches.map((matchItem: any) => ({
          path: matchItem.path,
          line: matchItem.line,
          lineText: matchItem.line_text ?? matchItem.lineText,
        })),
      );
    }
    if (searchEvent?.match) {
      textMatches.push({
        path: searchEvent.match.path,
        line: searchEvent.match.line,
        lineText: searchEvent.match.line_text ?? searchEvent.match.lineText,
      });
    }
  }

  return {
    symbols: [],
    textMatches,
    raw: events,
  };
}

function toSymbols(value: unknown): SqrySymbolResult[] {
  const parsedResponse = value as any;

  // Determine raw symbols from either parsedResponse.symbols or value itself
  let rawSymbols: any[] | undefined;
  if (Array.isArray(parsedResponse?.symbols)) {
    rawSymbols = parsedResponse.symbols;
  } else if (Array.isArray(value)) {
    rawSymbols = value;
  }

  if (!rawSymbols) {
    return [];
  }

  return rawSymbols.map((symbol: any) => ({
    name: String(symbol?.name ?? ""),
    kind: symbol?.kind,
    filePath: String(symbol?.file_path ?? symbol?.filePath ?? ""),
    startLine: Number(symbol?.start_line ?? symbol?.startLine ?? 1),
    qualifiedName: symbol?.qualified_name ?? symbol?.qualifiedName,
    language: symbol?.language,
  }));
}

function toTextMatches(value: unknown): SqryTextMatch[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.map((match: any) => ({
    path: String(match.path ?? ""),
    line: Number(match.line ?? 0),
    lineText: match.line_text ?? match.lineText,
  }));
}
