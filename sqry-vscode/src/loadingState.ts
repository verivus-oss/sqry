/**
 * Loading state machine for the sqry VS Code extension.
 *
 * The 5-phase contract (DAG STEP_5, codex iter1 MAJOR fix):
 *
 *   Activating -> LspStarting -> WorkspaceResolving -> Ready
 *                                                       \
 *                                                        +-> Failed (terminal)
 *
 * - During `Activating`, `LspStarting`, and `WorkspaceResolving` the
 *   status bar shows `sqry.statusBar.resolving` ("sqry: resolving
 *   workspace…") and the tree view shows a single skeleton row. The
 *   extension MUST NOT fall back to per-folder filesystem stat probes
 *   during these phases (DAG STEP_5 acceptance criterion 4).
 * - `Ready` is reached only after the LSP has answered a successful
 *   `getWorkspaceStatus()` call. A single aggregate request is queued at
 *   `Ready` and re-queued on `onDidChangeWorkspaceFolders`,
 *   `onDidChangeConfiguration("sqry")`, and the `sqry.refreshStats`
 *   command.
 * - Manual rebuild commands gate on `Ready`. If the extension has not
 *   reached `Ready` within `MANUAL_GATE_TIMEOUT_MS` the gate rejects
 *   with a timeout error.
 * - `Failed` is terminal: the status bar shows `sqry.statusBar.unavailable`
 *   ("sqry: unavailable") with a `View Logs` action.
 *
 * The state machine is plain in-process state — no VS Code APIs, no
 * filesystem access — so it can be unit tested without an extension
 * host. Transitions are validated against the explicit DAG; invalid
 * transitions throw [`InvalidTransitionError`].
 */

export const MANUAL_GATE_TIMEOUT_MS = 30_000;

export type LoadingPhase =
  | "Activating"
  | "LspStarting"
  | "WorkspaceResolving"
  | "Ready"
  | "Failed";

export interface FailedDetails {
  /** Human-readable reason surfaced to the user via the status-bar tooltip. */
  readonly reason: string;
  /**
   * When the extension entered `Failed` because the LSP could not start
   * we surface a `View Logs` action. Other failure modes (e.g. workspace
   * resolve failed but LSP is alive) keep `viewLogsAction = false`.
   */
  readonly viewLogsAction: boolean;
}

/** Per-phase listener payload. */
export type PhaseListener = (phase: LoadingPhase, failed?: FailedDetails) => void;

export class InvalidTransitionError extends Error {
  constructor(from: LoadingPhase, to: LoadingPhase) {
    super(`sqry loading state: invalid transition ${from} -> ${to}`);
    this.name = "InvalidTransitionError";
  }
}

/**
 * Validate the DAG.  The forward edges are the only legal transitions.
 * `Failed` is a sink reachable from every non-terminal phase.
 */
const ALLOWED_TRANSITIONS: Readonly<Record<LoadingPhase, ReadonlyArray<LoadingPhase>>> = {
  Activating: ["LspStarting", "Failed"],
  LspStarting: ["WorkspaceResolving", "Failed"],
  WorkspaceResolving: ["Ready", "Failed"],
  Ready: ["WorkspaceResolving", "Failed"],
  Failed: [],
};

export class LoadingStateMachine {
  private currentPhase: LoadingPhase = "Activating";
  private failedDetails: FailedDetails | null = null;
  private readonly listeners = new Set<PhaseListener>();
  private readonly readyWaiters: Array<{
    resolve: () => void;
    reject: (err: Error) => void;
    timer: ReturnType<typeof setTimeout>;
  }> = [];

  public get phase(): LoadingPhase {
    return this.currentPhase;
  }

  public get failure(): FailedDetails | null {
    return this.failedDetails;
  }

  public isReady(): boolean {
    return this.currentPhase === "Ready";
  }

  public isLoading(): boolean {
    return (
      this.currentPhase === "Activating" ||
      this.currentPhase === "LspStarting" ||
      this.currentPhase === "WorkspaceResolving"
    );
  }

  public isFailed(): boolean {
    return this.currentPhase === "Failed";
  }

  /**
   * Subscribe to phase transitions. Returns a disposable; the listener
   * is invoked synchronously with the new phase on every transition.
   */
  public onDidChangePhase(listener: PhaseListener): { dispose(): void } {
    this.listeners.add(listener);
    return {
      dispose: () => {
        this.listeners.delete(listener);
      },
    };
  }

  public transition(to: LoadingPhase, failure?: FailedDetails): void {
    const allowed = ALLOWED_TRANSITIONS[this.currentPhase];
    if (!allowed.includes(to)) {
      throw new InvalidTransitionError(this.currentPhase, to);
    }
    this.currentPhase = to;
    if (to === "Failed") {
      this.failedDetails = failure ?? { reason: "sqry initialization failed", viewLogsAction: true };
      this.rejectAllWaiters(new Error(this.failedDetails.reason));
    } else {
      this.failedDetails = null;
      if (to === "Ready") {
        this.resolveAllWaiters();
      }
    }
    for (const listener of this.listeners) {
      try {
        listener(to, this.failedDetails ?? undefined);
      } catch {
        // Listener errors must not break the state machine.
      }
    }
  }

  /**
   * Wait for the state machine to reach `Ready`. Returns immediately
   * when already at `Ready`; rejects on `Failed`; otherwise resolves
   * when the next `Ready` transition occurs or rejects after
   * `timeoutMs` (default 30s — DAG manual-gate contract).
   */
  public waitForReady(timeoutMs: number = MANUAL_GATE_TIMEOUT_MS): Promise<void> {
    if (this.currentPhase === "Ready") {
      return Promise.resolve();
    }
    if (this.currentPhase === "Failed") {
      return Promise.reject(
        new Error(this.failedDetails?.reason ?? "sqry initialization failed"),
      );
    }
    return new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => {
        const idx = this.readyWaiters.findIndex((w) => w.timer === timer);
        if (idx >= 0) {
          this.readyWaiters.splice(idx, 1);
        }
        reject(new Error(`sqry: workspace resolution timed out after ${timeoutMs}ms`));
      }, timeoutMs);
      this.readyWaiters.push({ resolve, reject, timer });
    });
  }

  public dispose(): void {
    this.rejectAllWaiters(new Error("sqry: extension deactivated"));
    this.listeners.clear();
  }

  private resolveAllWaiters(): void {
    const waiters = this.readyWaiters.splice(0);
    for (const w of waiters) {
      clearTimeout(w.timer);
      w.resolve();
    }
  }

  private rejectAllWaiters(err: Error): void {
    const waiters = this.readyWaiters.splice(0);
    for (const w of waiters) {
      clearTimeout(w.timer);
      w.reject(err);
    }
  }
}
