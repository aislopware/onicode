import { describe, expect, it } from "@effect/vitest";

import type {
  EnvironmentId,
  OrchestrationV2ThreadShell,
  Project,
  ThreadId,
} from "@onicode/contracts";
import { ProviderInstanceId, RuntimeRequestId } from "@onicode/contracts";
import * as DateTime from "effect/DateTime";

import { childThreadRequest, projectThreadAwarenessV2 } from "./agentAwareness.ts";

const NOW = "2026-05-22T12:00:00.000Z";

const project = {
  title: "onicode",
} satisfies Pick<Project, "title">;

describe("projectThreadAwarenessV2", () => {
  const updatedAt = DateTime.makeUnsafe(NOW);
  const v2Thread = (
    overrides: Partial<
      Pick<
        OrchestrationV2ThreadShell,
        | "activityRunStatus"
        | "latestRunBackgroundWake"
        | "latestRunStopRequested"
        | "status"
        | "pendingBackgroundTasks"
        | "pendingRuntimeRequest"
        | "lineage"
        | "settledOverride"
        | "lastVisitedAt"
        | "latestRunCompletedAt"
      >
    > = {},
  ) => ({
    id: "thread-2" as ThreadId,
    lineage: {
      rootThreadId: "thread-2" as ThreadId,
      parentThreadId: null,
      relationshipToParent: null,
    },
    title: "Integrate orchestration",
    modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "gpt-5.4" },
    status: "running" as const,
    pendingRuntimeRequest: null,
    settledOverride: null,
    updatedAt,
    ...overrides,
  });

  it("does not announce completion after a manual stop", () => {
    const input = {
      environmentId: "env-1" as EnvironmentId,
      project,
      thread: v2Thread({ status: "completed" }),
    };
    expect(projectThreadAwarenessV2(input)?.phase).toBe("completed");
    expect(
      projectThreadAwarenessV2({
        ...input,
        thread: { ...input.thread, latestRunStopRequested: true },
      }),
    ).toBeNull();
  });

  it.each([
    ["holds a wake run", true, [{ taskId: "watch", kind: "monitor" as const }], "running"],
    ["completes a wake run once its monitor ends", true, [], "completed"],
    [
      "holds a wake run while a monitor remains beside other work",
      true,
      [
        { taskId: "build", kind: "command" as const },
        { taskId: "watch", kind: "monitor" as const },
      ],
      "running",
    ],
    [
      "completes a wake run with only a command pending",
      true,
      [{ taskId: "build", kind: "command" as const }],
      "completed",
    ],
    [
      "completes a run the user started",
      false,
      [{ taskId: "watch", kind: "monitor" as const }],
      "completed",
    ],
  ] as const)("%s while a monitor wakes the thread", (_, latestRunBackgroundWake, tasks, phase) => {
    expect(
      projectThreadAwarenessV2({
        environmentId: "env-1" as EnvironmentId,
        project,
        thread: v2Thread({
          status: "completed",
          latestRunBackgroundWake,
          pendingBackgroundTasks: [...tasks],
        }),
      })?.phase,
    ).toBe(phase);
  });

  it.each([
    ["running", "subagent"],
    ["completed", "subagent"],
    ["failed", "subagent"],
    ["running", "side chat"],
  ] as const)("does not publish %s %s activity", (status, kind) => {
    expect(
      projectThreadAwarenessV2({
        environmentId: "env-1" as EnvironmentId,
        project,
        thread: v2Thread({
          status,
          lineage: {
            rootThreadId: "parent" as ThreadId,
            parentThreadId: "parent" as ThreadId,
            ...(kind === "subagent"
              ? { relationshipToParent: "subagent" }
              : { relationshipToParent: "fork", sideChat: true }),
          },
        }),
      }),
    ).toBeNull();
  });

  it("keeps an older activity run visible over a newer cancelled run", () => {
    expect(
      projectThreadAwarenessV2({
        environmentId: "env-1" as EnvironmentId,
        project,
        thread: v2Thread({ status: "cancelled", activityRunStatus: "running" }),
      }),
    ).toMatchObject({ phase: "running", headline: "Agent is working" });
  });

  it.each([
    ["completed", "completed", false],
    ["failed", "failed", false],
    ["monitor-held", "completed", true],
  ] as const)("removes settled %s work", (_, status, latestRunBackgroundWake) => {
    expect(
      projectThreadAwarenessV2({
        environmentId: "env-1" as EnvironmentId,
        project,
        thread: v2Thread({
          status,
          settledOverride: "settled",
          latestRunBackgroundWake,
          pendingBackgroundTasks: [{ taskId: "watch", kind: "monitor" }],
        }),
      }),
    ).toBeNull();
  });

  it.each(["completed", "failed"] as const)("marks %s work someone has opened since", (status) => {
    const seenWhenVisited = (lastVisitedAt: DateTime.Utc | null) =>
      projectThreadAwarenessV2({
        environmentId: "env-1" as EnvironmentId,
        project,
        thread: v2Thread({ status, latestRunCompletedAt: updatedAt, lastVisitedAt }),
      })?.seen;
    expect(seenWhenVisited(null)).toBeUndefined();
    expect(seenWhenVisited(DateTime.subtract(updatedAt, { milliseconds: 1 }))).toBeUndefined();
    expect(seenWhenVisited(updatedAt)).toBe(true);
  });

  it.each(["starting", "running", "waiting"] as const)(
    "keeps settled %s work visible",
    (status) => {
      expect(
        projectThreadAwarenessV2({
          environmentId: "env-1" as EnvironmentId,
          project,
          thread: v2Thread({ status, settledOverride: "settled" }),
        }),
      ).toMatchObject({ phase: status === "starting" ? "starting" : "running" });
    },
  );

  it.each(["user_input", "command"] as const)(
    "keeps a settled terminal thread with a pending %s request",
    (kind) => {
      expect(
        projectThreadAwarenessV2({
          environmentId: "env-1" as EnvironmentId,
          project,
          thread: v2Thread({
            status: "completed",
            settledOverride: "settled",
            pendingRuntimeRequest: {
              id: RuntimeRequestId.make("request-settled"),
              kind,
              createdAt: updatedAt,
            },
          }),
        }),
      ).toMatchObject({
        phase: kind === "user_input" ? "waiting_for_input" : "waiting_for_approval",
      });
    },
  );

  it("keeps settled work held open by an awaited monitor", () => {
    expect(
      projectThreadAwarenessV2({
        environmentId: "env-1" as EnvironmentId,
        project,
        thread: v2Thread({
          status: "completed",
          settledOverride: "settled",
          pendingBackgroundTasks: [{ taskId: "watch", kind: "monitor", awaited: true }],
        }),
      }),
    ).toMatchObject({ phase: "running" });
  });

  it.each([
    ["only a dev server", "completed", [{ taskId: "dev", kind: "command" }]],
    ["a dev server's watchdog monitor", "completed", [{ taskId: "watch", kind: "monitor" }]],
    [
      "a command the agent waits on",
      "running",
      [{ taskId: "tests", kind: "command", awaited: true }],
    ],
    [
      "a dev server and a subagent",
      "running",
      [
        { taskId: "dev", kind: "command" },
        { taskId: "review", kind: "subagent" },
      ],
    ],
  ] as const)("reports a completed run waiting on %s as %s", (_case, phase, tasks) => {
    expect(
      projectThreadAwarenessV2({
        environmentId: "env-1" as EnvironmentId,
        project,
        thread: v2Thread({ status: "completed", pendingBackgroundTasks: tasks }),
      }),
    ).toMatchObject({ phase });
  });

  it("reports an async question as input while the run keeps working", () => {
    const input = {
      environmentId: "env-1" as EnvironmentId,
      project,
      thread: v2Thread({
        pendingRuntimeRequest: {
          id: RuntimeRequestId.make("request-async"),
          kind: "user_input",
          createdAt: updatedAt,
          blocking: false,
        },
      }),
    };
    expect(projectThreadAwarenessV2(input)).toMatchObject({
      phase: "waiting_for_input",
      headline: "Waiting for input",
    });
  });

  it("shows what a thread under it asks of the person, an approval first", () => {
    const phase = (
      childRequest: "approval" | "user_input",
      pendingRuntimeRequest: OrchestrationV2ThreadShell["pendingRuntimeRequest"] = null,
    ) =>
      projectThreadAwarenessV2({
        environmentId: "env-1" as EnvironmentId,
        project,
        thread: v2Thread({ pendingRuntimeRequest }),
        childRequest,
      })?.phase;
    const own = (kind: "user_input" | "command") => ({
      id: RuntimeRequestId.make("request-own"),
      kind,
      createdAt: updatedAt,
    });

    expect(phase("user_input")).toBe("waiting_for_input");
    expect(phase("approval")).toBe("waiting_for_approval");
    expect(phase("approval", own("user_input"))).toBe("waiting_for_approval");
    expect(phase("user_input", own("command"))).toBe("waiting_for_approval");
  });

  it("does not present authentication refreshes as user approvals", () => {
    expect(
      projectThreadAwarenessV2({
        environmentId: "env-1" as EnvironmentId,
        project,
        thread: v2Thread({
          pendingRuntimeRequest: {
            id: RuntimeRequestId.make("request-auth-refresh"),
            kind: "auth_refresh",
            createdAt: updatedAt,
          },
        }),
      }),
    ).toMatchObject({ phase: "running", headline: "Agent is working" });
  });
});

describe("childThreadRequest", () => {
  const createdAt = DateTime.makeUnsafe(NOW);
  const parent = "thread-1" as ThreadId;
  const request = (kind: "user_input" | "command" | "auth_refresh", userOnly?: true) => ({
    id: RuntimeRequestId.make("request-1"),
    kind,
    createdAt,
    ...(userOnly ? { userOnly } : {}),
  });
  const child = (
    overrides: Partial<Parameters<typeof childThreadRequest>[0]> & {
      readonly sideChat?: true;
    } = {},
  ) => {
    const { sideChat, ...rest } = overrides;
    return childThreadRequest({
      lineage: {
        rootThreadId: parent,
        parentThreadId: parent,
        relationshipToParent: sideChat ? null : "subagent",
        ...(sideChat ? { sideChat } : {}),
      },
      creationSource: "server",
      status: "running",
      activityRunStatus: "running",
      pendingRuntimeRequest: request("command"),
      ...rest,
    });
  };

  it("is a live subagent's approval or the card only a person can answer", () => {
    expect(child()).toBe("approval");
    expect(child({ pendingRuntimeRequest: request("user_input", true) })).toBe("user_input");
    // The agent above answers its subagent's questions itself.
    expect(child({ pendingRuntimeRequest: request("user_input") })).toBeNull();
    expect(child({ pendingRuntimeRequest: request("auth_refresh") })).toBeNull();
    expect(child({ pendingRuntimeRequest: null })).toBeNull();
    // Once its run ends, nothing waits on the answer.
    expect(child({ status: "completed", activityRunStatus: null })).toBeNull();
    // A provider's own subagent asks on its parent.
    expect(child({ creationSource: "provider" })).toBeNull();
  });

  it("is anything a side chat asks, and nothing a listed thread does", () => {
    expect(child({ sideChat: true, pendingRuntimeRequest: request("user_input") })).toBe(
      "user_input",
    );
    expect(child({ sideChat: true })).toBe("approval");
    expect(
      child({
        lineage: { rootThreadId: parent, parentThreadId: null, relationshipToParent: null },
      }),
    ).toBeNull();
  });
});
