import type { SqryClient } from "./sqryClient";
import type { ReadinessContextBinding, WorkspaceStatusRefreshResult } from "./startupCommands";

/** Minimal state-machine surface required by initial workspace resolution. */
export interface StartupResolutionState {
  isFailed(): boolean;
  transition(
    phase: "Ready" | "Failed",
    failure?: { readonly reason: string; readonly viewLogsAction: boolean },
  ): void;
}

/** Dependencies for the bounded startup-resolution critical path. */
export interface StartupResolutionDependencies {
  readonly activeClient: SqryClient;
  readonly loadingState: StartupResolutionState;
  readonly refreshWorkspaceStatus: (
    activeClient: SqryClient,
  ) => Promise<WorkspaceStatusRefreshResult>;
  readonly emitTelemetry: () => Promise<void>;
  readonly maybeAutoIndex: () => Promise<void>;
  readonly log: (line: string) => void;
}

/** Resources that must be ordered safely during extension deactivation. */
export interface StartupDeactivationDependencies {
  /** Client disposal cancels every pending LSP candidate synchronously. */
  readonly activeClient: Pick<SqryClient, "dispose"> | undefined;
  /** Context reset is user-interface cleanup and may wait on the host. */
  readonly readinessContextBinding: ReadinessContextBinding | undefined;
}

/**
 * Dispose a client that is leaving the activation lifecycle.
 *
 * This is deliberately synchronous: `SqryClient.dispose` cancels pending
 * startup owners and unregisters the configuration listener before any
 * terminal-failure path returns to the VS Code host.
 */
export function disposeStartupClient(
  activeClient: Pick<SqryClient, "dispose"> | undefined,
): void {
  activeClient?.dispose();
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function transitionToFailed(
  loadingState: StartupResolutionState,
  reason: string,
): void {
  if (!loadingState.isFailed()) {
    loadingState.transition("Failed", { reason, viewLogsAction: true });
  }
}

/**
 * Stop all language-client work before awaiting readiness-context cleanup.
 *
 * `setContext` is host work, not a lifecycle owner. If it stalls during
 * deactivation, a pending client start must already have been cancelled.
 */
export async function deactivateStartupResources(
  dependencies: StartupDeactivationDependencies,
): Promise<void> {
  disposeStartupClient(dependencies.activeClient);
  await dependencies.readinessContextBinding?.close();
}

/**
 * Complete the only readiness-critical workspace round trip.
 *
 * Telemetry intentionally runs after `Ready` and is scheduled separately, so
 * a stuck or rejected telemetry RPC cannot keep the extension in its loading
 * state. The returned boolean lets the activation wiring stop cleanly after a
 * visible terminal failure instead of throwing out of activation.
 */
export async function completeInitialWorkspaceResolution(
  dependencies: StartupResolutionDependencies,
): Promise<boolean> {
  try {
    const status = await dependencies.refreshWorkspaceStatus(dependencies.activeClient);
    if (!status.ok) {
      transitionToFailed(
        dependencies.loadingState,
        `sqry workspace resolution failed: ${status.error.message}`,
      );
      return false;
    }

    dependencies.loadingState.transition("Ready");
    void Promise.resolve()
      .then(dependencies.emitTelemetry)
      .catch((error: unknown) => {
        dependencies.log(`[sqry] Failed to emit workspace telemetry: ${errorMessage(error)}`);
      });

    await dependencies.maybeAutoIndex();
    return true;
  } catch (error) {
    const message = errorMessage(error);
    dependencies.log(`[sqry] sqry startup did not complete: ${message}`);
    transitionToFailed(
      dependencies.loadingState,
      `sqry startup did not complete: ${message}`,
    );
    return false;
  }
}
