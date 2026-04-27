/**
 * AutoIndexManager — manages debounced per-root index rebuilds triggered by file saves.
 *
 * Responsibilities:
 *   - Debounce: rapid save events for the same root reset the timer; only the last fires.
 *   - Dirty latch: when a save arrives while a build is in progress, the latch is set so a
 *     follow-up rebuild is scheduled immediately after the current build completes.
 *   - Per-root isolation: each workspace root has its own timer, latch, and build flag.
 *   - Source-root enqueue (STEP_5 acceptance criterion 3, codex iter1 MAJOR fix):
 *     `enqueueFromWorkspaceStatus` is the single entrypoint that consumes the LSP
 *     `getWorkspaceStatus()` aggregate and enqueues only entries whose
 *     `status === "missing"`. The filtering logic lives here, NOT in `extension.ts`.
 *     `extension.ts` is responsible only for wiring (calling this method with the
 *     LSP-supplied status + a per-source-root runner callback).
 */
import type { SqrySourceRootStatus, SqryWorkspaceStatus } from "./lspProtocol";

/**
 * Predicate the extension supplies to filter source-root paths against the
 * user-visible exclude rules. `undefined` means "no exclusion" (legacy
 * default for callers that have not wired the predicate yet). Returning
 * `true` means the path should be skipped.
 */
export type SourceRootExcludePredicate = (sourceRootPath: string) => boolean;

/**
 * Per-source-root runner. Invoked once per missing root, sequentially. The
 * runner returns a `Promise<void>` so the caller can `await` the entire
 * enqueue cycle (used by activation and `onDidChangeWorkspaceFolders`).
 */
export type SourceRootRunner = (root: SqrySourceRootStatus) => Promise<void>;

export interface EnqueueFromStatusOptions {
  /** Optional exclude predicate; entries that match are skipped silently. */
  readonly exclude?: SourceRootExcludePredicate;
}

export interface EnqueueFromStatusResult {
  /** Total entries in the aggregate. */
  readonly inspected: number;
  /** Entries that passed `status === "missing"` AND the exclude predicate. */
  readonly enqueued: number;
  /** Entries that were `missing` but rejected by the exclude predicate. */
  readonly excluded: number;
  /** Entries skipped because their `status` was not `missing`. */
  readonly nonMissing: number;
}

export class AutoIndexManager {
  private readonly timers = new Map<string, ReturnType<typeof setTimeout>>();
  private readonly dirtyLatches = new Map<string, boolean>();
  private readonly buildingRoots = new Set<string>();

  /**
   * Schedule a debounced rebuild for `rootPath`.
   *
   * If a timer is already pending for this root it is cancelled and replaced, implementing
   * the debounce behaviour.
   */
  schedule(rootPath: string, delayMs: number, callback: () => void): void {
    const existing = this.timers.get(rootPath);
    if (existing !== undefined) {
      clearTimeout(existing);
    }
    const id = setTimeout(() => {
      this.timers.delete(rootPath);
      callback();
    }, delayMs);
    this.timers.set(rootPath, id);
  }

  /** Cancel any pending debounce timer for `rootPath` without running the callback. */
  cancel(rootPath: string): void {
    const existing = this.timers.get(rootPath);
    if (existing !== undefined) {
      clearTimeout(existing);
      this.timers.delete(rootPath);
    }
  }

  /** Mark `rootPath` as actively building. */
  startBuild(rootPath: string): void {
    this.buildingRoots.add(rootPath);
    // Clear the dirty latch at the start of a new build so we track only saves that
    // arrive *during* this build.
    this.dirtyLatches.set(rootPath, false);
  }

  /**
   * Mark the build for `rootPath` as complete.
   *
   * Clears the building flag and checks the dirty latch.
   * Returns `true` if a follow-up rebuild is needed (latch was set), `false` otherwise.
   * The dirty latch is cleared regardless.
   */
  completeBuild(rootPath: string): boolean {
    this.buildingRoots.delete(rootPath);
    const dirty = this.dirtyLatches.get(rootPath) ?? false;
    this.dirtyLatches.set(rootPath, false);
    return dirty;
  }

  /** Record that a save arrived while a build was in progress for `rootPath`. */
  markDirty(rootPath: string): void {
    this.dirtyLatches.set(rootPath, true);
  }

  /** Returns `true` if a build is currently in progress for `rootPath`. */
  isBuilding(rootPath: string): boolean {
    return this.buildingRoots.has(rootPath);
  }

  /** Returns `true` if the dirty latch is set for `rootPath`. */
  isDirty(rootPath: string): boolean {
    return this.dirtyLatches.get(rootPath) ?? false;
  }

  /**
   * Consume the LSP `WorkspaceIndexStatus` aggregate and run `runner` once
   * per source root whose `status === "missing"` (and that survives the
   * optional exclude predicate). Member folders, ok / building / error
   * entries, and excluded paths are silently skipped.
   *
   * The runs are sequential — each `await runner(root)` resolves before
   * the next is invoked — so rebuilds do not contend with each other on
   * the LSP write lock. The method resolves only after every selected
   * runner has resolved (or rejected; see error handling below).
   *
   * Errors thrown by `runner` are propagated as the *first* failure: the
   * outer caller (extension activation) treats a partial enqueue cycle as
   * a soft failure (the extension logs and continues; subsequent saves
   * will retry via the debounced path). The remaining roots are NOT
   * processed once a runner throws — this matches the existing
   * `maybeAutoIndex()` behaviour where `await indexWithProgress(folder)`
   * propagates upward.
   *
   * STEP_5 acceptance criterion 3 (codex iter1 MAJOR fix): the source-
   * root filtering logic lives here, NOT in `extension.ts`. The DAG
   * acceptance for STEP_5 says “autoIndex.ts only enqueues source roots
   * returned by getWorkspaceStatus” — that contract belongs to this
   * method.
   */
  async enqueueFromWorkspaceStatus(
    status: SqryWorkspaceStatus,
    runner: SourceRootRunner,
    options: EnqueueFromStatusOptions = {},
  ): Promise<EnqueueFromStatusResult> {
    let enqueued = 0;
    let excluded = 0;
    let nonMissing = 0;
    const total = status.source_root_statuses.length;
    for (const root of status.source_root_statuses) {
      if (root.status !== "missing") {
        nonMissing += 1;
        continue;
      }
      if (options.exclude && options.exclude(root.path)) {
        excluded += 1;
        continue;
      }
      enqueued += 1;
      // eslint-disable-next-line no-await-in-loop -- sequential by design (see jsdoc above).
      await runner(root);
    }
    return { inspected: total, enqueued, excluded, nonMissing };
  }

  /** Cancel all pending timers and reset all state. */
  dispose(): void {
    for (const id of this.timers.values()) {
      clearTimeout(id);
    }
    this.timers.clear();
    this.dirtyLatches.clear();
    this.buildingRoots.clear();
  }
}
