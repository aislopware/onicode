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
  reserveDesktopAppActivationOrder,
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

  function receiverHarness(claim: (requestId: string) => boolean | Promise<boolean> = () => true) {
    let deliver: (input: DesktopAppActivationRequest) => void = () => undefined;
    const claims: string[] = [];
    const claimResults: Promise<boolean>[] = [];
    const completions: DesktopAppActivationResponse[] = [];
    let onComplete: () => void = () => undefined;
    const receiver = createDesktopAppActivationReceiver({
      onRequest: (listener) => {
        deliver = listener;
        return () => undefined;
      },
      claim: (requestId) => {
        claims.push(requestId);
        const result = Promise.resolve(claim(requestId));
        claimResults.push(result);
        return result;
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
      claims,
      claimResults,
      completions,
      nextCompletion,
    };
  }

  it("claims a request that arrived during a remount only once the handler is back", async () => {
    const harness = receiverHarness();
    const unmounted = vi.fn(async () => success("request-1"));
    harness.receiver.setHandler(unmounted);
    harness.receiver.setHandler(null);
    const completed = harness.nextCompletion();
    harness.deliver();
    // Claiming now would leave the command waiting on a page with no handler.
    expect(harness.claims).toEqual([]);
    harness.receiver.setHandler(async (input) => success(input.requestId));
    await completed;

    expect(unmounted).not.toHaveBeenCalled();
    expect(harness.completions).toEqual([success("request-1")]);
  });

  it("runs the handler that is current once the claim returns", async () => {
    let grant: (claimed: boolean) => void = () => undefined;
    let claimRequested: () => void = () => undefined;
    const claimed = new Promise<void>((resolve) => {
      claimRequested = resolve;
    });
    const harness = receiverHarness(
      () =>
        new Promise<boolean>((resolve) => {
          grant = resolve;
          claimRequested();
        }),
    );
    const unmounted = vi.fn(async () => success("request-1"));
    const completed = harness.nextCompletion();
    harness.receiver.setHandler(unmounted);
    harness.deliver();
    await claimed;
    harness.receiver.setHandler(null);
    harness.receiver.setHandler(async (input) => success(input.requestId));
    grant(true);
    await completed;

    expect(unmounted).not.toHaveBeenCalled();
    expect(harness.completions).toEqual([success("request-1")]);
  });

  it("skips a request the desktop app already answered", async () => {
    const harness = receiverHarness((requestId) => requestId !== request.requestId);
    const handled = vi.fn(async (input: DesktopAppActivationRequest) => success(input.requestId));
    harness.receiver.setHandler(handled);
    const completed = harness.nextCompletion();
    harness.deliver();
    // A skipped request never completes, so a second request marks progress.
    harness.deliver({ ...request, requestId: "request-2" });
    await completed;

    expect(handled.mock.calls.map(([input]) => input.requestId)).toEqual(["request-2"]);
    expect(harness.completions).toEqual([success("request-2")]);
  });

  it("claims again when the handler stayed away past the claim", async () => {
    let grant: (claimed: boolean) => void = () => undefined;
    let claimRequested: () => void = () => undefined;
    const claimed = new Promise<void>((resolve) => {
      claimRequested = resolve;
    });
    let calls = 0;
    const harness = receiverHarness((requestId) => {
      if (requestId !== request.requestId) return true;
      calls += 1;
      if (calls > 1) return false;
      return new Promise<boolean>((resolve) => {
        grant = resolve;
        claimRequested();
      });
    });
    const unmounted = vi.fn(async () => success("request-1"));
    const remounted = vi.fn(async (input: DesktopAppActivationRequest) => success(input.requestId));
    harness.receiver.setHandler(unmounted);
    harness.deliver();
    await claimed;
    harness.receiver.setHandler(null);
    grant(true);
    // The receiver awaited this claim first, so it sees no handler before the test goes on.
    await harness.claimResults[0];
    const completed = harness.nextCompletion();
    harness.receiver.setHandler(remounted);
    harness.deliver({ ...request, requestId: "request-2" });
    await completed;

    expect(unmounted).not.toHaveBeenCalled();
    expect(remounted.mock.calls.map(([input]) => input.requestId)).toEqual(["request-2"]);
    expect(harness.claims.filter((requestId) => requestId === request.requestId)).toHaveLength(2);
  });

  it("keeps an older open that finishes late from replacing a newer one", async () => {
    let projectAppeared: () => void = () => undefined;
    const deps = dependencies({
      findProject: (_environmentId, workspaceRoot) =>
        workspaceRoot === request.workspaceRoot
          ? null
          : { id: existingProjectId, environmentId, workspaceRoot },
      waitForProject: vi.fn(
        () =>
          new Promise<void>((resolve) => {
            projectAppeared = resolve;
          }),
      ),
    });
    // An activation still adding its project, then a sidebar drop of a known folder.
    const older = handleDesktopAppActivationRequest(request, deps);
    await expect(
      handleDesktopAppActivationRequest(
        { ...request, requestId: "request-2", workspaceRoot: "/workspace/other" },
        deps,
      ),
    ).resolves.toMatchObject({ ok: true });
    projectAppeared();

    // Nobody is left to tell: a drop stops quietly, an activation's command is answered by the receiver.
    await expect(older).resolves.toBeNull();
    expect(vi.mocked(deps.openThread).mock.calls.map(([projectRef]) => projectRef)).toEqual([
      { environmentId, projectId: existingProjectId },
    ]);
  });

  it.each([
    { name: "still shows its thread", outcome: { threadId } },
    { name: "is abandoned by the new-thread handler", outcome: null },
  ])(
    "reports an older open a newer one replaced while it $name as replaced",
    async ({ outcome }) => {
      let finishOpen: () => void = () => undefined;
      const deps = dependencies({
        openThread: vi.fn(async (projectRef) => {
          if (projectRef.projectId !== existingProjectId) return { threadId };
          await new Promise<void>((resolve) => {
            finishOpen = resolve;
          });
          return outcome;
        }),
        findProject: (_environmentId, workspaceRoot) => ({
          id: workspaceRoot === request.workspaceRoot ? existingProjectId : createdProjectId,
          environmentId,
          workspaceRoot,
        }),
      });
      // The first folder of a drop is still navigating when an activation shows its own thread.
      const older = handleDesktopAppActivationRequest(request, deps);
      await expect(
        handleDesktopAppActivationRequest(
          { ...request, requestId: "request-2", workspaceRoot: "/workspace/other" },
          deps,
        ),
      ).resolves.toMatchObject({ ok: true });
      finishOpen();

      // Null stops the rest of the drop, which would otherwise undo the newer open.
      await expect(older).resolves.toBeNull();
    },
  );

  it("ranks every folder of a drop by when it was dropped", async () => {
    let projectAppeared: () => void = () => undefined;
    const deps = dependencies({
      findProject: (_environmentId, workspaceRoot) =>
        workspaceRoot === "/workspace/new"
          ? null
          : { id: existingProjectId, environmentId, workspaceRoot },
      waitForProject: vi.fn(
        () =>
          new Promise<void>((resolve) => {
            projectAppeared = resolve;
          }),
      ),
    });
    const dropOrder = reserveDesktopAppActivationOrder();
    await handleDesktopAppActivationRequest(request, deps, dropOrder);
    // An activation that came after the drop, still adding its project.
    const activation = handleDesktopAppActivationRequest(
      { ...request, requestId: "request-2", workspaceRoot: "/workspace/new" },
      deps,
    );
    await handleDesktopAppActivationRequest(
      { ...request, requestId: "request-3", workspaceRoot: "/workspace/second" },
      deps,
      dropOrder,
    );
    projectAppeared();

    await expect(activation).resolves.toMatchObject({ ok: true, projectId: createdProjectId });
  });

  it("keeps a later folder of an older drop from replacing a newer open that showed", async () => {
    const deps = dependencies();
    const dropOrder = reserveDesktopAppActivationOrder();
    await handleDesktopAppActivationRequest(request, deps, dropOrder);
    await expect(
      handleDesktopAppActivationRequest({ ...request, requestId: "request-2" }, deps),
    ).resolves.toMatchObject({ ok: true });

    await expect(
      handleDesktopAppActivationRequest({ ...request, requestId: "request-3" }, deps, dropOrder),
    ).resolves.toBeNull();
    expect(deps.openThread).toHaveBeenCalledTimes(2);
  });

  it("ranks an activation by when the page received it, not when it was claimed", async () => {
    let grant: (claimed: boolean) => void = () => undefined;
    let claimRequested: () => void = () => undefined;
    const claimed = new Promise<void>((resolve) => {
      claimRequested = resolve;
    });
    const harness = receiverHarness(
      () =>
        new Promise<boolean>((resolve) => {
          grant = resolve;
          claimRequested();
        }),
    );
    const orders: Array<number | undefined> = [];
    harness.receiver.setHandler(async (input, order) => {
      orders.push(order);
      return success(input.requestId);
    });
    const completed = harness.nextCompletion();
    harness.deliver();
    await claimed;
    // A drop made while the claim is in flight.
    const dropOrder = reserveDesktopAppActivationOrder();
    grant(true);
    await completed;

    expect(orders).toHaveLength(1);
    expect(orders[0]).toBeLessThan(dropOrder);
  });

  it("lets an older open show its thread when a newer one fails before it could", async () => {
    let projectAppeared: () => void = () => undefined;
    const createProject = vi
      .fn<DesktopAppActivationDependencies["createProject"]>()
      .mockResolvedValueOnce(createdProjectId)
      .mockRejectedValueOnce(new Error("Active project already exists for workspace root"));
    const deps = dependencies({
      findProject: () => null,
      createProject,
      waitForProject: vi.fn(
        () =>
          new Promise<void>((resolve) => {
            projectAppeared = resolve;
          }),
      ),
    });
    // An activation and a drop of the same new folder.
    const older = handleDesktopAppActivationRequest(request, deps);
    await expect(
      handleDesktopAppActivationRequest({ ...request, requestId: "request-2" }, deps),
    ).resolves.toMatchObject({ ok: false, code: "project-create-failed" });
    projectAppeared();

    await expect(older).resolves.toMatchObject({ ok: true, projectId: createdProjectId });
  });

  it.each([
    {
      name: "rejects",
      handler: async (): Promise<DesktopAppActivationResponse | null> => {
        throw new Error("boom");
      },
      code: "internal-error",
    },
    {
      name: "throws before returning a promise",
      handler: (): Promise<DesktopAppActivationResponse | null> => {
        throw new Error("boom");
      },
      code: "internal-error",
    },
    {
      name: "gave way to a newer open",
      handler: async (): Promise<DesktopAppActivationResponse | null> => null,
      code: "thread-open-failed",
    },
  ])(
    "answers a request whose handler $name instead of leaving the command waiting",
    async ({ handler, code }) => {
      const harness = receiverHarness();
      const completed = harness.nextCompletion();
      harness.receiver.setHandler(handler);
      harness.deliver();
      await completed;

      expect(harness.completions).toEqual([
        expect.objectContaining({ requestId: request.requestId, ok: false, code }),
      ]);
    },
  );
});
