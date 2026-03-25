// Fixture TypeScript module for LSP integration tests.

export interface Result {
  value: string;
  length: number;
}

export default function transform(input: string): Result {
  const trimmed = input.trim();
  return { value: trimmed, length: trimmed.length };
}

export function describe(value: string): string;
export function describe(value: number): string;
export function describe(value: string | number): string {
  const prefix = "résumé"; // combining mark to test UTF-16 conversions
  return `${prefix}:${value}`;
}

export function callEmoji(): string {
  const star = "⭐";
  return star.repeat(2);
}
