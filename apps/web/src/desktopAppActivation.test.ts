import { EnvironmentId, ProjectId, ThreadId } from "@onicode/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

import {
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
      environment: "available",
      enabled: false,
      localEnvironmentOff: false,
      expected: "pending",
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
                  entry: { enabled } as never,
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
});
