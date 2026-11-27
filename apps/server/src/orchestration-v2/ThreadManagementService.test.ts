import { expect, it } from "@effect/vitest";
import {
  CommandId,
  EnvironmentId,
  MessageId,
  type OrchestrationV2Command,
  type OrchestrationV2ServerCommand,
  type OrchestrationV2Run,
  type OrchestrationV2ProviderThread,
  ProviderThreadId,
  ProviderDriverKind,
  ProviderSessionId,
  type OrchestrationV2StoredEvent,
  type OrchestrationV2ThreadProjection,
  ProjectId,
  ProviderInstanceId,
  RunId,
  ThreadId,
} from "@onicode/contracts";
import * as Context from "effect/Context";
import * as Deferred from "effect/Deferred";
import * as DateTime from "effect/DateTime";
import * as Duration from "effect/Duration";
import * as Option from "effect/Option";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Queue from "effect/Queue";
import * as Result from "effect/Result";
import * as Stream from "effect/Stream";
import * as TestClock from "effect/testing/TestClock";

import { AgentScope } from "@onicode/shared/AgentScope";

import * as ServerEnvironment from "../environment/ServerEnvironment.ts";
import * as ServerSettings from "../serverSettings.ts";
import * as SqlitePersistence from "../persistence/Sqlite.ts";
import * as CommandReceiptStore from "./CommandReceiptStore.ts";
import * as Orchestrator from "./Orchestrator.ts";
import * as ProviderSessionManager from "./ProviderSessionManager.ts";
import * as IdAllocator from "@onicode/provider-core/server/IdAllocator";
import * as ProviderAdapter from "@onicode/provider-core/server/ProviderAdapter";
import * as ThreadManagementService from "./ThreadManagementService.ts";
import * as ThreadCommandExecutor from "./ThreadCommandExecutor.ts";
import * as LegacyV1ThreadImporter from "./legacy/LegacyV1ThreadImporter.ts";
import {
  ThreadWorktreeRestore,
  WorktreeRecreateError,
} from "../workspace/restoreMissingWorktree.ts";

it("stamps authoritative provenance on commands that create threads or messages", () => {
  const command: OrchestrationV2Command = {
    type: "thread.create",
    createdBy: "agent",
    creationSource: "mcp",
    commandId: CommandId.make("command:thread-management:create"),
    threadId: ThreadId.make("thread:thread-management:create"),
    projectId: ProjectId.make("project:thread-management"),
    title: "Thread management",
    modelSelection: {
      instanceId: ProviderInstanceId.make("codex"),
      model: "gpt-5-codex",
    },
    runtimeMode: "full-access",
    interactionMode: "default",
    branch: null,
    worktreePath: null,
    // A client cannot claim a linked environment's origin, or a parent there.
    linkOrigin: { sessionId: "forged", label: "Somewhere else" },
    delegatedFrom: {
      environmentId: EnvironmentId.make("environment:elsewhere"),
      threadId: ThreadId.make("thread:elsewhere"),
      title: "Somewhere else",
    },
    // A client's message goes through intake; only the server folds one into the creation.
    initialMessage: {
      messageId: MessageId.make("message:thread-management:forged"),
      text: "Forged",
      attachments: [],
    },
    // Nor a workflow run it is a step of.
    workflowRun: {
      environmentId: EnvironmentId.make("environment:elsewhere"),
      runId: "run-1",
      workflow: "fix",
      step: "write",
    },
  };

  const stamped = ThreadManagementService.withCreationProvenance(command, {
    createdBy: "user",
    creationSource: "web",
  });
  expect(stamped).toMatchObject({
    createdBy: "user",
    creationSource: "web",
  });
  expect(stamped).not.toHaveProperty("linkOrigin");
  expect(stamped).not.toHaveProperty("delegatedFrom");
  expect(stamped).not.toHaveProperty("initialMessage");
  expect(stamped).not.toHaveProperty("workflowRun");
});

const layerReceiptsTest = CommandReceiptStore.layer.pipe(
  Layer.provide(SqlitePersistence.layerMemory),
);

const layerDispatchChokepointTest =
  (settingsLayer: ReturnType<typeof ServerSettings.layerTest>) => (dispatched: Array<string>) =>
    ThreadManagementService.layer.pipe(
      Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
      Layer.provide(
        Layer.mock(Orchestrator.OrchestratorV2)({
          getThreadRecords: () =>
            Effect.succeed({
              thread: {
                id: ThreadId.make("thread:thread-management:chokepoint"),
                projectId: ProjectId.make("project:thread-management:chokepoint"),
                deletedAt: null,
                modelSelection: {
                  instanceId: ProviderInstanceId.make("codex_work"),
                  model: "test",
                },
              },
            } as unknown as OrchestrationV2ThreadProjection),
          dispatch: (command) =>
            Effect.sync(() => {
              dispatched.push(command.type);
              return { sequence: 1, storedEvents: [] } as never;
            }),
        }),
      ),
      Layer.provide(settingsLayer),
      Layer.provideMerge(layerReceiptsTest),
    );

it.effect.each([
  { label: "refuses a continuation message.dispatch", missedRunId: undefined, passes: false },
  // Held in the queue it starts nothing, and refused it would be lost with the turn that ended.
  {
    label: "lets a steer that missed a failed turn back into the queue",
    missedRunId: RunId.make("run:thread-management:chokepoint-missed"),
    passes: true,
  },
  // A delivery intent can turn the message into a new turn, so the claim alone opens nothing.
  {
    label: "refuses a missed steer that names a delivery intent",
    missedRunId: RunId.make("run:thread-management:chokepoint-missed"),
    deliveryIntent: "auto" as const,
    passes: false,
  },
])(
  "the dispatch chokepoint $label on a project-disabled instance before reaching the orchestrator",
  ({ missedRunId, deliveryIntent, passes }) => {
    const projectId = ProjectId.make("project:thread-management:chokepoint");
    const threadId = ThreadId.make("thread:thread-management:chokepoint");
    const instanceId = ProviderInstanceId.make("codex_work");
    const commandId = CommandId.make("command:thread-management:chokepoint-disabled");
    const dispatched: Array<string> = [];
    const layerTest = layerDispatchChokepointTest(
      ServerSettings.layerTest({
        providerInstances: { [instanceId]: { driver: "codex", enabled: true } },
        projectSettingsOverrides: {
          [projectId]: { providerInstances: { [instanceId]: { enabled: false } } },
        },
      }),
    )(dispatched);

    return Effect.gen(function* () {
      const threads = yield* ThreadManagementService.ThreadManagementService;
      const result = yield* Effect.result(
        threads.dispatch({
          type: "message.dispatch",
          commandId,
          threadId,
          messageId: MessageId.make("message:thread-management:chokepoint-disabled"),
          text: "Background task completed.",
          attachments: [],
          dispatchMode: { type: "queue_after_active" },
          ...(missedRunId === undefined ? {} : { steerMissedRunId: missedRunId }),
          ...(deliveryIntent === undefined ? {} : { deliveryIntent }),
          createdBy: "agent",
          creationSource: "server",
        }),
      );

      if (passes) {
        expect(dispatched).toEqual(["message.dispatch"]);
        return;
      }
      const error = Result.getOrThrow(Result.flip(result));
      expect(error).toBeInstanceOf(
        ThreadManagementService.ThreadManagementProviderInstanceDisabledError,
      );
      expect(error).toMatchObject({ projectId, instanceId });
      expect(dispatched).toEqual([]);
    }).pipe(Effect.provide(layerTest));
  },
);

it.effect(
  "a replay of an already-accepted command id dispatches despite the instance now being disabled",
  () => {
    const projectId = ProjectId.make("project:thread-management:chokepoint");
    const threadId = ThreadId.make("thread:thread-management:chokepoint");
    const instanceId = ProviderInstanceId.make("codex_work");
    const commandId = CommandId.make("command:thread-management:chokepoint-replay");
    const dispatched: Array<string> = [];
    const layerTest = layerDispatchChokepointTest(
      ServerSettings.layerTest({
        providerInstances: { [instanceId]: { driver: "codex", enabled: true } },
        projectSettingsOverrides: {
          [projectId]: { providerInstances: { [instanceId]: { enabled: false } } },
        },
      }),
    )(dispatched);

    return Effect.gen(function* () {
      const receipts = yield* CommandReceiptStore.CommandReceiptStoreV2;
      yield* receipts.insertIfAbsent({
        commandId,
        threadId,
        commandType: "message.dispatch",
        acceptedAt: yield* DateTime.now,
        resultSequence: 1,
        status: "accepted",
        error: null,
      });

      const threads = yield* ThreadManagementService.ThreadManagementService;
      yield* threads.dispatch({
        type: "message.dispatch",
        commandId,
        threadId,
        messageId: MessageId.make("message:thread-management:chokepoint-replay"),
        text: "Background task completed.",
        attachments: [],
        dispatchMode: { type: "queue_after_active" },
        createdBy: "agent",
        creationSource: "server",
      });

      expect(dispatched).toEqual(["message.dispatch"]);
    }).pipe(Effect.provide(layerTest));
  },
);

it.effect.each([
  { label: "fails a send that starts a turn", running: false },
  { label: "still steers a running turn", running: true },
])("sendToThread with a missing worktree that cannot be recreated $label", ({ running }) => {
  const projectId = ProjectId.make("project:thread-management:worktree-gone");
  const threadId = ThreadId.make("thread:thread-management:worktree-gone");
  const dispatched: Array<string> = [];
  const layerTest = ThreadManagementService.layerWithLegacyImporter.pipe(
    Layer.provide(
      Layer.succeed(ThreadWorktreeRestore, {
        takeRecreated: () => Effect.undefined,
        restore: () =>
          Effect.fail(
            new WorktreeRecreateError({
              worktreePath: "/worktrees/gone",
              branch: "onicode/gone",
              cause: new Error("branch_checked_out_in_worktree"),
            }),
          ),
      }),
    ),
    Layer.provide(
      Layer.mock(LegacyV1ThreadImporter.LegacyV1ThreadImporter)({
        ensureTranscript: () => Effect.succeed({ importedThreadCount: 0, importedMessageCount: 0 }),
      }),
    ),
    Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
    Layer.provide(
      Layer.mock(Orchestrator.OrchestratorV2)({
        getThreadRecords: () =>
          Effect.succeed({
            thread: {
              id: threadId,
              projectId,
              deletedAt: null,
              archivedAt: null,
              worktreePath: "/worktrees/gone",
              branch: "onicode/gone",
              modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "test" },
            },
            runs: running
              ? [
                  {
                    id: RunId.make("run:thread-management:worktree-gone"),
                    status: "running",
                    activeAttemptId: "attempt:thread-management:worktree-gone",
                  },
                ]
              : [],
            providerTurns: running
              ? [
                  {
                    runAttemptId: "attempt:thread-management:worktree-gone",
                    status: "running",
                  },
                ]
              : [],
            messages: [],
            turnItems: [],
          } as unknown as OrchestrationV2ThreadProjection),
        dispatch: (command) =>
          Effect.sync(() => {
            dispatched.push(command.type);
            return { sequence: 1, storedEvents: [] } as never;
          }),
      }),
    ),
    Layer.provide(ServerSettings.layerTest()),
    Layer.provideMerge(layerReceiptsTest),
  );

  return Effect.gen(function* () {
    const threads = yield* ThreadManagementService.ThreadManagementService;
    const error = yield* Effect.flip(
      threads.sendToThread({
        projectId,
        threadId,
        commandId: CommandId.make("command:thread-management:worktree-gone"),
        messageId: MessageId.make("message:thread-management:worktree-gone"),
        text: "Reply with ok.",
        attachments: [],
        mode: "auto",
        createdBy: "agent",
        creationSource: "mcp",
      }),
    );
    // The mock records no message, so a dispatched send ends in the projection read.
    expect(error._tag).toBe(
      running
        ? "ThreadManagementDurableRunProjectionError"
        : "ThreadManagementWorktreeRestoreError",
    );
    expect(dispatched).toEqual(running ? ["message.dispatch"] : []);
  }).pipe(Effect.provide(layerTest));
});

it.effect("waitForThread follows a restart continuation instead of the cut run", () =>
  Effect.gen(function* () {
    const projectId = ProjectId.make("project:thread-management:wait-restart");
    const threadId = ThreadId.make("thread:thread-management:wait-restart");
    const runId = RunId.make("run:thread-management:wait-restart");
    const continuationId = RunId.make("run:thread-management:wait-restart:2");
    const subscribed = yield* Deferred.make<void>();
    const events = yield* Queue.unbounded<OrchestrationV2StoredEvent>();
    let runs: Array<{
      id: RunId;
      status: OrchestrationV2Run["status"];
      restartContinuationOfRunId?: RunId;
    }> = [{ id: runId, status: "cancelled" }];
    let pending = true;
    const stored = (sequence: number, event: object) =>
      ({ sequence, event: { threadId, ...event } }) as unknown as OrchestrationV2StoredEvent;
    const layerTest = ThreadManagementService.layer.pipe(
      Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
      Layer.provide(
        Layer.mock(Orchestrator.OrchestratorV2)({
          getThreadEventSequence: () => Effect.succeed(0),
          restartContinuationPending: (run) => Effect.succeed(pending && run.id === runId),
          awaitRestartContinuation: () => Effect.never,
          streamStoredEventsFrom: (input) =>
            input?.eventType === "run.updated"
              ? Stream.fromEffect(Deferred.succeed(subscribed, undefined)).pipe(
                  Stream.drain,
                  Stream.concat(Stream.fromQueue(events)),
                )
              : Stream.never,
          getThreadRecords: () =>
            Effect.sync(
              () =>
                ({
                  thread: { id: threadId, projectId, deletedAt: null },
                  runs,
                }) as unknown as OrchestrationV2ThreadProjection,
            ),
        }),
      ),
      Layer.provide(ServerSettings.layerTest()),
      Layer.provide(layerReceiptsTest),
    );
    const service = yield* ThreadManagementService.ThreadManagementService.pipe(
      Effect.provide(layerTest),
    );
    const fiber = yield* service
      .waitForThread({ projectId, threadId, runId, timeoutMs: 60 * 60 * 1_000 })
      .pipe(Effect.forkChild);
    yield* Deferred.await(subscribed);
    const continuation = { id: continuationId, restartContinuationOfRunId: runId };
    runs = [...runs, { ...continuation, status: "running" }];
    pending = false;
    yield* Queue.offer(
      events,
      stored(1, { type: "run.updated", payload: { ...continuation, status: "running" } }),
    );
    runs = [runs[0]!, { ...continuation, status: "completed" }];
    yield* Queue.offer(
      events,
      stored(2, { type: "run.updated", payload: { ...continuation, status: "completed" } }),
    );
    const result = yield* Fiber.join(fiber);

    expect(result).toMatchObject({
      timedOut: false,
      run: { id: continuationId, status: "completed" },
    });
  }),
);

it.effect.each(["usageLimitContinuationOfRunId", "manualContinuationOfRunId"] as const)(
  "waitForThread takes the result from a run's %s",
  (link) =>
    Effect.gen(function* () {
      const projectId = ProjectId.make("project:thread-management:wait-continued");
      const threadId = ThreadId.make("thread:thread-management:wait-continued");
      const runId = RunId.make("run:thread-management:wait-continued");
      const continuationId = RunId.make("run:thread-management:wait-continued:2");
      const outputSchema = { type: "object" };
      const output = { value: { head: "abc" }, submittedAt: "2026-11-21T00:00:00.000Z" };
      const layerTest = ThreadManagementService.layer.pipe(
        Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
        Layer.provide(
          Layer.mock(Orchestrator.OrchestratorV2)({
            getThreadEventSequence: () => Effect.succeed(0),
            restartContinuationPending: () => Effect.succeed(false),
            streamStoredEventsFrom: () => Stream.never,
            getThreadRecords: () =>
              Effect.succeed({
                thread: { id: threadId, projectId, deletedAt: null },
                runs: [
                  { id: runId, ordinal: 1, status: "failed", outputSchema },
                  {
                    id: continuationId,
                    ordinal: 2,
                    status: "completed",
                    outputSchema,
                    output,
                    [link]: runId,
                  },
                ],
              } as unknown as OrchestrationV2ThreadProjection),
          }),
        ),
        Layer.provide(ServerSettings.layerTest()),
        Layer.provide(layerReceiptsTest),
      );
      const service = yield* ThreadManagementService.ThreadManagementService.pipe(
        Effect.provide(layerTest),
      );
      const result = yield* service.waitForThread({
        projectId,
        threadId,
        runId,
        timeoutMs: 1_000,
        untilOutput: true,
      });

      expect(result).toMatchObject({ timedOut: false, run: { id: continuationId, output } });
    }),
);

it.effect.each([
  { steer: "read" },
  // The run-end reaction moves a message Claude dropped unread to a run of its own.
  { steer: "dropped" },
  // Stop drops the unread steer instead, so the stopped run answers.
  { steer: "stopped" },
  // The user cancels the run the dropped message moved to before it starts.
  { steer: "requeue cancelled" },
  // The move commits right after the wait sees the message on the ended run.
  { steer: "requeued meanwhile" },
] as const)(
  "waitForThread follows a queued message steered into the running turn, $steer",
  ({ steer }) =>
    Effect.gen(function* () {
      const projectId = ProjectId.make("project:thread-management:wait-steered");
      const threadId = ThreadId.make("thread:thread-management:wait-steered");
      const activeId = RunId.make("run:thread-management:wait-steered:1");
      const queuedId = RunId.make("run:thread-management:wait-steered:2");
      const requeuedId = RunId.make("run:thread-management:wait-steered:3");
      const messageId = MessageId.make("message:thread-management:wait-steered");
      const subscribed = yield* Deferred.make<void>();
      const events = yield* Queue.unbounded<OrchestrationV2StoredEvent>();
      const steerItemReads = yield* Queue.unbounded<void>();
      let promoted = false;
      let activeStatus: OrchestrationV2Run["status"] = "running";
      let requeued: OrchestrationV2Run["status"] | undefined;
      const layerTest = ThreadManagementService.layer.pipe(
        Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
        Layer.provide(
          Layer.mock(Orchestrator.OrchestratorV2)({
            getThreadEventSequence: () => Effect.succeed(0),
            restartContinuationPending: () => Effect.succeed(false),
            steerDeliveryPending: () => Effect.succeed(false),
            streamStoredEventsFrom: (input) =>
              input?.eventType === "run.updated"
                ? Stream.fromEffect(Deferred.succeed(subscribed, undefined)).pipe(
                    Stream.drain,
                    Stream.concat(Stream.fromQueue(events)),
                  )
                : Stream.never,
            getThreadRecords: (_threadId, fields, filter) =>
              Effect.sync(() => {
                const activeEnded = activeStatus === "completed";
                const records = {
                  thread: { id: threadId, projectId, deletedAt: null },
                  runs: [
                    { id: activeId, status: activeStatus },
                    {
                      id: queuedId,
                      status: promoted ? "cancelled" : "queued",
                      startedAt: null,
                      userMessageId: messageId,
                    },
                    ...(requeued === undefined
                      ? []
                      : [{ id: requeuedId, status: requeued, userMessageId: messageId }]),
                  ],
                  messages: [
                    {
                      id: messageId,
                      runId: requeued !== undefined ? requeuedId : promoted ? activeId : queuedId,
                    },
                  ],
                  turnItems: [
                    {
                      type: "user_message",
                      runId: requeued === undefined ? activeId : requeuedId,
                      messageId,
                      inputIntent:
                        requeued === undefined ? "promoted_queued_to_steer" : "queued_turn",
                      status:
                        steer === "read" || requeued === "completed" ? "completed" : "pending",
                    },
                    ...(steer === "stopped"
                      ? [{ type: "run_interrupt_request", runId: activeId }]
                      : []),
                  ].filter((item) => filter?.turnItemRunIds?.includes(item.runId) ?? true),
                } as unknown as OrchestrationV2ThreadProjection;
                // The turn ends right after the wait learns which run took the message.
                if (promoted && fields.some((field) => field === "messages"))
                  activeStatus = "completed";
                if (
                  steer === "requeued meanwhile" &&
                  activeEnded &&
                  requeued === undefined &&
                  !fields.some((field) => field === "turnItems")
                )
                  requeued = "running";
                if (fields.some((field) => field === "turnItems"))
                  Queue.offerUnsafe(steerItemReads, undefined);
                return records;
              }),
          }),
        ),
        Layer.provide(ServerSettings.layerTest()),
        Layer.provide(layerReceiptsTest),
      );
      const service = yield* ThreadManagementService.ThreadManagementService.pipe(
        Effect.provide(layerTest),
      );
      const fiber = yield* service
        .waitForThread({ projectId, threadId, runId: queuedId, timeoutMs: 60 * 60 * 1_000 })
        .pipe(Effect.forkChild);
      yield* Deferred.await(subscribed);
      promoted = true;
      // Both updates are buffered before the wait reads either.
      yield* Queue.offerAll(
        events,
        [
          { id: queuedId, status: "cancelled" },
          { id: activeId, status: "completed" },
        ].map(
          (payload, index) =>
            ({
              sequence: index + 1,
              event: { threadId, type: "run.updated", payload },
            }) as unknown as OrchestrationV2StoredEvent,
        ),
      );
      if (steer !== "read" && steer !== "stopped") {
        yield* Effect.raceFirst(Queue.take(steerItemReads), Fiber.await(fiber));
        requeued = steer === "requeue cancelled" ? "cancelled" : "completed";
        yield* Queue.offer(events, {
          sequence: 3,
          event: {
            threadId,
            type: "run.updated",
            payload: { id: requeuedId, status: requeued },
          },
        } as unknown as OrchestrationV2StoredEvent);
        // A wait that missed the end reports a timeout instead of hanging the test.
        if (steer === "requeue cancelled") yield* TestClock.adjust(Duration.hours(1));
      }

      expect(yield* Fiber.join(fiber)).toMatchObject({
        timedOut: false,
        run:
          steer === "read" || steer === "stopped"
            ? { id: activeId, status: "completed" }
            : { id: requeuedId, status: requeued },
      });
    }),
);

it.effect.each([
  { settles: "while the wait is open" },
  // The worker moves the message just as the wait looks; a stale owner must not answer it.
  { settles: "as the wait first looks" },
] as const)("waitForThread follows a steer that missed its turn, settled $settles", ({ settles }) =>
  Effect.gen(function* () {
    const projectId = ProjectId.make("project:thread-management:wait-missed");
    const threadId = ThreadId.make("thread:thread-management:wait-missed");
    const failedId = RunId.make("run:thread-management:wait-missed:1");
    const queuedId = RunId.make("run:thread-management:wait-missed:2");
    const requeuedId = RunId.make("run:thread-management:wait-missed:3");
    const messageId = MessageId.make("message:thread-management:wait-missed");
    const subscribed = yield* Deferred.make<void>();
    const events = yield* Queue.unbounded<OrchestrationV2StoredEvent>();
    // The turn that took the steer failed before the worker delivered it.
    let requeued: OrchestrationV2Run["status"] | undefined;
    let settlesOnAsk = false;
    const steerSettled = yield* Deferred.make<void>();
    const awaitingSteer = yield* Queue.unbounded<void>();
    const layerTest = ThreadManagementService.layer.pipe(
      Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
      Layer.provide(
        Layer.mock(Orchestrator.OrchestratorV2)({
          getThreadEventSequence: () => Effect.succeed(0),
          restartContinuationPending: () => Effect.succeed(false),
          steerDeliveryPending: () =>
            Effect.sync(() => {
              if (settlesOnAsk) requeued ??= "queued";
              return requeued === undefined;
            }),
          awaitSteerDelivery: () =>
            Queue.offer(awaitingSteer, undefined).pipe(
              Effect.andThen(Deferred.await(steerSettled)),
            ),
          streamStoredEventsFrom: (input) =>
            input?.eventType === "run.updated"
              ? Stream.fromEffect(Deferred.succeed(subscribed, undefined)).pipe(
                  Stream.drain,
                  Stream.concat(Stream.fromQueue(events)),
                )
              : Stream.never,
          getThreadRecords: () =>
            Effect.sync(
              () =>
                ({
                  thread: { id: threadId, projectId, deletedAt: null },
                  runs: [
                    { id: failedId, status: "failed" },
                    {
                      id: queuedId,
                      status: "cancelled",
                      startedAt: null,
                      userMessageId: messageId,
                    },
                    ...(requeued === undefined
                      ? []
                      : [{ id: requeuedId, status: requeued, userMessageId: messageId }]),
                  ],
                  messages: [
                    { id: messageId, runId: requeued === undefined ? failedId : requeuedId },
                  ],
                  turnItems: [],
                }) as unknown as OrchestrationV2ThreadProjection,
            ),
        }),
      ),
      Layer.provide(ServerSettings.layerTest()),
      Layer.provide(layerReceiptsTest),
    );
    const service = yield* ThreadManagementService.ThreadManagementService.pipe(
      Effect.provide(layerTest),
    );
    // A wait that ends first reports the steer as still on its way, within its own timeout.
    const short = yield* service
      .waitForThread({ projectId, threadId, runId: queuedId, timeoutMs: 1_000 })
      .pipe(Effect.forkChild);
    yield* Queue.take(awaitingSteer);
    yield* TestClock.adjust(Duration.millis(1_000));
    expect(yield* Fiber.join(short)).toMatchObject({ timedOut: true, run: { id: failedId } });

    settlesOnAsk = settles === "as the wait first looks";
    const fiber = yield* service
      .waitForThread({ projectId, threadId, runId: queuedId, timeoutMs: 60 * 60 * 1_000 })
      .pipe(Effect.forkChild);
    if (!settlesOnAsk) {
      yield* Queue.take(awaitingSteer);
      // The worker settles the steer by moving the message to a run of its own.
      requeued = "queued";
      yield* Deferred.succeed(steerSettled, undefined);
    }
    // A wait that settles on the failed turn never subscribes.
    yield* Effect.raceFirst(Deferred.await(subscribed), Fiber.join(fiber));
    requeued = "completed";
    yield* Queue.offer(events, {
      sequence: 1,
      event: { threadId, type: "run.updated", payload: { id: requeuedId, status: "completed" } },
    } as unknown as OrchestrationV2StoredEvent);

    expect(yield* Fiber.join(fiber)).toMatchObject({
      timedOut: false,
      run: { id: requeuedId, status: "completed" },
    });
  }),
);

it.effect("waitForThread can wait past an ended run for the thread's delegated work", () =>
  Effect.gen(function* () {
    const projectId = ProjectId.make("project:thread-management:wait-delegated");
    const threadId = ThreadId.make("thread:thread-management:wait-delegated");
    const runId = RunId.make("run:thread-management:wait-delegated");
    const wakeRunId = RunId.make("run:thread-management:wait-delegated:wake");
    const subscribed = yield* Deferred.make<void>();
    const events = yield* Queue.unbounded<OrchestrationV2StoredEvent>();
    const at = (seconds: number) => DateTime.makeUnsafe(seconds * 1_000);
    const run = (id: RunId, ordinal: number, status: OrchestrationV2Run["status"], end: number) =>
      ({
        id,
        ordinal,
        status,
        startedAt: at(end - 1),
        completedAt: status === "running" ? null : at(end),
      }) as unknown as OrchestrationV2Run;
    // The run ended, but a task it delegated still runs and will wake the thread.
    let runs = [run(runId, 1, "completed", 1)];
    let subagents = [{ status: "running" }];
    // The wake's turn-end capture gives up, which records no event.
    const captureAwaited = yield* Deferred.make<void>();
    const captureGaveUp = yield* Deferred.make<void>();
    const stored = (sequence: number, event: object) =>
      ({ sequence, event: { threadId, ...event } }) as unknown as OrchestrationV2StoredEvent;
    const layerTest = ThreadManagementService.layer.pipe(
      Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
      Layer.provide(
        Layer.mock(Orchestrator.OrchestratorV2)({
          getThreadEventSequence: () => Effect.succeed(0),
          restartContinuationPending: () => Effect.succeed(false),
          delegatedTaskResultPending: () =>
            Deferred.isDone(captureGaveUp).pipe(Effect.map((done) => !done)),
          awaitLatestRunCapture: () =>
            Deferred.succeed(captureAwaited, undefined).pipe(
              Effect.andThen(Deferred.await(captureGaveUp)),
            ),
          streamStoredEventsFrom: (input) =>
            input?.eventType === "subagent.updated"
              ? Stream.fromEffect(Deferred.succeed(subscribed, undefined)).pipe(
                  Stream.drain,
                  Stream.concat(Stream.fromQueue(events)),
                )
              : Stream.never,
          getThreadRecords: () =>
            Effect.sync(
              () =>
                ({
                  thread: { id: threadId, projectId, deletedAt: null },
                  runs,
                  messages: [],
                  subagents,
                  providerThreads: [],
                }) as unknown as OrchestrationV2ThreadProjection,
            ),
        }),
      ),
      Layer.provide(ServerSettings.layerTest()),
      Layer.provide(layerReceiptsTest),
    );
    const service = yield* ThreadManagementService.ThreadManagementService.pipe(
      Effect.provide(layerTest),
    );
    // Without the option, the ended run is the answer, as before.
    expect(
      yield* service.waitForThread({ projectId, threadId, runId, timeoutMs: 60 * 60 * 1_000 }),
    ).toMatchObject({ timedOut: false, run: { id: runId } });
    expect(
      yield* service.waitForThread({ projectId, threadId, runId, timeoutMs: 60 * 60 * 1_000 }),
    ).not.toHaveProperty("resultRun");

    const fiber = yield* service
      .waitForThread({
        projectId,
        threadId,
        runId,
        timeoutMs: 60 * 60 * 1_000,
        untilDelegatedWorkSettled: true,
      })
      .pipe(Effect.forkChild);
    // A wait that returns early fails below instead of hanging here.
    yield* Effect.raceFirst(Deferred.await(subscribed), Fiber.await(fiber));
    // The task finishes; its delivery wakes the thread, whose next run answers.
    subagents = [{ status: "completed" }];
    runs = [...runs, run(wakeRunId, 2, "completed", 3)];
    yield* Queue.offer(events, stored(1, { type: "subagent.updated", payload: {} }));
    yield* Effect.raceFirst(Deferred.await(captureAwaited), Fiber.await(fiber));
    yield* Deferred.succeed(captureGaveUp, undefined);
    const result = yield* Fiber.join(fiber);

    expect(result).toMatchObject({
      timedOut: false,
      run: { id: runId, status: "completed" },
      resultRun: { id: wakeRunId, status: "completed" },
    });
  }),
);

it.effect(
  "a turn that ended without its result is waited past, and a later turn answers for it",
  () =>
    Effect.gen(function* () {
      const projectId = ProjectId.make("project:thread-management:owed");
      const threadId = ThreadId.make("thread:thread-management:owed");
      const runId = RunId.make("run:thread-management:owed");
      const wakeRunId = RunId.make("run:thread-management:owed:wake");
      const subscribed = yield* Deferred.make<void>();
      const events = yield* Queue.unbounded<OrchestrationV2StoredEvent>();
      const outputSchema = { type: "object", required: ["head"] };
      const run = (id: RunId, ordinal: number, fields: object) =>
        ({ id, ordinal, ...fields }) as unknown as OrchestrationV2Run;
      // The agent ended its turn to wait for something, without the result it was asked for.
      let runs = [run(runId, 1, { status: "completed", outputSchema })];
      const submitted: Array<unknown> = [];
      const stored = (sequence: number, event: object) =>
        ({ sequence, event: { threadId, ...event } }) as unknown as OrchestrationV2StoredEvent;
      const layerTest = ThreadManagementService.layer.pipe(
        Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
        Layer.provide(
          Layer.mock(Orchestrator.OrchestratorV2)({
            getThreadEventSequence: () => Effect.succeed(0),
            restartContinuationPending: () => Effect.succeed(false),
            streamStoredEventsFrom: (input) =>
              input?.eventType === "run.output-submitted"
                ? Stream.fromEffect(Deferred.succeed(subscribed, undefined)).pipe(
                    Stream.drain,
                    Stream.concat(Stream.fromQueue(events)),
                  )
                : Stream.never,
            getThreadRecords: () =>
              Effect.sync(
                () =>
                  ({
                    thread: { id: threadId, projectId, deletedAt: null },
                    runs,
                    messages: [],
                  }) as unknown as OrchestrationV2ThreadProjection,
              ),
            dispatch: (command) =>
              Effect.sync(() => {
                submitted.push(command);
                return { sequence: 1 } as never;
              }),
          }),
        ),
        Layer.provide(ServerSettings.layerTest()),
        Layer.provide(layerReceiptsTest),
      );
      // Built in the test's scope, so the receipt store stays open while the service runs.
      const service = Context.get(
        yield* Layer.build(layerTest),
        ThreadManagementService.ThreadManagementService,
      );
      const wait = { projectId, threadId, runId, timeoutMs: 60 * 60 * 1_000 };
      // Without the option the ended turn is the answer, as before.
      expect(yield* service.waitForThread(wait)).toMatchObject({ timedOut: false });
      // A turn asked for nothing owes nothing, so the option does not hold it.
      const plainRunId = RunId.make("run:thread-management:owed:plain");
      runs = [run(plainRunId, 0, { status: "completed" }), ...runs];
      expect(
        yield* Effect.raceFirst(
          service.waitForThread({ ...wait, runId: plainRunId, untilOutput: true }),
          Effect.as(Deferred.await(subscribed), "waited for a result"),
        ),
      ).toMatchObject({ timedOut: false, run: { id: plainRunId } });
      runs = runs.slice(1);

      const fiber = yield* service
        .waitForThread({ ...wait, untilOutput: true })
        .pipe(Effect.forkChild);
      yield* Effect.raceFirst(Deferred.await(subscribed), Fiber.await(fiber));
      // Something wakes the thread. The turn it starts was asked for nothing, and submits anyway.
      // A request queued behind it asks for a result of its own, and is not the one owed.
      const queuedRunId = RunId.make("run:thread-management:owed:queued");
      runs = [
        ...runs,
        run(wakeRunId, 2, { status: "running" }),
        run(queuedRunId, 3, { status: "queued", outputSchema }),
      ];
      const accepted = yield* service.submitRunOutput({
        threadId,
        commandId: CommandId.make("command:thread-management:owed"),
        json: '{"head":"abc"}',
      });
      expect(accepted.runId).toBe(runId);
      expect(submitted).toMatchObject([
        { type: "run.output.submit", runId, value: { head: "abc" } },
      ]);
      const output = { value: { head: "abc" }, submittedAt: "2026-01-01T00:00:00.000Z" };
      runs = [run(runId, 1, { status: "completed", outputSchema, output }), runs[1]!, runs[2]!];
      yield* Queue.offer(events, stored(1, { type: "run.output-submitted", payload: {} }));
      expect(yield* Fiber.join(fiber)).toMatchObject({
        timedOut: false,
        run: { id: runId, output },
      });

      // The result is given once: a later plain turn has nothing to submit to.
      const again = yield* service
        .submitRunOutput({
          threadId,
          commandId: CommandId.make("command:thread-management:owed:again"),
          json: '{"head":"def"}',
        })
        .pipe(Effect.flip);
      expect(again.message).toContain("not asked for a result");
    }).pipe(Effect.scoped),
);

it.effect("waitForThread settles delegated work on a thread rolled back past its only run", () =>
  Effect.gen(function* () {
    const projectId = ProjectId.make("project:thread-management:wait-rolled-back");
    const threadId = ThreadId.make("thread:thread-management:wait-rolled-back");
    const runId = RunId.make("run:thread-management:wait-rolled-back");
    const rolledBack = {
      id: runId,
      ordinal: 1,
      status: "rolled_back",
      startedAt: DateTime.makeUnsafe(0),
      completedAt: DateTime.makeUnsafe(1_000),
    } as unknown as OrchestrationV2Run;
    const layerTest = ThreadManagementService.layer.pipe(
      Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
      Layer.provide(
        Layer.mock(Orchestrator.OrchestratorV2)({
          getThreadEventSequence: () => Effect.succeed(0),
          restartContinuationPending: () => Effect.succeed(false),
          delegatedTaskResultPending: () => Effect.succeed(false),
          streamStoredEventsFrom: () => Stream.never,
          getThreadRecords: () =>
            Effect.succeed({
              thread: { id: threadId, projectId, deletedAt: null },
              runs: [rolledBack],
              messages: [],
              subagents: [],
              providerThreads: [],
            } as unknown as OrchestrationV2ThreadProjection),
        }),
      ),
      Layer.provide(ServerSettings.layerTest()),
      Layer.provide(layerReceiptsTest),
    );
    const service = yield* ThreadManagementService.ThreadManagementService.pipe(
      Effect.provide(layerTest),
    );
    const fiber = yield* service
      .waitForThread({
        projectId,
        threadId,
        runId,
        timeoutMs: 60_000,
        untilDelegatedWorkSettled: true,
      })
      .pipe(Effect.forkChild);
    // Nothing will ever run there again, so a wait still open runs out here.
    yield* TestClock.adjust(Duration.millis(60_000));
    const result = yield* Fiber.join(fiber);

    expect(result).toMatchObject({
      timedOut: false,
      run: { id: runId, status: "rolled_back" },
      resultRun: { id: runId, status: "rolled_back" },
    });
  }),
);

it.effect("waitForThread waits for output only from a run that was asked for one", () =>
  Effect.gen(function* () {
    const projectId = ProjectId.make("project:thread-management:wait-output");
    const threadId = ThreadId.make("thread:thread-management:wait-output");
    const askedRunId = RunId.make("run:thread-management:wait-output:asked");
    const plainRunId = RunId.make("run:thread-management:wait-output:plain");
    const layerTest = ThreadManagementService.layer.pipe(
      Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
      Layer.provide(
        Layer.mock(Orchestrator.OrchestratorV2)({
          getThreadEventSequence: () => Effect.succeed(0),
          restartContinuationPending: () => Effect.succeed(false),
          streamStoredEventsFrom: () => Stream.never,
          getThreadRecords: () =>
            Effect.succeed({
              thread: { id: threadId, projectId, deletedAt: null },
              runs: [
                { id: askedRunId, status: "completed", outputSchema: { type: "object" } },
                { id: plainRunId, status: "completed" },
              ],
            } as unknown as OrchestrationV2ThreadProjection),
        }),
      ),
      Layer.provide(ServerSettings.layerTest()),
      Layer.provide(layerReceiptsTest),
    );
    const service = yield* ThreadManagementService.ThreadManagementService.pipe(
      Effect.provide(layerTest),
    );
    const wait = (runId: RunId) =>
      service
        .waitForThread({ projectId, threadId, runId, timeoutMs: 60_000, untilOutput: true })
        .pipe(Effect.forkChild);
    const asked = yield* wait(askedRunId);
    const plain = yield* wait(plainRunId);
    yield* TestClock.adjust(Duration.millis(60_000));

    // A later turn may still submit the result the first run owes.
    expect(yield* Fiber.join(asked)).toMatchObject({ timedOut: true, run: { id: askedRunId } });
    // Nothing can submit one for a run that never asked, so its end is the answer.
    expect(yield* Fiber.join(plain)).toMatchObject({
      timedOut: false,
      run: { id: plainRunId, status: "completed" },
    });
  }),
);

it.effect("waitForThread returns the cut run once its continuation is declined", () =>
  Effect.gen(function* () {
    const projectId = ProjectId.make("project:thread-management:wait-declined");
    const threadId = ThreadId.make("thread:thread-management:wait-declined");
    const runId = RunId.make("run:thread-management:wait-declined");
    const subscribed = yield* Deferred.make<void>();
    const declined = yield* Deferred.make<void>();
    let pending = true;
    const layerTest = ThreadManagementService.layer.pipe(
      Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
      Layer.provide(
        Layer.mock(Orchestrator.OrchestratorV2)({
          getThreadEventSequence: () => Effect.succeed(0),
          restartContinuationPending: () => Effect.succeed(pending),
          awaitRestartContinuation: () => Deferred.await(declined),
          streamStoredEventsFrom: (input) =>
            input?.eventType === "run.updated"
              ? Stream.fromEffect(Deferred.succeed(subscribed, undefined)).pipe(
                  Stream.drain,
                  Stream.concat(Stream.never),
                )
              : Stream.never,
          getThreadRecords: () =>
            Effect.succeed({
              thread: { id: threadId, projectId, deletedAt: null },
              runs: [{ id: runId, status: "cancelled" }],
            } as unknown as OrchestrationV2ThreadProjection),
        }),
      ),
      Layer.provide(ServerSettings.layerTest()),
      Layer.provide(layerReceiptsTest),
    );
    const service = yield* ThreadManagementService.ThreadManagementService.pipe(
      Effect.provide(layerTest),
    );
    const fiber = yield* service
      .waitForThread({ projectId, threadId, runId, timeoutMs: 60 * 60 * 1_000 })
      .pipe(Effect.forkChild);
    yield* Deferred.await(subscribed);
    pending = false;
    yield* Deferred.succeed(declined, undefined);
    const result = yield* Fiber.join(fiber);

    expect(result).toMatchObject({ timedOut: false, run: { id: runId, status: "cancelled" } });
  }),
);

it.effect.each([
  "live",
  "no-match",
  "no-match-clear",
  "no-session",
  "unsupported-runtime",
] as const)(
  "marks background work awaited on the thread's active provider thread with state %s",
  (state) =>
    Effect.gen(function* () {
      const threadId = ThreadId.make("thread:background-wait");
      const providerThreadId = ProviderThreadId.make("provider-thread:background-wait");
      const providerSessionId = ProviderSessionId.make("provider-session:background-wait");
      const driver = ProviderDriverKind.make("claudeAgent");
      const now = yield* DateTime.now;
      const providerThread: OrchestrationV2ProviderThread = {
        id: providerThreadId,
        driver,
        providerInstanceId: ProviderInstanceId.make("claudeAgent"),
        providerSessionId,
        appThreadId: threadId,
        ownerNodeId: null,
        nativeThreadRef: { driver, nativeId: "native-thread", strength: "strong" },
        nativeConversationHeadRef: null,
        status: "idle",
        firstRunOrdinal: 1,
        lastRunOrdinal: 1,
        handoffIds: [],
        forkedFrom: null,
        pendingBackgroundTasks: [{ taskId: "tests", kind: "command" }],
        contextUsage: null,
        nativeMetadata: null,
        createdAt: now,
        updatedAt: now,
      };
      const calls: Array<{ providerThreadId: ProviderThreadId; awaited: boolean }> = [];
      const runtime = {
        driver,
        providerSessionId,
        ...(state === "unsupported-runtime"
          ? {}
          : {
              awaitBackgroundTasks: (input: {
                readonly providerThread: OrchestrationV2ProviderThread;
                readonly taskIds: ReadonlyArray<string>;
                readonly awaited: boolean;
              }) =>
                Effect.sync(() => {
                  calls.push({
                    providerThreadId: input.providerThread.id,
                    awaited: input.awaited,
                  });
                  return { matched: input.taskIds.filter((taskId) => taskId === "tests") };
                }),
            }),
      } as unknown as ProviderAdapter.ProviderAdapterV2SessionRuntime;
      const layerTest = ThreadManagementService.layer.pipe(
        Layer.provide(
          Layer.mergeAll(
            Layer.mock(Orchestrator.OrchestratorV2)({
              getThreadRecords: () =>
                Effect.succeed({
                  thread: {
                    id: threadId,
                    deletedAt: null,
                    activeProviderThreadId: providerThreadId,
                  },
                  providerThreads: [providerThread],
                } as unknown as OrchestrationV2ThreadProjection),
            }),
            Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({
              get: () =>
                Effect.succeed(state === "no-session" ? Option.none() : Option.some(runtime)),
            }),
          ),
        ),
        Layer.provide(ServerSettings.layerTest()),
        Layer.provide(layerReceiptsTest),
      );
      const result = yield* Effect.gen(function* () {
        const service = yield* ThreadManagementService.ThreadManagementService;
        return yield* service
          .awaitBackgroundTasks({
            threadId,
            taskIds: state.startsWith("no-match") ? ["finished"] : ["tests", "finished"],
            awaited: state !== "no-match-clear",
          })
          .pipe(Effect.result);
      }).pipe(Effect.provide(Layer.merge(layerTest, ThreadCommandExecutor.layer)));

      if (state === "live") {
        expect(result).toMatchObject({
          success: { matched: ["tests"], unmatched: ["finished"] },
        });
        expect(calls).toEqual([{ providerThreadId, awaited: true }]);
      } else if (state === "no-match") {
        expect(result).toMatchObject({
          failure: { _tag: "ThreadManagementBackgroundWaitError", reason: "unmatched" },
        });
        expect(calls).toEqual([{ providerThreadId, awaited: true }]);
      } else if (state === "no-match-clear") {
        expect(result).toMatchObject({ success: { matched: [], unmatched: ["finished"] } });
        expect(calls).toEqual([{ providerThreadId, awaited: false }]);
      } else {
        expect(result).toMatchObject({
          failure: {
            _tag: "ThreadManagementBackgroundWaitError",
            reason: state === "no-session" ? "unavailable" : "unsupported",
          },
        });
        expect(calls).toEqual([]);
      }
    }),
);

it("an unmatched background wait says Workflow IDs already keep the thread working", () => {
  const error = new ThreadManagementService.ThreadManagementBackgroundWaitError({
    threadId: ThreadId.make("thread:thread-management:background-wait"),
    reason: "unmatched",
  });
  expect(error.message).toContain("Workflow");
});

it.effect.each([
  { status: "completed" as const, followUp: "none", settles: true },
  { status: "failed" as const, followUp: "none", settles: false },
  { status: "interrupted" as const, followUp: "none", settles: false },
  { status: "completed" as const, followUp: "notification", settles: true },
  { status: "completed" as const, followUp: "replayed-notification", settles: true },
  { status: "completed" as const, followUp: "delegated", settles: true },
  { status: "completed" as const, followUp: "failed-notification", settles: false },
  { status: "completed" as const, followUp: "held-notification", settles: true },
  { status: "completed" as const, followUp: "user", settles: false },
  { status: "completed" as const, followUp: "steered-notification", settles: false },
  // A notification steered in unread; requeueUnreadSteers closes it and runs its wake.
  { status: "completed" as const, followUp: "unread-notification", settles: true },
  // Send now cancels the queued run; its message joins the run that asked to settle.
  { status: "completed" as const, followUp: "promoted-to-steer", settles: true },
  // A delegated-completion wake cancelled unstarted because the turn already took the result.
  { status: "completed" as const, followUp: "cancelled-delegated", settles: true },
  { status: "completed" as const, followUp: "stopped-delegated", settles: false },
  // Work the settle blocker waits on can end with no run update.
  { status: "completed" as const, followUp: "subagent-finished", settles: true },
  { status: "completed" as const, followUp: "background-task-finished", settles: true },
  { status: "completed" as const, followUp: "approval-answered", settles: true },
])("settleAfterRun handles $status with $followUp follow-up", ({ status, followUp, settles }) =>
  Effect.gen(function* () {
    const projectId = ProjectId.make("project:thread-management:settle-after-run");
    const threadId = ThreadId.make("thread:thread-management:settle-after-run");
    const runId = RunId.make("run:thread-management:settle-after-run");
    const followUpId = RunId.make(`${runId}:follow-up`);
    const subscribed = yield* Deferred.make<void>();
    const events = yield* Queue.unbounded<OrchestrationV2StoredEvent>();
    const providerThreadEvents = yield* Queue.unbounded<OrchestrationV2StoredEvent>();
    const turnItemEvents = yield* Queue.unbounded<OrchestrationV2StoredEvent>();
    const requestEvents = yield* Queue.unbounded<OrchestrationV2StoredEvent>();
    const replayRejected = yield* Deferred.make<void>();
    const refusedForBlocker = yield* Deferred.make<void>();
    const blocked = followUp.endsWith("-finished") || followUp.endsWith("-answered");
    let blocking = blocked;
    let sequence = 0;
    let sequenceReads = 0;
    let rejectedCommandId: CommandId | undefined;
    const runs = [{ id: runId, ordinal: 1, status, userMessageId: MessageId.make("original") }];
    const nextRun = {
      id: followUpId,
      ordinal: 2,
      status: "running" as OrchestrationV2Run["status"],
      userMessageId: MessageId.make("follow-up"),
      startedAt: followUp === "stopped-delegated" ? "2026-01-01T00:00:00.000Z" : null,
    };
    const dispatched: Array<string> = [];
    // The notification starts after the completed run was observed, before settlement commits.
    let raced = false;
    let unreadNotification = followUp === "unread-notification";
    const layerTest = ThreadManagementService.layer.pipe(
      Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
      Layer.provide(
        Layer.mock(Orchestrator.OrchestratorV2)({
          getThreadEventSequence: () =>
            Effect.sync(() => {
              if (followUp === "replayed-notification" && sequenceReads++ === 2) sequence = 1;
              return sequence;
            }),
          restartContinuationPending: () => Effect.succeed(false),
          streamStoredEventsFrom: (input) =>
            Stream.fromEffect(Deferred.succeed(subscribed, undefined)).pipe(
              Stream.drain,
              Stream.concat(
                Stream.fromQueue(
                  input?.eventType === "provider-thread.updated"
                    ? providerThreadEvents
                    : input?.eventType === "turn-item.updated"
                      ? turnItemEvents
                      : input?.eventType === "runtime-request.updated"
                        ? requestEvents
                        : events,
                ),
              ),
            ),
          getThreadRecords: (_threadId, _fields, filter) =>
            Effect.sync(
              () =>
                ({
                  thread: { id: threadId, projectId, deletedAt: null },
                  runs: raced ? [...runs, nextRun] : runs,
                  turnItems: [
                    {
                      runId,
                      type: "notification" as const,
                      status: unreadNotification ? "pending" : "cancelled",
                    },
                  ].filter(
                    (item) =>
                      followUp === "unread-notification" &&
                      (filter?.turnItemTypes === undefined ||
                        filter.turnItemTypes.includes(item.type)),
                  ),
                  messages: [
                    { id: MessageId.make("original"), runId, role: "user" as const },
                    {
                      id: nextRun.userMessageId,
                      runId: followUp === "promoted-to-steer" ? runId : followUpId,
                      role: "user" as const,
                      ...(followUp.endsWith("notification") ? { notification: {} } : {}),
                      ...(followUp.endsWith("delegated") ? { delegatedCompletion: {} } : {}),
                    },
                    { id: MessageId.make("reply"), runId: followUpId, role: "assistant" as const },
                    ...(followUp === "steered-notification"
                      ? [
                          {
                            id: MessageId.make("user-steer"),
                            runId: followUpId,
                            role: "user" as const,
                          },
                        ]
                      : []),
                  ].filter(
                    (message) =>
                      (filter?.messageIds === undefined ||
                        filter.messageIds.includes(message.id)) &&
                      (filter?.messageRunIds === undefined ||
                        filter.messageRunIds.includes(message.runId)) &&
                      (filter?.messageRoles === undefined ||
                        filter.messageRoles.includes(message.role)),
                  ),
                }) as unknown as OrchestrationV2ThreadProjection,
            ),
          dispatch: (command) =>
            Effect.gen(function* () {
              if (blocking) {
                yield* Deferred.succeed(refusedForBlocker, undefined);
                return yield* new Orchestrator.OrchestratorDispatchError({
                  commandId: command.commandId,
                  commandType: command.type,
                  cause: "Thread is waiting on background work or an approval",
                });
              }
              if (followUp !== "none" && followUp !== "unread-notification" && !blocked && !raced) {
                raced = true;
                rejectedCommandId = command.commandId;
                return yield* new Orchestrator.OrchestratorDispatchError({
                  commandId: command.commandId,
                  commandType: command.type,
                  cause: "Thread is still running",
                });
              }
              if (followUp === "replayed-notification" && command.commandId === rejectedCommandId) {
                yield* Deferred.succeed(replayRejected, undefined);
                return yield* new Orchestrator.OrchestratorCommandPreviouslyRejectedError({
                  commandId: command.commandId,
                  commandType: command.type,
                  detail: "Thread is still running",
                });
              }
              dispatched.push(command.type);
              return { sequence: 1, storedEvents: [] } as never;
            }),
        }),
      ),
      Layer.provide(ServerSettings.layerTest()),
      Layer.provide(layerReceiptsTest),
    );
    const service = yield* ThreadManagementService.ThreadManagementService.pipe(
      Effect.provide(layerTest),
    );
    const fiber = yield* service
      .settleAfterRun({ projectId, threadId, runId })
      .pipe(Effect.forkChild);
    if (blocked) {
      yield* Deferred.await(refusedForBlocker);
      blocking = false;
      sequence = 1;
      yield* followUp === "subagent-finished"
        ? Queue.offer(turnItemEvents, {
            sequence,
            event: {
              type: "turn-item.updated",
              threadId,
              payload: { type: "subagent", status: "completed" },
            },
          } as unknown as OrchestrationV2StoredEvent)
        : followUp === "approval-answered"
          ? Queue.offer(requestEvents, {
              sequence,
              event: {
                type: "runtime-request.updated",
                threadId,
                payload: { id: "request:settle-after-run", status: "resolved" },
              },
            } as unknown as OrchestrationV2StoredEvent)
          : Queue.offer(providerThreadEvents, {
              sequence,
              event: {
                type: "provider-thread.updated",
                threadId,
                payload: { id: "provider-thread:settle-after-run", pendingBackgroundTasks: [] },
              },
            } as unknown as OrchestrationV2StoredEvent);
    }
    if (followUp === "unread-notification") {
      yield* Effect.raceFirst(Deferred.await(subscribed), Fiber.await(fiber));
      expect(dispatched).toEqual([]);
      unreadNotification = false;
      raced = true;
      nextRun.status = "completed";
      sequence = 1;
      yield* Queue.offer(turnItemEvents, {
        sequence,
        event: {
          type: "turn-item.updated",
          threadId,
          payload: { type: "notification", status: "cancelled" },
        },
      } as unknown as OrchestrationV2StoredEvent);
    } else if (followUp !== "none" && !blocked) {
      yield* Deferred.await(subscribed);
      expect(dispatched).toEqual([]);
      nextRun.status =
        followUp === "failed-notification"
          ? "failed"
          : followUp === "held-notification"
            ? "queued"
            : followUp === "promoted-to-steer" || followUp.endsWith("ed-delegated")
              ? "cancelled"
              : "completed";
      yield* Queue.offer(events, {
        sequence: 1,
        event: { type: "run.updated", threadId, payload: nextRun },
      } as unknown as OrchestrationV2StoredEvent);
    }
    if (followUp === "replayed-notification") {
      yield* Deferred.await(replayRejected);
      expect(dispatched).toEqual([]);
      sequence = 2;
      yield* Queue.offer(events, {
        sequence,
        event: { type: "run.updated", threadId, payload: nextRun },
      } as unknown as OrchestrationV2StoredEvent);
    }
    yield* Fiber.join(fiber);
    expect(dispatched).toEqual(settles ? ["thread.settle"] : []);
  }),
);

// The agent reads a message steered in after it asked to settle and ends on a question, or never
// reads it and requeueUnreadSteers runs it next.
it.effect.each([
  { type: "user_message", inputIntent: "steer", read: true },
  { type: "user_message", inputIntent: "promoted_queued_to_steer", read: true },
  { type: "user_message", inputIntent: "steer", read: false },
  { type: "user_message", inputIntent: "promoted_queued_to_steer", read: false },
] as const)(
  "settleAfterRun keeps the thread open after a $inputIntent (read: $read) once the settle was requested",
  ({ type, inputIntent, read }) =>
    Effect.gen(function* () {
      const projectId = ProjectId.make("project:thread-management:settle-after-steer");
      const threadId = ThreadId.make("thread:thread-management:settle-after-steer");
      const runId = RunId.make("run:thread-management:settle-after-steer");
      yield* TestClock.adjust(Duration.millis(60_000));
      const dispatched: Array<string> = [];
      const layerTest = ThreadManagementService.layer.pipe(
        Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
        Layer.provide(
          Layer.mock(Orchestrator.OrchestratorV2)({
            getThreadEventSequence: () => Effect.succeed(0),
            restartContinuationPending: () => Effect.succeed(false),
            streamStoredEventsFrom: () => Stream.empty,
            getThreadRecords: (_threadId, _fields, filter) =>
              Effect.succeed({
                thread: { id: threadId, projectId, deletedAt: null },
                runs: [{ id: runId, ordinal: 1, status: "completed", userMessageId: "original" }],
                messages: [
                  { id: "original", runId, role: "user" },
                  { id: "steered", runId, role: "user" },
                ],
                turnItems: [
                  {
                    runId,
                    type,
                    ...(type === "user_message" ? { inputIntent } : {}),
                    status: read ? "completed" : "pending",
                    createdAt: "1970-01-01T00:00:30.000Z",
                    completedAt: read ? "1970-01-01T00:01:30.000Z" : null,
                  },
                ].filter(
                  (item) =>
                    filter?.turnItemTypes === undefined || filter.turnItemTypes.includes(item.type),
                ),
              } as unknown as OrchestrationV2ThreadProjection),
            dispatch: (command) =>
              Effect.sync(() => {
                dispatched.push(command.type);
                return { sequence: 1, storedEvents: [] } as never;
              }),
          }),
        ),
        Layer.provide(ServerSettings.layerTest()),
        Layer.provide(layerReceiptsTest),
      );
      const service = yield* ThreadManagementService.ThreadManagementService.pipe(
        Effect.provide(layerTest),
      );
      yield* service.settleAfterRun({ projectId, threadId, runId });
      expect(dispatched).toEqual([]);
    }),
);

const forceStopSibling = "thread:thread-management:force-stop:other";
it.effect.each([
  {
    name: "kills the thread's own agent and ends its turn",
    killed: true,
    expected: { outcome: "killed", sharedWithThreadIds: [] },
  },
  {
    name: "reports a session torn down without an agent of its own as released",
    expected: { outcome: "released", sharedWithThreadIds: [] },
  },
  {
    name: "keeps a process another working thread shares and only ends this turn",
    sharedWith: [forceStopSibling],
    expected: { outcome: "shared", sharedWithThreadIds: [forceStopSibling] },
  },
  {
    name: "still settles a finished run's background work in a shared process",
    sharedWith: [forceStopSibling],
    settledRun: true,
    expected: { outcome: "shared", sharedWithThreadIds: [forceStopSibling] },
  },
  {
    name: "reports a teardown that has not finished in time as still releasing",
    teardownHangs: true,
    expected: { outcome: "releasing", sharedWithThreadIds: [] },
  },
  {
    name: "lets the retired sessions close when the settlement fails",
    settleFails: true,
    expected: "failed",
  },
  {
    name: "still settles when its caller goes away after retiring",
    callerGoesAway: true,
    expected: "interrupted",
  },
])("Force stop $name", ({ expected, ...row }) =>
  Effect.gen(function* () {
    const threadId = ThreadId.make("thread:thread-management:force-stop");
    const instanceId = ProviderInstanceId.make("codex");
    // A provider hosting several threads in one process runs them on its instance's shared session.
    const providerSessionId = IdAllocator.derive.providerSession({
      providerInstanceId: instanceId,
    });
    const providerThreadId = ProviderThreadId.make("provider-thread:force-stop");
    const runId = RunId.make("run:thread-management:force-stop");
    const sharedWith = "sharedWith" in row ? row.sharedWith : undefined;
    const order: Array<string> = [];
    const dispatched: Array<OrchestrationV2ServerCommand> = [];
    const retiredProviderThreadIds: Array<ProviderThreadId> = [];
    const afterCompleted = yield* Deferred.make<void>();
    const retired = yield* Deferred.make<void>();
    const interruptSent = yield* Deferred.make<void>();
    const layerTest = ThreadManagementService.layer.pipe(
      Layer.provide(
        Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({
          retireThread: (input) =>
            Effect.gen(function* () {
              order.push("retire");
              retiredProviderThreadIds.push(...input.ownProviderThreadIds);
              if ("callerGoesAway" in row) {
                yield* Deferred.succeed(retired, undefined);
                yield* Deferred.await(interruptSent);
              }
              // Started at once: it gets past `after` here only if that is already complete.
              yield* input.after.pipe(
                Effect.andThen(Effect.sync(() => order.push("after"))),
                Effect.andThen(Deferred.succeed(afterCompleted, undefined)),
                Effect.forkDetach({ startImmediately: true }),
              );
              return {
                shared: sharedWith !== undefined,
                sharedWithThreadIds: (sharedWith ?? []).map((id) => ThreadId.make(id)),
                teardown: "teardownHangs" in row ? Effect.never : Deferred.await(afterCompleted),
              };
            }),
        }),
      ),
      Layer.provide(
        Layer.mock(Orchestrator.OrchestratorV2)({
          getThreadRecords: () =>
            Effect.succeed({
              thread: {
                id: threadId,
                deletedAt: null,
                modelSelection: { instanceId, model: "gpt-5.4" },
                activeProviderThreadId: providerThreadId,
              },
              runs: [
                {
                  id: runId,
                  ordinal: 1,
                  status: "settledRun" in row ? "completed" : "running",
                  activeAttemptId: "attempt",
                  providerThreadId,
                },
                // A follow-up that never ran neither supersedes the request nor becomes the target.
                {
                  id: RunId.make("run:queued-follow-up"),
                  ordinal: 2,
                  status: "queued",
                  providerThreadId,
                },
                {
                  id: RunId.make("run:steered-follow-up"),
                  ordinal: 3,
                  status: "cancelled",
                  startedAt: null,
                  providerThreadId,
                },
              ],
              providerThreads: [
                { id: providerThreadId, appThreadId: threadId, providerSessionId },
                // A native subagent's thread is read with its parent but is its child's own.
                {
                  id: ProviderThreadId.make("provider-thread:force-stop:native-child"),
                  appThreadId: ThreadId.make("thread:thread-management:force-stop:child"),
                  providerSessionId,
                },
              ],
              providerTurns: [
                {
                  id: "provider-turn:force-stop",
                  providerThreadId,
                  runAttemptId: "attempt",
                  status: "settledRun" in row ? "completed" : "running",
                },
              ],
            } as unknown as OrchestrationV2ThreadProjection),
          dispatch: (command) =>
            Effect.suspend(() => {
              order.push("dispatch");
              dispatched.push(command);
              return "settleFails" in row
                ? Effect.fail(
                    new Orchestrator.OrchestratorDispatchError({
                      commandId: command.commandId,
                      commandType: command.type,
                      cause: "simulated",
                    }),
                  )
                : Effect.succeed({ sequence: 1, storedEvents: [] } as never);
            }),
        }),
      ),
      Layer.provide(
        Layer.succeed(AgentScope, {
          ...AgentScope.defaultValue(),
          kill: () => Effect.succeed("killed" in row),
        }),
      ),
      Layer.provide(ServerSettings.layerTest()),
      Layer.provide(layerReceiptsTest),
    );
    yield* Effect.gen(function* () {
      const service = yield* ThreadManagementService.ThreadManagementService;
      const stopping = yield* service
        .forceStopThread({ threadId, runId })
        .pipe(Effect.result, Effect.forkChild);
      if ("callerGoesAway" in row) {
        yield* Deferred.await(retired);
        yield* Fiber.interrupt(stopping).pipe(Effect.forkDetach({ startImmediately: true }));
        yield* Deferred.succeed(interruptSent, undefined);
      }
      yield* TestClock.adjust("10 seconds");
      const exit = yield* Fiber.await(stopping);
      expect(
        Exit.isSuccess(exit)
          ? Result.isSuccess(exit.value)
            ? exit.value.success
            : "failed"
          : "interrupted",
      ).toEqual(expected);
      yield* Deferred.await(afterCompleted);
      // The sessions are retired before the settlement can promote a follow-up, and close after it.
      expect(order).toEqual(["retire", "dispatch", "after"]);
      expect(retiredProviderThreadIds).toEqual([providerThreadId]);
      expect(dispatched).toMatchObject([
        {
          type: "thread.background-work.settle",
          providerThreadId,
          reason: "Force stopped by user",
        },
      ]);
    }).pipe(Effect.provide(layerTest));
  }),
);

it.effect("Force stop refuses once a newer run has started, before killing anything", () =>
  Effect.gen(function* () {
    const threadId = ThreadId.make("thread:thread-management:force-stop-superseded");
    let killed = false;
    const layerTest = ThreadManagementService.layer.pipe(
      Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
      Layer.provide(
        Layer.mock(Orchestrator.OrchestratorV2)({
          getThreadRecords: () =>
            Effect.succeed({
              thread: { id: threadId, deletedAt: null },
              runs: [
                { id: RunId.make("run:stopped"), ordinal: 1, status: "running" },
                { id: RunId.make("run:newer"), ordinal: 2, status: "running" },
              ],
              providerThreads: [],
              providerTurns: [],
            } as unknown as OrchestrationV2ThreadProjection),
        }),
      ),
      Layer.provide(
        Layer.succeed(AgentScope, {
          ...AgentScope.defaultValue(),
          kill: () => Effect.sync(() => (killed = true)),
        }),
      ),
      Layer.provide(ServerSettings.layerTest()),
      Layer.provide(layerReceiptsTest),
    );
    const error = yield* Effect.gen(function* () {
      const service = yield* ThreadManagementService.ThreadManagementService;
      return yield* Effect.flip(
        service.forceStopThread({ threadId, runId: RunId.make("run:stopped") }),
      );
    }).pipe(Effect.provide(layerTest));
    expect(error.reason).toBe("superseded");
    expect(killed).toBe(false);
  }),
);

it.effect(
  "admits two transcript exports at a time and frees a slot on failure or interruption",
  () =>
    Effect.gen(function* () {
      const started = yield* Queue.unbounded<string>();
      const gates = new Map(
        ["a", "b", "c", "d", "e", "f"].map((id) => [
          id,
          Deferred.makeUnsafe<void, Orchestrator.OrchestratorV2Error>(),
        ]),
      );
      const layerTest = ThreadManagementService.layer.pipe(
        Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
        Layer.provide(
          Layer.mock(Orchestrator.OrchestratorV2)({
            getTranscriptByteFloor: () => Effect.succeed(0),
            getThreadSnapshot: (threadId) =>
              Queue.offer(started, threadId).pipe(
                Effect.andThen(Deferred.await(gates.get(threadId)!)),
                Effect.as({
                  schemaVersion: 1,
                  snapshotSequence: 1,
                  projection: {
                    thread: { id: threadId, title: threadId },
                    updatedAt: DateTime.makeUnsafe(0),
                    visibleTurnItems: [],
                  } as unknown as OrchestrationV2ThreadProjection,
                }),
              ),
          }),
        ),
        Layer.provide(ServerSettings.layerTest()),
        Layer.provide(layerReceiptsTest),
      );

      yield* Effect.gen(function* () {
        const threads = yield* ThreadManagementService.ThreadManagementService;
        const exportThread = (id: string) =>
          threads.getThreadTranscript(ThreadId.make(id)).pipe(
            Effect.provideService(ServerEnvironment.ServerEnvironmentIdentity, {
              getEnvironmentId: Effect.succeed(EnvironmentId.make("environment:export")),
            }),
            Effect.forkChild({ startImmediately: true }),
          );
        const a = yield* exportThread("a");
        const b = yield* exportThread("b");
        const c = yield* exportThread("c");
        const d = yield* exportThread("d");
        const e = yield* exportThread("e");
        const f = yield* exportThread("f");
        expect(yield* Queue.takeAll(started)).toEqual(["a", "b"]);

        yield* Fiber.interrupt(c);
        yield* Deferred.fail(
          gates.get("a")!,
          new Orchestrator.OrchestratorProjectionError({ threadId: ThreadId.make("a"), cause: "" }),
        );
        expect(yield* Queue.take(started)).toBe("d");
        yield* Fiber.interrupt(b);
        expect(yield* Queue.take(started)).toBe("e");
        yield* Deferred.succeed(gates.get("d")!, undefined);
        expect(yield* Queue.take(started)).toBe("f");
        yield* Deferred.succeed(gates.get("e")!, undefined);
        yield* Deferred.succeed(gates.get("f")!, undefined);

        expect((yield* Fiber.await(a))._tag).toBe("Failure");
        expect((yield* Fiber.join(d)).threadId).toBe("d");
        expect((yield* Fiber.join(e)).threadId).toBe("e");
        expect((yield* Fiber.join(f)).threadId).toBe("f");
        expect(yield* Queue.size(started)).toBe(0);
      }).pipe(Effect.provide(layerTest));
    }),
);

it.effect("client reads of a deleted thread's checkpoints and records find no thread", () =>
  Effect.gen(function* () {
    const live = ThreadId.make("thread:checkpoints:live");
    const deleted = ThreadId.make("thread:checkpoints:deleted");
    const layerTest = ThreadManagementService.layer.pipe(
      Layer.provide(Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({})),
      Layer.provide(
        Layer.mock(Orchestrator.OrchestratorV2)({
          getThreadRecords: (threadId) =>
            Effect.succeed({
              thread: {
                id: threadId,
                deletedAt: threadId === deleted ? DateTime.makeUnsafe(0) : null,
              },
              providerThreads: [],
            } as never),
          getCheckpointContext: () =>
            Effect.succeed({ runs: [], checkpointScopes: [], checkpoints: [] }),
        }),
      ),
      Layer.provide(ServerSettings.layerTest()),
      Layer.provide(layerReceiptsTest),
    );
    yield* Effect.gen(function* () {
      const threads = yield* ThreadManagementService.ThreadManagementService;
      expect((yield* threads.getCheckpointContext(live)).checkpoints).toEqual([]);
      expect((yield* threads.getLiveThreadRecords(live, ["providerThreads"])).thread.id).toBe(live);
      for (const read of [
        Effect.asVoid(threads.getCheckpointContext(deleted)),
        Effect.asVoid(threads.getLiveThreadRecords(deleted, ["providerThreads"])),
      ]) {
        expect(yield* Effect.flip(read)).toMatchObject({
          _tag: "OrchestratorProjectionError",
          cause: { _tag: "ProjectionStoreThreadNotFoundError" },
        });
      }
    }).pipe(Effect.provide(layerTest));
  }),
);
