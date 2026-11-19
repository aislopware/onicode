import type { EnvironmentPresentation } from "@onicode/client-runtime/connection";
import type { EnvironmentShellStatus } from "@onicode/client-runtime/state/shell";
import type {
  DesktopAppActivationFailure,
  DesktopBridge,
  DesktopAppActivationRequest,
  DesktopAppActivationResponse,
  EnvironmentId,
  ExecutionEnvironmentPlatformOs,
  ProjectId,
  ScopedProjectRef,
  ThreadId,
} from "@onicode/contracts";

export interface DesktopAppActivationProject {
  readonly id: ProjectId;
  readonly environmentId: EnvironmentId;
  readonly workspaceRoot: string;
}

export interface DesktopAppActivationTarget {
  readonly environmentId: EnvironmentId;
  readonly platform: ExecutionEnvironmentPlatformOs;
}

export interface DesktopAppActivationDependencies {
  readonly getTarget: () => DesktopAppActivationTarget | null;
  readonly findProject: (
    environmentId: EnvironmentId,
    workspaceRoot: string,
  ) => DesktopAppActivationProject | null;
  readonly createProject: (
    environmentId: EnvironmentId,
    workspaceRoot: string,
  ) => Promise<ProjectId>;
  readonly waitForProject: (projectRef: ScopedProjectRef) => Promise<void>;
  readonly openThread: (
    projectRef: ScopedProjectRef,
    signal: AbortSignal,
  ) => Promise<{ readonly threadId: ThreadId } | null>;
}

/**
 * `pending` holds requests until this computer's environment can take them. A project list that is
 * not live waits too: it can miss a project added elsewhere, and adding that folder would conflict.
 */
export function resolveLocalWorkspaceState(input: {
  readonly environmentId: EnvironmentId | null;
  readonly environment: Pick<
    EnvironmentPresentation,
    "entry" | "connection" | "serverConfig"
  > | null;
  readonly shellStatus: EnvironmentShellStatus | null;
  /** The connections and this computer's service ID are known. */
  readonly discoverySettled: boolean;
  /** With Local on, the primary joins the connections only once its server answers. */
  readonly localEnvironmentOff: boolean;
}): "ready" | "pending" | "unavailable" {
  if (input.environmentId === null) {
    return input.localEnvironmentOff && input.discoverySettled ? "unavailable" : "pending";
  }
  // A blocked connection retries when the window activates, which `onicode app` does; these never
  // connect. With Local on, a saved connection only stands in until the primary registers.
  if (
    input.environment?.entry.enabled === false ||
    input.environment?.connection.phase === "unsupported"
  ) {
    return input.localEnvironmentOff ||
      input.environment.entry.target._tag === "PrimaryConnectionTarget"
      ? "unavailable"
      : "pending";
  }
  return input.environment?.connection.phase === "connected" &&
    input.environment.serverConfig !== null &&
    input.shellStatus === "live"
    ? "ready"
    : "pending";
}

function failure(
  requestId: string,
  code: DesktopAppActivationFailure["code"],
  message: string,
): DesktopAppActivationFailure {
  return { version: 1, requestId, ok: false, code, message };
}

function desktopPlatformToEnvironmentOs(
  platform: DesktopAppActivationRequest["platform"],
): ExecutionEnvironmentPlatformOs {
  return platform === "win32" ? "windows" : platform;
}

function errorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message.trim().length > 0 ? error.message : fallback;
}

let nextOpenOrder = 0;
const runningOpens = new Set<{ readonly order: number; readonly abort: AbortController }>();

/** Opens ordered by one call (the folders of a drop) rank by that moment, not their own start. */
export function reserveDesktopAppActivationOrder(): number {
  return nextOpenOrder++;
}

/**
 * The newest open that reaches its thread wins: older ones still running stop before they show
 * theirs and resolve to null, so a drop stops instead of opening its next folder over the winner.
 */
export async function handleDesktopAppActivationRequest(
  request: DesktopAppActivationRequest,
  dependencies: DesktopAppActivationDependencies,
  order = reserveDesktopAppActivationOrder(),
): Promise<DesktopAppActivationResponse | null> {
  const open = { order, abort: new AbortController() };
  runningOpens.add(open);
  try {
    const response = await openWorkspace(request, dependencies, order, open.abort.signal);
    return open.abort.signal.aborted ? null : response;
  } finally {
    runningOpens.delete(open);
  }
}

async function openWorkspace(
  request: DesktopAppActivationRequest,
  dependencies: DesktopAppActivationDependencies,
  order: number,
  signal: AbortSignal,
): Promise<DesktopAppActivationResponse | null> {
  const target = dependencies.getTarget();
  if (target === null) {
    return failure(
      request.requestId,
      "environment-unavailable",
      "This computer's Onicode server is not connected.",
    );
  }

  const requestPlatform = desktopPlatformToEnvironmentOs(request.platform);
  if (requestPlatform !== target.platform) {
    return failure(
      request.requestId,
      "platform-mismatch",
      `The command path is for ${requestPlatform}, but this computer's Onicode server uses ${target.platform}. Cross-platform path mapping is not supported.`,
    );
  }

  let projectId = dependencies.findProject(target.environmentId, request.workspaceRoot)?.id ?? null;
  if (projectId === null) {
    try {
      projectId = await dependencies.createProject(target.environmentId, request.workspaceRoot);
      await dependencies.waitForProject({ environmentId: target.environmentId, projectId });
    } catch (error) {
      return failure(
        request.requestId,
        "project-create-failed",
        errorMessage(error, "Onicode could not add the project."),
      );
    }
  }

  for (const other of runningOpens) {
    if (other.order < order) other.abort.abort();
  }
  if (signal.aborted) return null;
  try {
    const opened = await dependencies.openThread(
      { environmentId: target.environmentId, projectId },
      signal,
    );
    if (opened === null) {
      return failure(
        request.requestId,
        "thread-open-failed",
        "Onicode could not open a new thread for the project.",
      );
    }
    return {
      version: 1,
      requestId: request.requestId,
      ok: true,
      projectId,
      threadId: opened.threadId,
    };
  } catch (error) {
    return failure(
      request.requestId,
      "thread-open-failed",
      errorMessage(error, "Onicode could not open a new thread for the project."),
    );
  }
}

type DesktopAppActivationHandler = (
  request: DesktopAppActivationRequest,
) => Promise<DesktopAppActivationResponse | null>;

/**
 * Listens for the page's whole life, so a request sent while the handler remounts (leaving
 * /welcome) waits for it instead of reaching no listener.
 */
export function createDesktopAppActivationReceiver(
  activation: Pick<DesktopBridge["appActivation"], "onRequest" | "claim" | "complete">,
) {
  let handler: DesktopAppActivationHandler | null = null;
  const waiters = new Set<() => void>();
  const awaitHandler = async () => {
    while (handler === null) {
      await new Promise<void>((resolve) => waiters.add(resolve));
    }
    return handler;
  };
  activation.onRequest((request) => {
    void (async () => {
      let handle: DesktopAppActivationHandler | null = null;
      // A handler that left during the claim may come back after the command timed out.
      while (handle === null) {
        await awaitHandler();
        // The command already has its answer; opening the folder now would surprise the user.
        if (!(await activation.claim(request.requestId))) return;
        handle = handler;
      }
      let response: DesktopAppActivationResponse;
      try {
        response =
          (await handle(request)) ??
          failure(request.requestId, "thread-open-failed", "A newer open replaced this one.");
      } catch (error) {
        response = failure(
          request.requestId,
          "internal-error",
          errorMessage(error, "Onicode could not open the project."),
        );
      }
      await activation.complete(response);
    })().catch(() => undefined);
  });
  return {
    setHandler: (next: DesktopAppActivationHandler | null) => {
      handler = next;
      if (next === null) return;
      for (const wake of waiters) wake();
      waiters.clear();
    },
  };
}

let receiver: ReturnType<typeof createDesktopAppActivationReceiver> | null = null;

export function startDesktopAppActivationReceiver(): void {
  const activation = window.desktopBridge?.appActivation;
  if (activation !== undefined && receiver === null) {
    receiver = createDesktopAppActivationReceiver(activation);
  }
}

export function setDesktopAppActivationHandler(handler: DesktopAppActivationHandler | null): void {
  receiver?.setHandler(handler);
}
