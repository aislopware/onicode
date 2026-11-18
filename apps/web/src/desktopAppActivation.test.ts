import {
  EnvironmentId,
  ProjectId,
  ThreadId,
  type DesktopAppActivationRequest,
  type DesktopAppActivationResponse,
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

function success(requestId: string): DesktopAppActivationResponse {
  return { version: 1, requestId, ok: true, projectId: existingProjectId, threadId };
}

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

  function receiverHarness() {
    let deliver: (input: DesktopAppActivationRequest) => void = () => undefined;
    let abandon: (requestId: string) => void = () => undefined;
    const completions: DesktopAppActivationResponse[] = [];
    let onComplete: () => void = () => undefined;
    const receiver = createDesktopAppActivationReceiver({
      onRequest: (listener) => {
        deliver = listener;
        return () => undefined;
      },
      onAbandoned: (listener) => {
        abandon = listener;
        return () => undefined;
      },
      complete: async (response) => {
        completions.push(response);
        onComplete();
      },
    });
    const nextCompletion = () =>
      new Promise<void>((resolve) => {
        onComplete = resolve;
      });
    return {
      receiver,
      deliver: (input: DesktopAppActivationRequest = request) => deliver(input),
      abandon,
      completions,
      nextCompletion,
    };
  }

  it("handles a request that arrived during a remount once the handler is back", async () => {
    const harness = receiverHarness();
    const unmounted = vi.fn(async () => success("request-1"));
    harness.receiver.setHandler(unmounted);
    harness.receiver.setHandler(null);
    const completed = harness.nextCompletion();
    harness.deliver();
    // Lets the queue reach the request while no handler is set.
    await Promise.resolve();
    harness.receiver.setHandler(async (input) => success(input.requestId));
    await completed;

    expect(unmounted).not.toHaveBeenCalled();
    expect(harness.completions).toEqual([success("request-1")]);
  });

  it("drops a request the desktop app gave up on before a handler came back", async () => {
    const harness = receiverHarness();
    const handled = vi.fn(async (input: DesktopAppActivationRequest) => success(input.requestId));
    harness.deliver();
    harness.abandon(request.requestId);
    const completed = harness.nextCompletion();
    harness.receiver.setHandler(handled);
    // A dropped request never reaches the handler, so a second request marks the queue's progress.
    harness.deliver({ ...request, requestId: "request-2" });
    await completed;

    expect(handled.mock.calls.map(([input]) => input.requestId)).toEqual(["request-2"]);
  });

  it("does not open a thread once the desktop app gives up while the project is added", async () => {
    const harness = receiverHarness();
    const deps = dependencies({
      findProject: () => null,
      waitForProject: vi.fn(async () => harness.abandon(request.requestId)),
    });
    const completed = harness.nextCompletion();
    harness.receiver.setHandler((input, signal) =>
      handleDesktopAppActivationRequest(input, deps, signal),
    );
    harness.deliver();
    await completed;

    expect(deps.createProject).toHaveBeenCalledOnce();
    expect(deps.openThread).not.toHaveBeenCalled();
  });

  it.each([
    {
      name: "rejects",
      handler: async (): Promise<DesktopAppActivationResponse> => {
        throw new Error("boom");
      },
    },
    {
      name: "throws before returning a promise",
      handler: (): Promise<DesktopAppActivationResponse> => {
        throw new Error("boom");
      },
    },
  ])(
    "answers a request whose handler $name instead of leaving the command waiting",
    async ({ handler }) => {
      const harness = receiverHarness();
      const completed = harness.nextCompletion();
      harness.receiver.setHandler(handler);
      harness.deliver();
      await completed;

      expect(harness.completions).toEqual([
        expect.objectContaining({
          requestId: request.requestId,
          ok: false,
          code: "internal-error",
        }),
      ]);
    },
  );
});
