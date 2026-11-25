import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import { expect, it } from "@effect/vitest";
import {
  EnvironmentId,
  ProjectId,
  ProviderInstanceId,
  type ScheduledTaskError,
  ThreadId,
} from "@onicode/contracts";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Ref from "effect/Ref";

import * as ProviderAdapters from "../orchestration-v2/ProviderAdapterRegistry.ts";
import * as Launch from "../orchestration-v2/ThreadLaunchService.ts";
import * as Threads from "../orchestration-v2/ThreadManagementService.ts";
import * as RemoteDelegation from "../peer/RemoteDelegation.ts";
import * as Sqlite from "../persistence/Sqlite.ts";
import * as ManagedPolicy from "../policy/ManagedPolicy.ts";
import * as ManagedProjectFolders from "../project/ManagedProjectFolders.ts";
import * as Projects from "../project/ProjectService.ts";
import * as Providers from "../provider/ProviderRegistry.ts";
import * as Schedules from "../scheduledTasks/ScheduledTaskService.ts";
import * as Scheduler from "../scheduling/Scheduler.ts";
import * as Secrets from "../secrets/SecretRequests.ts";
import * as ServerSettings from "../serverSettings.ts";
import * as Git from "../vcs/GitVcsDriver.ts";
import * as TaskFolders from "../workspace/TaskFolders.ts";
import type { McpInvocationScope } from "./McpInvocationContext.ts";
import * as Mcp from "./OrchestratorMcpService.ts";

const dependencies = Layer.mergeAll(
  NodeCrypto.layer,
  NodeServices.layer,
  Sqlite.layerMemory,
  ServerSettings.layerTest(),
  ManagedPolicy.layerTest(),
  Layer.mock(ManagedProjectFolders.ManagedProjectFolders)({
    namedProjectsRoot: "/projects",
    isScratchProject: () => Effect.succeed(false),
  }),
  Layer.mock(Projects.ProjectService)({}),
  Layer.mock(Providers.ProviderRegistry)({}),
  Layer.mock(ProviderAdapters.ProviderAdapterRegistryV2)({}),
  Layer.mock(Threads.ThreadManagementService)({}),
  Layer.mock(Launch.ThreadLaunchService)({}),
  Layer.mock(Git.GitVcsDriver)({}),
  Layer.mock(TaskFolders.TaskFolders)({}),
  Layer.mock(Secrets.SecretRequests)({}),
  Layer.mock(RemoteDelegation.RemoteDelegation)({}),
  Layer.mock(Scheduler.Scheduler)({ register: () => Effect.void }),
);

const client: McpInvocationScope = {
  environmentId: EnvironmentId.make("schedule-test"),
  requestNamespace: "client",
  thread: undefined,
  client: { sessionId: "client", label: "Test", access: "full-access" },
  capabilities: new Set(["orchestration"]),
  issuedAt: 1,
};

const fixture = Effect.gen(function* () {
  const schedules = yield* Schedules.ScheduledTaskService;
  const { task } = yield* schedules.upsert({
    title: "Scheduled review",
    prompt: "Review",
    projectId: ProjectId.make("project"),
    schedule: { type: "interval", everyMs: 60_000 },
    enabled: true,
    workspaceStrategy: { type: "root" },
    modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "test" },
    runtimeMode: "full-access",
    interactionMode: "default",
  });
  return { schedules, id: task.id };
});

const layer = Schedules.layer.pipe(Layer.provideMerge(dependencies));

it.effect("an MCP edit cannot recreate a concurrently deleted schedule", () =>
  Effect.gen(function* () {
    const { schedules, id } = yield* fixture;
    const editing = {
      ...schedules,
      list: () => schedules.list().pipe(Effect.tap(() => schedules.delete({ id }))),
    };
    const result = yield* Mcp.OrchestratorMcpService.pipe(
      Effect.flatMap((mcp) =>
        mcp.updateScheduledTask(client, { scheduledTaskId: id, enabled: false }),
      ),
      Effect.provide(Mcp.layer),
      Effect.provideService(Schedules.ScheduledTaskService, editing),
      Effect.result,
    );
    expect((yield* schedules.list()).tasks).toEqual([]);
    expect(result._tag).toBe("Failure");
  }).pipe(Effect.provide(layer)),
);

it.effect("an MCP edit keeps a concurrent pause or edit of the fields it leaves alone", () =>
  Effect.gen(function* () {
    const { schedules, id } = yield* fixture;
    const stored = () =>
      schedules.list().pipe(Effect.map(({ tasks }) => tasks.find((task) => task.id === id)!));
    const edit = (between: Effect.Effect<unknown, ScheduledTaskError>, input: object) =>
      Mcp.OrchestratorMcpService.pipe(
        Effect.flatMap((mcp) => mcp.updateScheduledTask(client, { scheduledTaskId: id, ...input })),
        Effect.provide(Mcp.layer),
        Effect.provideService(Schedules.ScheduledTaskService, {
          ...schedules,
          list: () => schedules.list().pipe(Effect.tap(() => between)),
        }),
        Effect.result,
      );

    // A user pauses the task in Settings while an agent renames it.
    const renamed = yield* edit(schedules.setEnabled({ id, enabled: false }), { title: "Renamed" });
    expect(renamed._tag === "Failure" && renamed.failure.message).toContain(
      "The task changed while saving. Read the task again.",
    );
    expect(yield* stored()).toMatchObject({ enabled: false, title: "Scheduled review" });

    // Another client rewrites the prompt while an agent changes the schedule.
    const task = yield* stored();
    const rescheduled = yield* edit(
      schedules.upsert({ ...task, requireExisting: true, prompt: "Review twice" }),
      { schedule: { type: "interval", everyMs: 120_000 } },
    );
    expect(rescheduled._tag).toBe("Failure");
    expect(yield* stored()).toMatchObject({
      prompt: "Review twice",
      schedule: { everyMs: 60_000 },
    });

    // Another client changes the schedule while an agent rewrites the prompt.
    const current = yield* stored();
    const reprompted = yield* edit(
      schedules.upsert({
        ...current,
        requireExisting: true,
        schedule: { type: "interval", everyMs: 300_000 },
      }),
      { prompt: "Review once" },
    );
    expect(reprompted._tag).toBe("Failure");
    expect(yield* stored()).toMatchObject({
      prompt: "Review twice",
      schedule: { everyMs: 300_000 },
    });

    // Another client keeps one thread while an agent's edit works its mode out from the old one.
    const unbound = yield* stored();
    const remoded = yield* edit(
      schedules.upsert({ ...unbound, requireExisting: true, threadMode: "fresh_session" }),
      { freshSession: false },
    );
    expect(remoded._tag).toBe("Failure");
    expect(yield* stored()).toMatchObject({ threadMode: "fresh_session" });

    // With nothing in between, the same edit saves.
    const saved = yield* edit(Effect.void, { title: "Renamed" });
    expect(saved._tag).toBe("Success");
    expect(yield* stored()).toMatchObject({ enabled: false, title: "Renamed" });
  }).pipe(Effect.provide(layer)),
);

it.effect(
  "an MCP edit or unbind of a user schedule marks it agent-authored so firing runs under agent checks",
  () =>
    Effect.gen(function* () {
      const projectId = ProjectId.make("project");
      const boundThreadId = ThreadId.make("thread:bound");
      const launches = yield* Ref.make<ReadonlyArray<Launch.ThreadLaunchInput>>([]);
      const supervisedClient: McpInvocationScope = {
        environmentId: EnvironmentId.make("schedule-test"),
        requestNamespace: "client:supervised",
        thread: undefined,
        client: { sessionId: "client:supervised", label: "Test", access: "approval-required" },
        capabilities: new Set(["orchestration"]),
        issuedAt: 1,
      };
      const overrides = Layer.mergeAll(
        Layer.mock(Projects.ProjectService)({
          getById: (id) => Effect.succeedSome({ id, workspaceRoot: "/repo" } as never),
        }),
        Layer.mock(Git.GitVcsDriver)({
          resolvePrimaryRemoteName: () => Effect.succeed("origin"),
          resolveDefaultBranchName: () => Effect.succeed("main"),
          resolveCommit: () => Effect.succeed({ commitSha: "abc123" }),
        }),
        Layer.mock(Threads.ThreadManagementService)({
          getThreadShell: (threadId) =>
            Effect.succeed({
              id: threadId,
              projectId,
              runtimeMode: "approval-required",
              interactionMode: "default",
              archivedAt: null,
              deletedAt: null,
            } as never),
        }),
        Layer.mock(Launch.ThreadLaunchService)({
          isNonGitProject: () => Effect.succeed(false),
          launch: (input) =>
            Ref.update(launches, (all) => [...all, input]).pipe(
              Effect.as({ threadId: ThreadId.make("thread:fired"), resumed: false } as never),
            ),
        }),
      );
      const testDependencies = Layer.merge(dependencies, overrides);
      const testLayer = Mcp.layer.pipe(
        Layer.provideMerge(Schedules.layer),
        Layer.provideMerge(testDependencies),
      );

      yield* Effect.gen(function* () {
        const schedules = yield* Schedules.ScheduledTaskService;
        const mcp = yield* Mcp.OrchestratorMcpService;
        const base = {
          title: "Supervised review",
          prompt: "Review",
          projectId,
          schedule: { type: "interval", everyMs: 60_000 } as const,
          enabled: true,
          modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "test" },
          runtimeMode: "approval-required" as const,
          interactionMode: "default" as const,
        };
        const { task: bound } = yield* schedules.upsert({
          ...base,
          threadId: boundThreadId,
          workspaceStrategy: { type: "root" },
        });
        const { task: unbound } = yield* schedules.upsert({
          ...base,
          threadId: null,
          workspaceStrategy: { type: "worktree", baseRef: "origin/main", startFromOrigin: true },
        });
        expect(bound.createdBy).toBe("user");
        expect(unbound.createdBy).toBe("user");

        // Unbinding a user task via MCP switches it to a worktree and marks it agent-authored.
        yield* mcp.updateScheduledTask(supervisedClient, {
          scheduledTaskId: bound.id,
          bindToCurrentThread: false,
        });
        // Editing an existing unbound user worktree task via MCP marks it agent-authored too.
        yield* mcp.updateScheduledTask(supervisedClient, {
          scheduledTaskId: unbound.id,
          prompt: "Edited by agent",
        });

        const storedTasks = (yield* schedules.list()).tasks;
        const storedBound = storedTasks.find((task) => task.id === bound.id)!;
        const storedUnbound = storedTasks.find((task) => task.id === unbound.id)!;
        expect(storedBound).toMatchObject({
          createdBy: "agent",
          threadId: null,
          workspaceStrategy: { type: "worktree", baseRef: "origin/main", startFromOrigin: true },
        });
        expect(storedUnbound.createdBy).toBe("agent");

        // A later save omitting createdBy (such as Settings) keeps the stored author.
        const { createdBy: _createdBy, ...withoutCreatedBy } = storedBound;
        const { task: resaved } = yield* schedules.upsert({
          ...withoutCreatedBy,
          prompt: "Tweaked in settings",
        });
        expect(resaved.createdBy).toBe("agent");

        // Firing the unbound task passes createdBy: "agent" into ThreadLaunchService.
        yield* schedules.runNow({ id: bound.id });
        const fired = yield* Ref.get(launches);
        expect(fired).toHaveLength(1);
        expect(fired[0]).toMatchObject({
          createdBy: "agent",
          runtimeMode: "approval-required",
          workspaceStrategy: { type: "worktree", baseRef: "origin/main", startFromOrigin: true },
        });
      }).pipe(Effect.provide(testLayer));
    }),
);
