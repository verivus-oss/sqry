/**
 * AutoIndexManager — manages debounced per-root index rebuilds triggered by file saves.
 *
 * Responsibilities:
 *   - Debounce: rapid save events for the same root reset the timer; only the last fires.
 *   - Dirty latch: when a save arrives while a build is in progress, the latch is set so a
 *     follow-up rebuild is scheduled immediately after the current build completes.
 *   - Per-root isolation: each workspace root has its own timer, latch, and build flag.
 */
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
