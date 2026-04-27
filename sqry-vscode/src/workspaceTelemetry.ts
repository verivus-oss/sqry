/**
 * STEP_12 telemetry — pure workspace-resolution log-line formatter.
 *
 * Lives in its own module (no `vscode` import) so it can be unit-tested
 * without booting the extension host or proxying every transitive
 * import in `extension.ts`. The activation path imports it via
 * `extension.ts` and emits the line ONCE at startup; tests pin the
 * exact wire format here.
 *
 * Format (DAG STEP_12 verbatim):
 *
 *   `[sqry] Resolved workspace <workspace_id_short> with N source roots, M members, K exclusions`
 *
 * Plural nouns are hard-coded so machine consumers can rely on a fixed
 * shape. The short hex (16 chars) is for human eyes; the full hex digest
 * stays reachable through `sqry/workspaceStatus`'s `workspace_id_full`
 * field for cross-process identity comparisons.
 */

export interface WorkspaceResolutionTelemetryArgs {
  /**
   * First 16 hex chars of the BLAKE3 digest. Display only.
   * Cross-process script consumers should key on the full hex digest
   * instead — see `SqryLogicalWorkspaceInfo.workspace_id_full`.
   */
  readonly workspaceIdShort: string;
  /** Number of source roots in the resolved workspace. */
  readonly sourceRootCount: number;
  /** Number of member folders in the resolved workspace. */
  readonly memberCount: number;
  /** Number of exclusions in the resolved workspace. */
  readonly exclusionCount: number;
}

/**
 * Build the verbatim STEP_12 startup line. Pure — no I/O, no `vscode`.
 */
export function formatWorkspaceResolutionTelemetry(
  args: WorkspaceResolutionTelemetryArgs,
): string {
  return (
    `[sqry] Resolved workspace ${args.workspaceIdShort} with ` +
    `${args.sourceRootCount} source roots, ` +
    `${args.memberCount} members, ` +
    `${args.exclusionCount} exclusions`
  );
}

/**
 * Minimal contract for the workspace info supplier — typically a
 * `SqryClient.getLogicalWorkspaceInfo` bound method. Decoupled from
 * `SqryClient` so tests can drive `emitWorkspaceResolutionTelemetry`
 * with a recording fake without instantiating the language client.
 */
export interface WorkspaceInfoSupplier {
  getLogicalWorkspaceInfo(): Promise<{
    readonly workspace_id_short: string;
    readonly source_roots: ReadonlyArray<unknown>;
    readonly member_folders: ReadonlyArray<unknown>;
    readonly exclusions: ReadonlyArray<unknown>;
  }>;
}

/**
 * Minimal contract for the output sink — typically a vscode
 * `OutputChannel`. Only `appendLine` is required; tests pass a
 * recording fake.
 */
export interface OutputSink {
  appendLine(message: string): void;
}

/**
 * STEP_12 — emit the single startup telemetry line.
 *
 * Calls `supplier.getLogicalWorkspaceInfo()` exactly once and writes
 * exactly one line via `sink.appendLine`. On failure, a single
 * best-effort error line is written to `sink.appendLine` instead of
 * propagating; the function never throws so activation cannot be
 * blocked by telemetry.
 *
 * Exported so the activation path in `extension.ts` and the unit-test
 * fakes share a single implementation. Tests pin the call counts
 * (per the DAG's "exactly ONE outputChannel line per startup"
 * criterion) by recording invocations on the fakes.
 */
export async function emitWorkspaceResolutionTelemetry(
  supplier: WorkspaceInfoSupplier,
  sink: OutputSink,
): Promise<void> {
  let info: Awaited<ReturnType<WorkspaceInfoSupplier["getLogicalWorkspaceInfo"]>>;
  try {
    info = await supplier.getLogicalWorkspaceInfo();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    sink.appendLine(
      `[sqry] STEP_12 telemetry: failed to fetch logical workspace info (${message})`,
    );
    return;
  }
  sink.appendLine(
    formatWorkspaceResolutionTelemetry({
      workspaceIdShort: info.workspace_id_short,
      sourceRootCount: info.source_roots.length,
      memberCount: info.member_folders.length,
      exclusionCount: info.exclusions.length,
    }),
  );
}
