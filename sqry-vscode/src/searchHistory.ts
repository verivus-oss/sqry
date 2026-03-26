/**
 * Search history with MRU (most recently used) recall.
 *
 * Pure functions for testability — no VS Code dependencies.
 */

export interface SearchHistoryEntry {
  query: string;
  timestamp: number;
}

/**
 * Add a query to the search history.
 *
 * - Deduplicates: if the query already exists, it is moved to the front.
 * - Trims the list to `maxSize` entries, dropping the oldest.
 * - Returns a new array (does not mutate the input).
 */
export function addToHistory(
  history: SearchHistoryEntry[],
  query: string,
  maxSize: number = 20,
): SearchHistoryEntry[] {
  const entry: SearchHistoryEntry = { query, timestamp: Date.now() };
  const filtered = history.filter((item) => item.query !== query);
  const updated = [entry, ...filtered];
  return updated.slice(0, maxSize);
}

/**
 * Clear all search history entries.
 */
export function clearHistory(): SearchHistoryEntry[] {
  return [];
}

/**
 * Format a timestamp as a human-readable relative time string.
 */
export function formatRelativeTime(timestamp: number): string {
  const seconds = Math.floor((Date.now() - timestamp) / 1000);

  if (seconds < 60) {
    return "just now";
  }

  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) {
    return `${minutes}m ago`;
  }

  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    return `${hours}h ago`;
  }

  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}
