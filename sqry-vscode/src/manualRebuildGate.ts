/**
 * Manual-rebuild gate — every user-facing rebuild command MUST funnel
 * through this helper before invoking `runIndex`.
 *
 * STEP_5 acceptance contract (codex iter1 MAJOR fix):
 * - All manual rebuild commands (`sqry.index`, `sqry.rebuildIndex`,
 *   plus any future command that writes the on-disk graph) gate on
 *   `LoadingPhase === "Ready"`.
 * - The wait MUST honour the DAG-mandated 30s timeout
 *   (`MANUAL_GATE_TIMEOUT_MS`).
 * - When the wait expires, the gate rejects with a stable error
 *   identified by [`MANUAL_REBUILD_GATE_TIMEOUT`] — UI and tests can
 *   match on this constant rather than parsing the message string.
 *
 * The helper has no dependency on the VS Code extension host; it
 * accepts a minimal `ReadyGate` interface so tests (and future
 * subagents writing alternative loading-state implementations) can
 * exercise the gate without a live `LoadingStateMachine`.
 */
import { MANUAL_GATE_TIMEOUT_MS } from "./loadingState";

/**
 * Stable identifier surfaced when the gate expires before the LSP
 * reaches `Ready`. Tests and UI handlers should match on this constant
 * via [`isManualRebuildGateTimeout`] rather than the human-readable
 * message string (which is locale-dependent).
 */
export const MANUAL_REBUILD_GATE_TIMEOUT = "sqry/manual-rebuild-gate-timeout";

export class ManualRebuildGateTimeoutError extends Error {
  /** Stable wire identifier — see [`MANUAL_REBUILD_GATE_TIMEOUT`]. */
  public readonly code = MANUAL_REBUILD_GATE_TIMEOUT;
  /** Original wait timeout in milliseconds, for diagnostics. */
  public readonly timeoutMs: number;
  constructor(timeoutMs: number, cause?: unknown) {
    super(
      `sqry: manual rebuild blocked — language server did not reach Ready within ${timeoutMs}ms`,
    );
    this.name = "ManualRebuildGateTimeoutError";
    this.timeoutMs = timeoutMs;
    if (cause !== undefined) {
      // Preserve the original cause for diagnostic logging (Node 16.9+).
      (this as unknown as { cause?: unknown }).cause = cause;
    }
  }
}

/** Type guard for the gate timeout — prefer this over `instanceof` checks. */
export function isManualRebuildGateTimeout(err: unknown): boolean {
  return (
    typeof err === "object" &&
    err !== null &&
    (err as { code?: unknown }).code === MANUAL_REBUILD_GATE_TIMEOUT
  );
}

/**
 * Minimal subset of [`LoadingStateMachine`] the gate needs. Keeping the
 * surface tiny makes the helper trivially mockable from tests without
 * requiring a real `LoadingStateMachine` instance.
 */
export interface ReadyGate {
  /** True iff the LSP has reached the `Ready` phase. */
  isReady(): boolean;
  /**
   * Wait for the gate to reach `Ready`. Resolves immediately when
   * already at `Ready`; rejects with the underlying `LoadingState`
   * error when the LSP terminally `Failed`; otherwise resolves on the
   * next `Ready` transition or rejects after `timeoutMs`.
   */
  waitForReady(timeoutMs?: number): Promise<void>;
}

export interface GatedManualRebuildOptions {
  /** Override the DAG-mandated 30s timeout — tests use a short value. */
  readonly timeoutMs?: number;
  /** Optional logger; receives one structured event per gate decision. */
  readonly log?: (event: GatedManualRebuildEvent) => void;
}

export type GatedManualRebuildEvent =
  | { readonly kind: "gate-immediate"; readonly phaseAtEntry: "Ready" }
  | { readonly kind: "gate-waited"; readonly waitedMs: number }
  | { readonly kind: "gate-timeout"; readonly timeoutMs: number; readonly cause?: string }
  | { readonly kind: "gate-failed"; readonly reason: string };

/**
 * Gate `runner` on the LSP reaching `Ready`. The runner is invoked
 * exactly once after the gate opens; its result (or rejection) is
 * propagated verbatim. Gate failures throw [`ManualRebuildGateTimeoutError`]
 * (timeout) or rethrow the underlying `Failed` reason (terminal LSP
 * failure) — `runner` is NOT invoked in either case.
 *
 * The 30-second default mirrors `MANUAL_GATE_TIMEOUT_MS` from the
 * loading-state machine; tests pass a smaller override.
 */
export async function gatedManualRebuild<T>(
  gate: ReadyGate,
  runner: () => Promise<T>,
  options: GatedManualRebuildOptions = {},
): Promise<T> {
  const timeoutMs = options.timeoutMs ?? MANUAL_GATE_TIMEOUT_MS;
  const log = options.log;
  if (gate.isReady()) {
    log?.({ kind: "gate-immediate", phaseAtEntry: "Ready" });
    return runner();
  }
  const startedAt = Date.now();
  try {
    await gate.waitForReady(timeoutMs);
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    // `LoadingStateMachine.waitForReady` embeds the substring
    // "timed out after" in its timeout rejection. Match on that so
    // we can wrap the typed `ManualRebuildGateTimeoutError`; any
    // other rejection comes from the state machine entering
    // `Failed`, which we propagate verbatim.
    if (reason.includes("timed out after") && !gate.isReady()) {
      log?.({ kind: "gate-timeout", timeoutMs, cause: reason });
      throw new ManualRebuildGateTimeoutError(timeoutMs, err);
    }
    log?.({ kind: "gate-failed", reason });
    throw err;
  }
  log?.({ kind: "gate-waited", waitedMs: Date.now() - startedAt });
  return runner();
}
