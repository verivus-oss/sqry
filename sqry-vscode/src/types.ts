export interface SqrySymbolResult {
  readonly name: string;
  readonly kind?: string;
  readonly filePath: string;
  readonly startLine: number;
  readonly qualifiedName?: string;
  readonly language?: string;
}

export interface SqryTextMatch {
  readonly path: string;
  readonly line: number;
  readonly lineText?: string;
}

export interface SqryResult {
  readonly symbols: SqrySymbolResult[];
  readonly textMatches: SqryTextMatch[];
  readonly raw: unknown;
}
