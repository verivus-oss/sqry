import * as vscode from "vscode";

/** VS Code context key that represents safe access to post-startup commands. */
export const SQRY_READY_CONTEXT = "sqry.ready";

/**
 * Every public manifest command whose implementation is unavailable until
 * startup has passed its aggregate workspace-status gate. `sqry.showOutput`
 * is intentionally excluded: it is the always-safe failure/log surface.
 */
export const STARTUP_GATED_COMMAND_IDS = [
  "sqry.query",
  "sqry.index",
  "sqry.searchWorkspace",
  "sqry.findReferences",
  "sqry.refreshStats",
  "sqry.clearResults",
  "sqry.restartLsp",
  "sqry.rebuildIndex",
  "sqry.searchHistory",
  "sqry.scanWorkspace",
  "sqry.showCallGraph",
  "sqry.showDependencies",
  "sqry.filterResults",
  "sqry.sortResults",
  "sqry.exportResults",
  "sqry.editWorkspaceClassification",
] as const;

export type StartupGatedCommandId = (typeof STARTUP_GATED_COMMAND_IDS)[number];
export type StartupCommandHandler = (...args: unknown[]) => unknown | Promise<unknown>;

/** Result returned by the aggregate workspace-status refresh boundary. */
export type WorkspaceStatusRefreshResult =
  | { readonly ok: true }
  | { readonly ok: false; readonly error: Error };

/** Minimal loading-state surface needed by safe bootstrap dispatch. */
export interface StartupLoadingState {
  readonly failure: { readonly reason: string } | null;
  isReady(): boolean;
  isFailed(): boolean;
}

/** Dependencies read by permanent public-command dispatchers. */
export interface StartupCommandDependencies {
  readonly getLoadingState: () => StartupLoadingState | undefined;
  readonly getOutputChannel: () => vscode.OutputChannel | undefined;
}

/** Phase-emitter surface used to bind the VS Code readiness context key. */
export interface ReadinessPhaseSource {
  onDidChangePhase(listener: (phase: string) => void): vscode.Disposable;
}

/** Outcome of an ordered `setContext` call. */
export type ContextWriteResult =
  | { readonly ok: true }
  | { readonly ok: false; readonly error: Error };

/** Thrown when startup cannot establish its mandatory initial false gate. */
export class ReadinessContextInitializationError extends Error {
  constructor(cause: Error) {
    super(`sqry could not establish command readiness: ${cause.message}`);
    this.name = "ReadinessContextInitializationError";
  }
}

/**
 * Activation-owned readiness context binding. `close` appends the terminal
 * false value to the same ordered queue as phase updates.
 */
export interface ReadinessContextBinding extends vscode.Disposable {
  close(): Promise<ContextWriteResult>;
}

/** Permanent public-command dispatch surface owned by extension activation. */
export interface StartupCommandRegistry extends vscode.Disposable {
  registerHandler(
    command: StartupGatedCommandId,
    handler: StartupCommandHandler,
  ): vscode.Disposable;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function contextWriteError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

function unavailableCommandMessage(
  command: StartupGatedCommandId,
  loadingState: StartupLoadingState | undefined,
): string {
  if (loadingState?.isFailed()) {
    const reason = loadingState.failure?.reason ?? "sqry startup failed";
    return `sqry is unavailable: ${reason}. Select View Logs for details, then reload the VS Code window after resolving the cause.`;
  }

  return `sqry is still starting. ${command} will be available after workspace resolution completes. Select View Logs for startup details.`;
}

function showStartupWarning(
  message: string,
  outputChannel: vscode.OutputChannel | undefined,
): void {
  void vscode.window.showWarningMessage(message, "View Logs").then((selection) => {
    if (selection === "View Logs") {
      outputChannel?.show(true);
    }
  });
}

async function writeOneContextValue(
  isReady: boolean,
  outputChannel: vscode.OutputChannel | undefined,
): Promise<ContextWriteResult> {
  try {
    await vscode.commands.executeCommand("setContext", SQRY_READY_CONTEXT, isReady);
    return { ok: true };
  } catch (error) {
    const normalizedError = contextWriteError(error);
    outputChannel?.appendLine(
      `[sqry] Failed to update command readiness context: ${errorMessage(normalizedError)}`,
    );
    return { ok: false, error: normalizedError };
  }
}

class OrderedReadinessContextBinding implements ReadinessContextBinding {
  private tail: Promise<ContextWriteResult> = Promise.resolve({ ok: true });
  private phaseDisposable: vscode.Disposable | undefined;
  private closePromise: Promise<ContextWriteResult> | undefined;
  private initialFailure: ContextWriteResult | undefined;
  private isClosed = false;

  constructor(
    private readonly phaseSource: ReadinessPhaseSource,
    private readonly outputChannel: vscode.OutputChannel | undefined,
    private readonly onRuntimeWriteFailure: (error: Error) => void,
  ) {}

  public async initialize(): Promise<ContextWriteResult> {
    const initialResult = await this.enqueue(false);
    if (!initialResult.ok) {
      this.initialFailure = initialResult;
      this.isClosed = true;
      return initialResult;
    }

    this.phaseDisposable = this.phaseSource.onDidChangePhase((phase) => {
      if (this.isClosed) {
        return;
      }
      void this.enqueue(phase === "Ready").then((result) => {
        if (!result.ok) {
          this.onRuntimeWriteFailure(result.error);
        }
      });
    });
    return initialResult;
  }

  public close(): Promise<ContextWriteResult> {
    if (this.closePromise) {
      return this.closePromise;
    }
    if (this.initialFailure && !this.initialFailure.ok) {
      return Promise.resolve(this.initialFailure);
    }

    this.isClosed = true;
    this.phaseDisposable?.dispose();
    this.phaseDisposable = undefined;
    this.closePromise = this.enqueue(false).then((result) => {
      if (!result.ok) {
        this.onRuntimeWriteFailure(result.error);
      }
      return result;
    });
    return this.closePromise;
  }

  public dispose(): void {
    void this.close();
  }

  private enqueue(isReady: boolean): Promise<ContextWriteResult> {
    this.tail = this.tail.then(() => writeOneContextValue(isReady, this.outputChannel));
    return this.tail;
  }
}

/**
 * Establish an initial false context value before startup advances, then keep
 * phase-derived writes serialized. A failed initial reset is terminal: no
 * phase listener is installed and no later true value can be issued.
 */
export async function bindSqryReadyContext(
  phaseSource: ReadinessPhaseSource,
  outputChannel: vscode.OutputChannel | undefined,
  onRuntimeWriteFailure: (error: Error) => void,
): Promise<ReadinessContextBinding> {
  const binding = new OrderedReadinessContextBinding(
    phaseSource,
    outputChannel,
    onRuntimeWriteFailure,
  );
  const initialResult = await binding.initialize();
  if (!initialResult.ok) {
    throw new ReadinessContextInitializationError(initialResult.error);
  }
  return binding;
}

class StartupCommandRegistryImpl implements StartupCommandRegistry {
  private readonly handlers = new Map<StartupGatedCommandId, StartupCommandHandler>();
  private readonly registrations: vscode.Disposable[];
  private isDisposed = false;

  constructor(private readonly dependencies: StartupCommandDependencies) {
    this.registrations = STARTUP_GATED_COMMAND_IDS.map((command) =>
      vscode.commands.registerCommand(command, async (...args: unknown[]) => {
        const loadingState = this.dependencies.getLoadingState();
        const outputChannel = this.dependencies.getOutputChannel();
        if (!loadingState?.isReady()) {
          const message = unavailableCommandMessage(command, loadingState);
          outputChannel?.appendLine(`[sqry] ${command} unavailable: ${message}`);
          showStartupWarning(message, outputChannel);
          return;
        }

        const handler = this.handlers.get(command);
        if (!handler) {
          const message = `sqry is not ready to run ${command}. Select View Logs for details, then reload the VS Code window.`;
          outputChannel?.appendLine(`[sqry] ${message}`);
          showStartupWarning(message, outputChannel);
          return;
        }

        await handler(...args);
      }),
    );
  }

  public registerHandler(
    command: StartupGatedCommandId,
    handler: StartupCommandHandler,
  ): vscode.Disposable {
    if (this.isDisposed) {
      throw new Error(`Cannot register ${command}: startup command registry is disposed.`);
    }
    if (this.handlers.has(command)) {
      throw new Error(`Startup command handler is already registered for ${command}.`);
    }
    this.handlers.set(command, handler);
    return {
      dispose: () => {
        if (this.handlers.get(command) === handler) {
          this.handlers.delete(command);
        }
      },
    };
  }

  public dispose(): void {
    if (this.isDisposed) {
      return;
    }
    this.isDisposed = true;
    this.handlers.clear();
    for (const registration of this.registrations) {
      registration.dispose();
    }
  }
}

/**
 * Register permanent, phase-safe dispatchers for every public command before
 * any binary download or LSP start can await. Real handlers are attached by
 * activation once their dependencies exist.
 */
export function registerStartupCommands(
  dependencies: StartupCommandDependencies,
): StartupCommandRegistry {
  return new StartupCommandRegistryImpl(dependencies);
}
