import {
  EnvironmentId,
  ProjectId,
  ThreadId,
  type DesktopAppActivationRequest,
} from "@onicode/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

import {
  createDesktopAppActivationReceiver,
  handleDesktopAppActivationRequest,
  resolveLocalWorkspaceState,
  type DesktopAppActivationDependencies,
} from "./desktopAppActivation";

const environmentId = EnvironmentId.make("primary");
const existingProjectId = ProjectId.make("project-existing");
const createdProjectId = ProjectId.make("project-created");
const threadId = ThreadId.make("thread-1");
const request = {
  version: 1,
  requestId: "request-1",
  type: "open-workspace",
  workspaceRoot: "/workspace/project",
  platform: "linux",
} as const;

function dependencies(
  overrides: Partial<DesktopAppActivationDependencies> = {},
): DesktopAppActivationDependencies {
  return {
    getTarget: () => ({ environmentId, platform: "linux" }),
    findProject: () => ({
      id: existingProjectId,
      environmentId,
      workspaceRoot: request.workspaceRoot,
    }),
    createProject: vi.fn(async () => createdProjectId),
    waitForProject: vi.fn(async () => undefined),
    openThread: vi.fn(async () => ({ threadId })),
    ...overrides,
  };
}

describe("desktop app activation", () => {
  it("waits for a created project before it opens the thread", async () => {
    const order: string[] = [];
    const deps = dependencies({
      findProject: () => null,
      createProject: vi.fn(async () => {
        order.push("create");
        return createdProjectId;
      }),
      waitForProject: vi.fn(async () => {
        order.push("project-event");
      }),
      openThread: vi.fn(async () => {
        order.push("open-thread");
        return { threadId };
      }),
    });

    const response = await handleDesktopAppActivationRequest(request, deps);

    expect(order).toEqual(["create", "project-event", "open-thread"]);
    expect(response).toMatchObject({ ok: true, projectId: createdProjectId });
  });

  it("rejects a Windows path when the primary environment is WSL", async () => {
    const response = await handleDesktopAppActivationRequest(
      { ...request, platform: "win32" },
      dependencies({ getTarget: () => ({ environmentId, platform: "linux" }) }),
    );

    expect(response).toMatchObject({ ok: false, code: "platform-mismatch" });
  });

  it.each([
    {
      name: "waits while the project list is still the disk cache",
      environment: "connected",
      shellStatus: "cached",
      expected: "pending",
    },
    {
      name: "waits while the project list catches up after a reconnect",
      environment: "connected",
      shellStatus: "synchronizing",
      expected: "pending",
    },
    {
      name: "opens once the project list is live",
      environment: "connected",
      shellStatus: "live",
      expected: "ready",
    },
    {
      name: "waits for this computer's environment to connect",
      environment: "connecting",
      shellStatus: "cached",
      expected: "pending",
    },
    {
      name: "answers at once when the connection to this computer's service is turned off",
      environment: "available",
      enabled: false,
      expected: "unavailable",
    },
    {
      name: "answers at once when this computer's service cannot be used",
      environment: "unsupported",
      expected: "unavailable",
    },
    {
      name: "waits for the Local environment over a saved connection that is turned off",
      // Turning a connection off publishes before it disconnects.
      environment: "connected",
      enabled: false,
      localEnvironmentOff: false,
      expected: "pending",
    },
    {
      name: "answers at once when the Local environment's own server cannot be used",
      environment: "unsupported",
      primary: true,
      localEnvironmentOff: false,
      expected: "unavailable",
    },
    {
      name: "waits until it is known whether this computer runs a service",
      environment: null,
      discoverySettled: false,
      expected: "pending",
    },
    {
      name: "answers at once when Local is off and this computer runs no service",
      environment: null,
      expected: "unavailable",
    },
    {
      name: "waits for the Local environment, which joins the connections once its server answers",
      environment: null,
      localEnvironmentOff: false,
      expected: "pending",
    },
  ] as const)(
    "$name",
    ({
      environment,
      enabled = true,
      primary = false,
      shellStatus = "live",
      discoverySettled = true,
      localEnvironmentOff = true,
      expected,
    }) => {
      expect(
        resolveLocalWorkspaceState({
          environmentId: environment === null ? null : environmentId,
          environment:
            environment === null
              ? null
              : {
                  entry: {
                    enabled,
                    target: { _tag: primary ? "PrimaryConnectionTarget" : "SshConnectionTarget" },
                  } as never,
                  connection: { phase: environment, error: null, traceId: null },
                  serverConfig: {} as never,
                },
          shellStatus: environment === null ? null : shellStatus,
          discoverySettled,
          localEnvironmentOff,
        }),
      ).toBe(expected);
    },
  );

  it.each([
    {
      name: "handles a request that arrived during a remount once the handler is back",
      waitedMs: 0,
    },
    {
      name: "drops a request the desktop app already gave up on before a handler came back",
      waitedMs: 1_001,
    },
  ])("$name", async ({ waitedMs }) => {
    let nowMs = 0;
    let deliver: (input: DesktopAppActivationRequest, deadlineAt: number) => void = () => undefined;
    const receiver = createDesktopAppActivationReceiver(
      {
        onRequest: (listener) => {
          deliver = listener;
          return () => undefined;
        },
        complete: async () => undefined,
      },
      () => nowMs,
    );
    const respond = async (input: DesktopAppActivationRequest) => ({
      version: 1 as const,
      requestId: input.requestId,
      ok: true as const,
      projectId: existingProjectId,
      threadId,
    });
    const unmounted = vi.fn(respond);
    const remounted = vi.fn(respond);
    let handled: () => void = () => undefined;
    const handledOrDropped = new Promise<void>((resolve) => {
      handled = resolve;
    });

    receiver.setHandler(unmounted);
    receiver.setHandler(null);
    deliver(request, 1_000);
    nowMs += waitedMs;
    receiver.setHandler(async (input) => {
      const response = await remounted(input);
      if (input.requestId === "request-2") handled();
      return response;
    });
    // A dropped request never reaches the handler, so a second request marks the queue's progress.
    deliver({ ...request, requestId: "request-2" }, nowMs + 1_000);
    await handledOrDropped;

    expect(unmounted).not.toHaveBeenCalled();
    expect(remounted.mock.calls.map(([input]) => input.requestId)).toEqual(
      waitedMs === 0 ? ["request-1", "request-2"] : ["request-2"],
    );
  });

  it("answers a request whose handler throws instead of leaving the command waiting", async () => {
    let completed: (response: unknown) => void = () => undefined;
    const completion = new Promise((resolve) => {
      completed = resolve;
    });
    const receiver = createDesktopAppActivationReceiver(
      {
        onRequest: (listener) => {
          listener(request, Number.POSITIVE_INFINITY);
          return () => undefined;
        },
        complete: async (response) => completed(response),
      },
      () => 0,
    );

    receiver.setHandler(async () => {
      throw new Error("boom");
    });

    await expect(completion).resolves.toMatchObject({
      requestId: request.requestId,
      ok: false,
      code: "internal-error",
    });
  });
});
