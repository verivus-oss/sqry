import { SqrySymbolResult } from "./types";

/**
 * Export search results as a JSON string.
 * Returns a pretty-printed JSON array with name, kind, file, line, and language fields.
 */
export function exportAsJson(symbols: SqrySymbolResult[]): string {
  return JSON.stringify(
    symbols.map((s) => ({
      name: s.name,
      kind: s.kind ?? "",
      file: s.filePath,
      line: s.startLine,
      language: s.language ?? "",
    })),
    null,
    2,
  );
}

/**
 * Export search results as a Markdown table.
 * Produces a pipe-delimited table with header and separator rows.
 */
export function exportAsMarkdown(symbols: SqrySymbolResult[]): string {
  const lines = [
    "| Name | Kind | File | Line | Language |",
    "|------|------|------|------|----------|",
  ];
  for (const s of symbols) {
    lines.push(
      `| ${s.name} | ${s.kind ?? ""} | ${s.filePath} | ${s.startLine} | ${s.language ?? ""} |`,
    );
  }
  return lines.join("\n");
}

/**
 * Export search results as CSV.
 * The first line is the header row. Values containing commas, double-quotes,
 * or newlines are RFC 4180 quoted.
 */
export function exportAsCsv(symbols: SqrySymbolResult[]): string {
  const lines = ["Name,Kind,File,Line,Language"];
  for (const s of symbols) {
    lines.push(
      [
        csvEscape(s.name),
        csvEscape(s.kind ?? ""),
        csvEscape(s.filePath),
        String(s.startLine),
        csvEscape(s.language ?? ""),
      ].join(","),
    );
  }
  return lines.join("\n");
}

/**
 * Escape a string value for inclusion in a CSV field.
 * Wraps in double quotes when the value contains commas, double-quotes, or newlines,
 * and doubles any embedded double-quote characters per RFC 4180.
 */
function csvEscape(value: string): string {
  if (value.includes(",") || value.includes('"') || value.includes("\n")) {
    return `"${value.replaceAll('"', '""')}"`;
  }
  return value;
}
