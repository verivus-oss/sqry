export interface LspPosition {
  readonly line: number;
  readonly character: number;
}

export interface LspRange {
  readonly start: LspPosition;
  readonly end: LspPosition;
}

export interface LspLocation {
  readonly uri: string;
  readonly range: LspRange;
}

export interface SqrySearchParams {
  readonly query: string;
  readonly path?: string;
  readonly limit?: number;
}

export interface SqrySearchItem {
  readonly name: string;
  readonly kind?: string;
  readonly qualified_name?: string;
  readonly language?: string;
  readonly location: LspLocation;
  readonly score?: number;
}

export interface SqrySearchResult {
  readonly results: SqrySearchItem[];
  readonly total: number;
  readonly truncated: boolean;
  readonly used_index: boolean;
}

export type SqryRelationKind =
  | "callers"
  | "callees"
  | "imports"
  | "exports"
  | "returns";

export interface SqryRelationParams {
  readonly relation: SqryRelationKind;
  readonly target: string;
  readonly path?: string;
  readonly limit?: number;
}

export interface SqryRelationResult {
  readonly relation: SqryRelationKind;
  readonly results: SqrySearchItem[];
  readonly total: number;
  readonly truncated: boolean;
  readonly used_index: boolean;
}

export interface SqryIndexStatusParams {
  readonly path?: string;
}

export interface SqryIndexStatus {
  readonly exists: boolean;
  readonly path?: string;
  readonly created_at?: string;
  readonly age_seconds?: number;
  readonly symbol_count?: number;
  readonly file_count?: number;
  readonly languages?: string[];
  readonly supports_fuzzy: boolean;
  readonly supports_relations: boolean;
  readonly cross_language_relation_count?: number;

  /** Symbol counts grouped by kind (e.g., {"function": 25669, "class": 332}) */
  readonly symbol_counts_by_kind?: Record<string, number>;

  /** File counts grouped by language (e.g., {"rust": 1245, "javascript": 523}) */
  readonly file_counts_by_language?: Record<string, number>;

  /** Relation counts grouped by language pair (e.g., {"go→javascript": 45}) */
  readonly relation_counts_by_pair?: Record<string, number>;

  readonly stale?: boolean;
  readonly building?: boolean;
  readonly build_age_seconds?: number;
}

/**
 * LSP response wrapper for sqry/indexStatus.
 * The LSP server wraps IndexStatus in a `status` field.
 */
export interface SqryIndexStatusResult {
  readonly status: SqryIndexStatus;
}

// ===== List Files Endpoint =====

export interface SqryListFilesParams {
  readonly path?: string;
  readonly offset?: number;
  readonly limit?: number;
}

export interface SqryListFilesResult {
  readonly files: string[];
  readonly total: number;
  readonly offset: number;
  readonly limit: number;
  readonly has_more: boolean;
}

// ===== List Symbols Endpoint =====

export interface SqryListSymbolsParams {
  readonly path?: string;
  readonly offset?: number;
  readonly limit?: number;
  /** Filter by symbol kind (e.g., "function", "class", "method") */
  readonly kind?: string;
}

export interface SqryListSymbolsResult {
  readonly symbols: SqrySearchItem[];
  readonly total: number;
  readonly offset: number;
  readonly limit: number;
  readonly has_more: boolean;
}

// ===== List Files by Language Endpoint =====

export interface SqryListFilesByLanguageParams {
  readonly language: string;
  readonly path?: string;
  readonly offset?: number;
  readonly limit?: number;
}

export interface SqryListFilesByLanguageResult {
  readonly language: string;
  readonly files: string[];
  readonly total: number;
  readonly offset: number;
  readonly limit: number;
  readonly has_more: boolean;
}

// ===== Cross-Language Relations Endpoint =====

/** Sort order for cross-language results */
export type SortOrder = "alphabetical" | "byFrequency" | "byRelevance";

export interface CrossLanguageRelation {
  readonly relation_type: string;
  readonly from_symbol: string;
  readonly from_language: string;
  readonly from_file: string;
  readonly to_symbol: string;
  readonly to_language: string;
  readonly to_file?: string;
}

/** Overflow information when results are truncated */
export interface OverflowInfo {
  readonly total_dropped: number;
  readonly truncated_pairs: [string, string][];
}

export interface SqryListCrossLanguageRelationsParams {
  readonly path?: string;
  readonly offset?: number;
  readonly limit?: number;
  readonly sort_order?: SortOrder;
  /** Filter by source language (e.g., "rust", "go") */
  readonly source_language?: string;
  /** Filter by target language (e.g., "javascript", "python") */
  readonly target_language?: string;
}

export interface SqryListCrossLanguageRelationsResult {
  readonly relations: CrossLanguageRelation[];
  readonly total: number;
  readonly offset: number;
  readonly limit: number;
  readonly has_more: boolean;
  readonly overflow?: OverflowInfo;
}

// ===== Duplicate Groups Endpoint =====

export interface SqryListDuplicateGroupsParams {
  readonly path?: string;
  /** Type of duplicate to detect: "body", "signature", or "struct" */
  readonly duplicate_type?: string;
  readonly limit?: number;
}

/** A group of duplicate symbols sharing the same hash */
export interface SqryDuplicateGroup {
  /** Unique identifier for this duplicate group (hash) */
  readonly group_id: string;
  /** Number of symbols in this group */
  readonly count: number;
  /** Representative name for the group (first symbol's name) */
  readonly representative_name: string;
  /** All symbols in the duplicate group */
  readonly symbols: SqrySearchItem[];
}

export interface SqryListDuplicateGroupsResult {
  readonly groups: SqryDuplicateGroup[];
  readonly total_groups: number;
  readonly total_symbols: number;
  /** Whether results were truncated due to limit */
  readonly truncated: boolean;
}

// ===== Circular Dependencies Endpoint =====

export interface SqryListCircularDependenciesParams {
  readonly path?: string;
  /** Type of circular dependency: "calls", "imports", or "modules" */
  readonly circular_type?: string;
  readonly limit?: number;
  /** Include self-loops (A -> A) in results */
  readonly should_include_self_loops?: boolean;
}

/** A cycle in the dependency graph */
export interface SqryCycle {
  /** Unique identifier for this cycle (hash of members) */
  readonly cycle_id: string;
  /** Number of nodes in the cycle */
  readonly depth: number;
  /** Nodes in the cycle (symbol names or file paths) */
  readonly members: string[];
  /** Type of cycle ("calls", "imports", "modules") */
  readonly cycle_type: string;
}

export interface SqryListCircularDependenciesResult {
  readonly cycles: SqryCycle[];
  readonly total_cycles: number;
  /** Whether results were truncated due to limit */
  readonly truncated: boolean;
}

// ===== Unused Symbols Endpoint =====

export interface SqryListUnusedSymbolsParams {
  readonly path?: string;
  /** Scope of unused analysis: "public", "private", "function", "struct", or "all" */
  readonly scope?: string;
  readonly limit?: number;
}

export interface SqryListUnusedSymbolsResult {
  readonly symbols: SqrySearchItem[];
  readonly total: number;
  /** Whether results were truncated due to limit */
  readonly truncated: boolean;
  /** Scope that was applied */
  readonly scope: string;
}
