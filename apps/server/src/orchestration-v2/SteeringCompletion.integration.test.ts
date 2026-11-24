import { makeProviderFailure } from "@onicode/provider-core/server/failure";
import { assert, it, vi } from "@effect/vitest";
import * as TestClock from "effect/testing/TestClock";
import {
  ChatAttachmentId,
  COMPOSER_CONTEXT_TERMINAL_TEXT_MAX_CHARS,
  ComposerContextId,
  CommandId,
  EnvironmentId,
  MessageId,
  EventId,
  NodeId,
  ProjectId,
  ProviderDriverKind,
  ProviderInstanceId,
  ProviderThreadId,
  ProviderTurnId,
  RuntimeRequestId,
  PROVIDER_SEND_TURN_MAX_INPUT_CHARS,
  ScheduledTaskId,
  ThreadId,
  TurnItemId,
  type ModelSelection,
  type OrchestrationV2DomainEvent,
} from "@onicode/contracts";
import {
  expandAssistantCitationsForProvider,
  serializeAssistantCitation,
} from "@onicode/shared/assistantCitations";
import { projectComposerContextForProvider } from "@onicode/shared/composerContextReferences";
import * as DateTime from "effect/DateTime";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Queue from "effect/Queue";
import * as Stream from "effect/Stream";
import { CodexProviderCapabilitiesV2 } from "./Adapters/CodexAdapterV2.ts";
import { DispatchModeLimit } from "./DispatchModeLimit.ts";
import * as EffectWorker from "./EffectWorker.ts";
import * as EffectOutbox from "./EffectOutbox.ts";
import * as EventSink from "./EventSink.ts";
import * as ThreadCommandExecutor from "./ThreadCommandExecutor.ts";
import * as Orchestrator from "./Orchestrator.ts";
import * as ProjectionStore from "./ProjectionStore.ts";
import * as ProviderAdapterRegistry from "./ProviderAdapterRegistry.ts";
import * as ProviderReplayHarness from "./testkit/ProviderReplayHarness.ts";
import * as ThreadManagementService from "./ThreadManagementService.ts";
import * as ServerSettings from "../serverSettings.ts";
import * as SqlitePersistence from "../persistence/Sqlite.ts";
import * as CommandReceiptStore from "./CommandReceiptStore.ts";
import { userFacingDispatchErrorMessage } from "./UserFacingErrors.ts";
import { checkpointWorkspace } from "@onicode/provider-testing/replayWorkspace";
import * as ProviderAdapter from "@onicode/provider-core/server/ProviderAdapter";
import * as ProviderContinuationRequests from "@onicode/provider-core/server/ProviderContinuationRequests";

const driver = ProviderDriverKind.make("codex");
const instanceId = ProviderInstanceId.make("codex");
const modelSelection = { instanceId, model: "test-model" };

it.effect.each(
  (
    [
      [false, "during delivery"],
      [false, "before dispatch"],
      [true, "after delivery"],
      [true, "without native steering"],
      [true, "with interrupting native steering"],
      [true, "settled only"],
      [false, "during continuation"],
      ["question", "during delivery"],
    ] as const
  ).map(([kind, timing]) => ({
    mailbox: kind === true,
    question: kind === "question",
    timing,
    label:
      kind === "question" ? "delegated task question" : kind ? "mailbox notification" : "steering",
  })),
)("delivers $label when completion wins $timing", ({ mailbox, question, timing }) =>
  Effect.gen(function* () {
    const cwd = yield* checkpointWorkspace(`steering-completion-${timing.replaceAll(" ", "-")}`);
    const events = yield* Queue.unbounded<ProviderAdapter.ProviderAdapterV2Event>();
    const started: ProviderAdapter.ProviderAdapterV2TurnInput[] = [];
    const steerEntered = yield* Deferred.make<void>();
    const rejectSteer = yield* Deferred.make<void>();
    let steerCalls = 0;
    const capabilities = {
      ...CodexProviderCapabilitiesV2,
      turns: {
        ...CodexProviderCapabilitiesV2.turns,
        supportsActiveSteering: timing !== "without native steering",
        activeSteeringInterruptsTools: timing === "with interrupting native steering",
      },
    };
    const adapter: ProviderAdapter.ProviderAdapterV2["Service"] = {
      instanceId,
      driver,
      getCapabilities: () => Effect.succeed(capabilities),
      planSelectionTransition: () => Effect.succeed({ type: "apply_on_next_turn" }),
      openSession: (input) =>
        Effect.gen(function* () {
          const now = yield* DateTime.now;
          return {
            instanceId,
            driver,
            providerSessionId: input.providerSessionId,
            providerSession: {
              id: input.providerSessionId,
              driver,
              providerInstanceId: instanceId,
              status: "ready",
              cwd,
              model: modelSelection.model,
              capabilities,
              createdAt: now,
              updatedAt: now,
              lastError: null,
            },
            events: Stream.fromQueue(events),
            ensureThread: ({ threadId }) =>
              Effect.succeed({
                id: ProviderThreadId.make(`provider-thread:${threadId}`),
                driver,
                providerInstanceId: instanceId,
                providerSessionId: input.providerSessionId,
                appThreadId: threadId,
                ownerNodeId: null,
                nativeThreadRef: { driver, nativeId: "native-thread", strength: "strong" },
                nativeConversationHeadRef: null,
                status: "idle",
                firstRunOrdinal: null,
                lastRunOrdinal: null,
                handoffIds: [],
                forkedFrom: null,
                createdAt: now,
                updatedAt: now,
              }),
            resumeThread: ({ providerThread }) => Effect.succeed(providerThread),
            startTurn: (turn) =>
              Effect.gen(function* () {
                started.push(turn);
                yield* Queue.offer(events, {
                  type: "provider_turn.updated",
                  driver,
                  providerTurn: {
                    id: ProviderTurnId.make(`provider-turn:${turn.attemptId}`),
                    providerThreadId: turn.providerThread.id,
                    nodeId: turn.rootNodeId,
                    runAttemptId: turn.attemptId,
                    nativeTurnRef: {
                      driver,
                      nativeId: `native:${turn.attemptId}`,
                      strength: "strong",
                    },
                    ordinal: turn.providerTurnOrdinal,
                    status: "running",
                    startedAt: now,
                    completedAt: null,
                  },
                });
              }),
            steerTurn: (turn) =>
              Effect.gen(function* () {
                steerCalls += 1;
                if (timing === "after delivery" || timing === "during continuation") return;
                yield* Deferred.succeed(steerEntered, undefined);
                yield* Deferred.await(rejectSteer);
                return yield* new ProviderAdapter.ProviderAdapterSteerRunError({
                  driver,
                  providerThreadId: turn.providerThread.id,
                  providerTurnId: turn.providerTurnId,
                  cause: "turn already completed",
                });
              }),
            interruptTurn: () => Effect.void,
            respondToRuntimeRequest: () => Effect.void,
            readThreadSnapshot: () => Effect.die("unused"),
            rollbackThread: () => Effect.die("unused"),
            forkThread: () => Effect.die("unused"),
          };
        }),
    };
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = ThreadId.make("thread:steering-completion");
      const watch = (predicate: (event: OrchestrationV2DomainEvent) => boolean) =>
        orchestrator.streamDomainEvents.pipe(
          Stream.filter(predicate),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped,
        );
      yield* orchestrator.dispatch({
        type: "thread.create",
        commandId: CommandId.make("create"),
        threadId,
        projectId: ProjectId.make("project:steering-completion"),
        title: "Steering race",
        modelSelection,
        runtimeMode: "full-access",
        interactionMode: "default",
        branch: null,
        worktreePath: cwd,
        createdBy: "user",
        creationSource: "web",
      });
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("first"),
        threadId,
        messageId: MessageId.make("message:first"),
        text: "first",
        attachments: [],
        dispatchMode: { type: "start_immediately" },
        createdBy: "user",
        creationSource: "web",
      });
      const running = yield* watch(
        (event) => event.type === "provider-turn.updated" && event.payload.status === "running",
      );
      yield* worker.drain();
      yield* Fiber.join(running);
      const first = started[0]!;
      const messageId = MessageId.make("message:steering");
      const taskId = NodeId.make("task:mailbox");
      if (mailbox) {
        const sink = yield* EventSink.EventSinkV2;
        const current = yield* orchestrator.getThreadProjection(threadId);
        const parentRun = current.runs.find((run) => run.id === first.runId)!;
        const now = yield* DateTime.now;
        yield* sink.write({
          events: [
            {
              id: EventId.make("mailbox:cohort"),
              type: "run.updated",
              threadId,
              runId: first.runId,
              occurredAt: now,
              payload: {
                ...parentRun,
                delegatedCompletion: {
                  disposition: "open",
                  nextGeneration: 2,
                  delivery: { generation: 1, messageId, taskIds: [taskId] },
                },
              },
            },
            {
              id: EventId.make("mailbox:task"),
              type: "subagent.updated",
              threadId,
              runId: first.runId,
              nodeId: taskId,
              occurredAt: now,
              payload: {
                id: taskId,
                threadId,
                runId: first.runId,
                parentNodeId: first.rootNodeId,
                origin: "app_owned",
                createdBy: "agent",
                driver,
                providerInstanceId: instanceId,
                providerThreadId: null,
                childThreadId: null,
                nativeTaskRef: null,
                prompt: "Do background work",
                title: "Background test",
                model: null,
                completionWake: timing === "settled only" ? "settled_only" : "always",
                completionDelivery: { state: "claimed", observedByRunId: null },
                status: "completed",
                result: "done",
                startedAt: now,
                completedAt: now,
                updatedAt: now,
              },
            },
          ],
        });
      }
      if (question) {
        const { thread } = yield* orchestrator.getThreadProjection(threadId);
        yield* (yield* EventSink.EventSinkV2).write({
          events: [
            {
              id: EventId.make("event:composer-selection"),
              type: "thread.model-selection-updated",
              threadId,
              providerInstanceId: instanceId,
              occurredAt: yield* DateTime.now,
              payload: { ...thread, modelSelection: composerSelection },
            },
          ],
        });
      }
      const dispatchSteer = orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("steer"),
        threadId,
        messageId,
        text: "fix the popover",
        attachments: [
          {
            type: "image",
            id: "steering-screenshot",
            name: "image.png",
            mimeType: "image/png",
            sizeBytes: 10,
          },
        ],
        dispatchMode:
          mailbox || question
            ? { type: "queue_after_active" }
            : { type: "steer_active", targetRunId: first.runId },
        createdBy: mailbox || question ? "agent" : "user",
        creationSource: mailbox || question ? "server" : "web",
        ...(question
          ? {
              notification: {
                source: { kind: "delegated_task", taskIds: [taskId] },
                outcome: "updated",
                summary: "Delegated task has a question",
              },
            }
          : {}),
        ...(mailbox
          ? {
              delegatedCompletion: {
                parentRunId: first.runId,
                generation: 1,
                taskIds: [taskId],
              },
            }
          : {}),
      });
      if (timing !== "before dispatch") yield* dispatchSteer;
      if (timing === "during continuation") {
        const projection = yield* orchestrator.getThreadProjection(threadId);
        const turn = projection.providerTurns[0]!;
        const failed = yield* watch(
          (event) => event.type === "provider-turn.updated" && event.payload.status === "failed",
        );
        yield* Queue.offer(events, {
          type: "provider_turn.updated",
          driver,
          providerTurn: { ...turn, status: "failed", completedAt: yield* DateTime.now },
        });
        yield* Fiber.join(failed);
        yield* worker.drain();
        assert.equal(steerCalls, 1);
        assert.lengthOf(started, 1);
        const continued = yield* orchestrator.getThreadProjection(threadId);
        assert.equal(continued.runs[0]?.status, "running");
        assert.equal(
          continued.messages.find((message) => message.id === messageId)?.runId,
          first.runId,
        );
        return;
      }
      if (timing === "after delivery") yield* worker.drain();
      const delivery =
        timing === "during delivery" ? yield* worker.runOnce().pipe(Effect.forkScoped) : null;
      if (delivery !== null) yield* Deferred.await(steerEntered);
      const completed = yield* watch(
        (event) =>
          event.type === "run.updated" &&
          event.payload.id === first.runId &&
          event.payload.status === "completed",
      );
      const projection = yield* orchestrator.getThreadProjection(threadId);
      const turn = projection.providerTurns[0]!;
      yield* Queue.offer(events, {
        type: "provider_turn.updated",
        driver,
        providerTurn: { ...turn, status: "completed", completedAt: yield* DateTime.now },
      });
      yield* Queue.offer(events, {
        type: "turn.terminal",
        driver,
        providerThreadId: turn.providerThreadId,
        providerTurnId: turn.id,
        runOrdinal: first.runOrdinal,
        status: "completed",
        failure: null,
        threadDisposition: "reusable",
      });
      yield* Fiber.join(completed);
      if (delivery !== null) {
        yield* Deferred.succeed(rejectSteer, undefined);
        yield* Fiber.join(delivery);
      }
      if (delivery !== null && !question) {
        const requeued = yield* orchestrator.getThreadProjection(threadId);
        assert.isTrue(requeued.runs.some((run) => run.status === "queued"));
        assert.isFalse(
          requeued.visibleTurnItems.some(
            ({ item }) => item.type === "user_message" && item.messageId === messageId,
          ),
          "a steer that fell back to the queue must not appear delivered",
        );
        const item = requeued.turnItems.find(
          (item) => item.type === "user_message" && item.messageId === messageId,
        );
        assert.equal(item?.status, "pending");
        assert.isNull(item?.startedAt);
        assert.isNull(item?.completedAt);
      }
      if (timing === "before dispatch") yield* dispatchSteer;
      yield* worker.drain();
      yield* orchestrator.resumeQueuedRuns;
      yield* worker.drain();
      if (timing === "after delivery") {
        assert.equal(steerCalls, 1);
        assert.equal(started.length, 1);
        if (mailbox) {
          const delivered = yield* orchestrator.getThreadProjection(threadId);
          assert.equal(delivered.subagents[0]?.completionDelivery?.state, "delivered");
          assert.equal(delivered.subagents[0]?.completionDelivery?.observedByRunId, null);
          assert.equal(delivered.runs[0]?.delegatedCompletion?.delivery, null);
          assert.equal(
            delivered.turnItems.filter((item) => item.type === "notification").length,
            1,
          );
          yield* orchestrator.dispatch({
            type: "notification.delivery.accept",
            commandId: CommandId.make("duplicate-acceptance"),
            threadId,
            messageId,
          });
          yield* worker.drain();
          assert.equal(steerCalls, 1);
          yield* orchestrator.dispatch({
            type: "delegated_task.completion-delivery.acknowledge",
            commandId: CommandId.make("read-result"),
            parentThreadId: threadId,
            taskId,
            observedByRunId: first.runId,
          });
          const acknowledged = yield* orchestrator.getThreadProjection(threadId);
          assert.equal(acknowledged.subagents[0]?.completionDelivery?.state, "acknowledged");
        }
        return;
      }
      assert.equal(started.length, 2);
      assert.equal(started[1]?.message.messageId, messageId);
      if (mailbox) assert.include(started[1]?.message.text ?? "", String(taskId));
      else assert.equal(started[1]?.message.text, "fix the popover");
      assert.deepEqual(started[1]?.message.attachments, [
        {
          type: "image",
          id: "steering-screenshot",
          name: "image.png",
          mimeType: "image/png",
          sizeBytes: 10,
        },
      ]);
      assert.equal(steerCalls, timing === "during delivery" ? 1 : 0);
      const final = yield* orchestrator.getThreadProjection(threadId);
      if (question) {
        assert.deepEqual(final.thread.modelSelection, composerSelection);
        assert.deepEqual(started[1]?.modelSelection, composerSelection);
      }
      if (!mailbox && !question) {
        assert.equal(
          final.turnItems.find(
            (item) => item.type === "user_message" && item.messageId === messageId,
          )?.status,
          "completed",
        );
      }
      assert.equal(final.messages.filter((message) => message.id === messageId).length, 1);
      assert.equal(
        final.messages.find((message) => message.id === messageId)?.runId,
        started[1]?.runId,
      );
      assert.equal(
        final.turnItems.filter((item) =>
          mailbox || question
            ? item.type === "notification"
            : item.type === "user_message" && item.messageId === messageId,
        ).length,
        1,
      );
      if (!mailbox && !question) {
        assert.equal(
          final.visibleTurnItems.filter(
            ({ item }) => item.type === "user_message" && item.messageId === messageId,
          ).length,
          1,
        );
      }
      yield* worker.drain();
      assert.equal(started.length, 2);
    }).pipe(
      Effect.provide(
        ProviderReplayHarness.layerWithRegistry(
          { name: `steering-completion-${timing}` },
          ProviderAdapterRegistry.layerSingle(adapter),
          { runEffectWorker: false },
        ),
      ),
    );
  }),
);

const runSelection = {
  instanceId,
  model: "test-model",
  options: [{ id: "effort", value: "xhigh" }],
};
const composerSelection = {
  ...runSelection,
  options: [...runSelection.options, { id: "fastMode", value: false }],
};

const supervised = { runtimeMode: "approval-required", interactionMode: "default" } as const;

const nextTurnSelectionHarness = Effect.fn("nextTurnSelectionHarness")(function* (
  name: string,
  canRestart = false,
  canSteer = true,
  interruptTurn: Effect.Effect<void> = Effect.void,
  steeringAcceptsAttachments = true,
  nativeGoals?: boolean,
  onSteer: (
    turn: ProviderAdapter.ProviderAdapterV2SteerInput,
  ) => Effect.Effect<void, ProviderAdapter.ProviderAdapterV2Error> = () => Effect.void,
  extraAdapters: ReadonlyArray<ProviderAdapter.ProviderAdapterV2["Service"]> = [],
  steersDuringCompaction = true,
  // Runs after each read of a steer's target, which the worker makes while it prepares the offer.
  afterSteerTargetRead: () => Effect.Effect<void> = () => Effect.void,
  // Fails the next reads that plan a queued run's start.
  queueReads: { failures: number } = { failures: 0 },
) {
  const driver = ProviderDriverKind.make(nativeGoals === undefined ? "codex" : "opencode");
  const cwd = yield* checkpointWorkspace(name);
  const events = yield* Queue.unbounded<ProviderAdapter.ProviderAdapterV2Event>();
  const started: ProviderAdapter.ProviderAdapterV2TurnInput[] = [];
  const steered: string[] = [];
  const steeredSelections: Array<ModelSelection | undefined> = [];
  const interrupted: ProviderTurnId[] = [];
  const capabilities = {
    ...CodexProviderCapabilitiesV2,
    turns: {
      ...CodexProviderCapabilitiesV2.turns,
      supportsActiveSteering: canSteer,
      supportsSteeringByInterruptRestart: canRestart,
      activeSteeringAcceptsAttachments: steeringAcceptsAttachments,
      activeSteeringDuringCompaction: steersDuringCompaction,
      ...(nativeGoals === undefined ? {} : { supportsNativeGoals: nativeGoals }),
    },
  };
  // Pauses a command that asks for capabilities, while it holds the thread's lock.
  const capabilitiesGate = { entered: Effect.void as Effect.Effect<void> };
  // Pauses a steer that changes the selection after it read the target run, holding the lock.
  const selectionGate = { entered: Effect.void as Effect.Effect<void> };
  const adapter: ProviderAdapter.ProviderAdapterV2["Service"] = {
    instanceId,
    driver,
    getCapabilities: () =>
      Effect.suspend(() => capabilitiesGate.entered).pipe(Effect.as(capabilities)),
    planSelectionTransition: () =>
      Effect.suspend(() => selectionGate.entered).pipe(
        Effect.as({ type: "apply_on_next_turn" as const }),
      ),
    openSession: (input) =>
      Effect.gen(function* () {
        const now = yield* DateTime.now;
        const turnUpdated = (
          turn: ProviderAdapter.ProviderAdapterV2TurnInput,
          status: "running" | "interrupted",
        ) =>
          Queue.offer(events, {
            type: "provider_turn.updated",
            driver,
            providerTurn: {
              id: ProviderTurnId.make(`provider-turn:${turn.attemptId}`),
              providerThreadId: turn.providerThread.id,
              nodeId: turn.rootNodeId,
              runAttemptId: turn.attemptId,
              nativeTurnRef: {
                driver,
                nativeId: `native:${turn.attemptId}`,
                strength: "strong",
              },
              ordinal: turn.providerTurnOrdinal,
              status,
              startedAt: now,
              completedAt: status === "running" ? null : now,
            },
          });
        const startTurn = (turn: ProviderAdapter.ProviderAdapterV2TurnInput) =>
          Effect.gen(function* () {
            started.push(turn);
            yield* turnUpdated(turn, "running");
          });
        return {
          instanceId,
          driver,
          providerSessionId: input.providerSessionId,
          providerSession: {
            id: input.providerSessionId,
            driver,
            providerInstanceId: instanceId,
            status: "ready",
            cwd,
            model: runSelection.model,
            capabilities,
            createdAt: now,
            updatedAt: now,
            lastError: null,
          },
          events: Stream.fromQueue(events),
          ensureThread: ({ threadId }) =>
            Effect.succeed({
              id: ProviderThreadId.make(`provider-thread:${threadId}`),
              driver,
              providerInstanceId: instanceId,
              providerSessionId: input.providerSessionId,
              appThreadId: threadId,
              ownerNodeId: null,
              nativeThreadRef: { driver, nativeId: "native-thread", strength: "strong" },
              nativeConversationHeadRef: null,
              status: "idle",
              firstRunOrdinal: null,
              lastRunOrdinal: null,
              handoffIds: [],
              forkedFrom: null,
              createdAt: now,
              updatedAt: now,
            }),
          resumeThread: ({ providerThread }) => Effect.succeed(providerThread),
          startTurn,
          compactThread: startTurn,
          steerTurn: (turn) =>
            Effect.sync(() => {
              steered.push(turn.message.text);
              steeredSelections.push(turn.modelSelection);
            }).pipe(Effect.andThen(onSteer(turn))),
          interruptTurn: ({ providerTurnId }) =>
            Effect.sync(() => {
              interrupted.push(providerTurnId);
            }).pipe(
              Effect.andThen(interruptTurn),
              Effect.andThen(
                Effect.forEach(
                  started.filter(
                    (turn) =>
                      ProviderTurnId.make(`provider-turn:${turn.attemptId}`) === providerTurnId,
                  ),
                  (turn) =>
                    turnUpdated(turn, "interrupted").pipe(
                      Effect.andThen(
                        Queue.offer(events, {
                          type: "turn.terminal",
                          driver,
                          providerThreadId: turn.providerThread.id,
                          providerTurnId,
                          runOrdinal: turn.runOrdinal,
                          status: "interrupted",
                          failure: null,
                          threadDisposition: "reusable",
                        }),
                      ),
                    ),
                  { discard: true },
                ),
              ),
            ),
          respondToRuntimeRequest: () => Effect.void,
          readThreadSnapshot: () => Effect.die("unused"),
          rollbackThread: () => Effect.die("unused"),
          forkThread: () => Effect.die("unused"),
        };
      }),
  };
  const layer = ProviderReplayHarness.layerWithRegistry(
    { name },
    ProviderAdapterRegistry.layerFromAdapters([adapter, ...extraAdapters]),
    {
      runEffectWorker: false,
      wrapProjectionStore: (store) => ({
        ...store,
        canStartQueuedRun: (threadId) =>
          Effect.suspend(() => {
            if (queueReads.failures === 0) return store.canStartQueuedRun(threadId);
            queueReads.failures -= 1;
            return Effect.fail(new ProjectionStore.ProjectionStoreReadError({ threadId }));
          }),
        getProviderControlContext: (threadId, target) =>
          store
            .getProviderControlContext(threadId, target)
            .pipe(Effect.tap(() => Effect.suspend(afterSteerTargetRead))),
      }),
    },
  );
  const startFirstTurn = Effect.gen(function* () {
    const orchestrator = yield* Orchestrator.OrchestratorV2;
    const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
    const threadId = ThreadId.make(`thread:${name}`);
    yield* orchestrator.dispatch({
      type: "thread.create",
      commandId: CommandId.make("create"),
      threadId,
      projectId: ProjectId.make(`project:${name}`),
      title: "Steer with changed options",
      modelSelection: runSelection,
      runtimeMode: "full-access",
      interactionMode: "default",
      branch: null,
      worktreePath: cwd,
      createdBy: "user",
      creationSource: "web",
    });
    const running = yield* orchestrator.streamDomainEvents.pipe(
      Stream.filter(
        (event) => event.type === "provider-turn.updated" && event.payload.status === "running",
      ),
      Stream.take(1),
      Stream.runDrain,
      Effect.forkScoped,
    );
    yield* orchestrator.dispatch({
      type: "message.dispatch",
      commandId: CommandId.make("first"),
      threadId,
      messageId: MessageId.make("message:first"),
      text: "first",
      attachments: [],
      modelSelection: runSelection,
      dispatchMode: { type: "start_immediately" },
      createdBy: "user",
      creationSource: "web",
    });
    yield* worker.drain();
    yield* Fiber.join(running);
    // Ingestion checks turn ownership under this lock after committing the running turn.
    yield* (yield* ThreadCommandExecutor.ThreadCommandExecutor).withLock(threadId, Effect.void);
    return threadId;
  });
  return {
    cwd,
    events,
    started,
    steered,
    steeredSelections,
    interrupted,
    layer,
    startFirstTurn,
    capabilitiesGate,
    selectionGate,
  };
});

const writeCompaction = Effect.fn("writeCompaction")(function* (
  threadId: ThreadId,
  status: "running" | "completed" | "failed",
  // A turn the run replaced, as a restart leaves behind.
  supersededTurn = false,
) {
  const { runs, providerTurns } = yield* (yield* Orchestrator.OrchestratorV2).getThreadProjection(
    threadId,
  );
  const run = runs[0]!;
  const now = yield* DateTime.now;
  yield* (yield* EventSink.EventSinkV2).write({
    events: [
      {
        id: EventId.make(`compaction-${status}-${supersededTurn}`),
        type: "turn-item.updated",
        threadId,
        runId: run.id,
        occurredAt: now,
        payload: {
          id: TurnItemId.make(`turn-item:compaction:${supersededTurn}`),
          threadId,
          runId: run.id,
          nodeId: run.rootNodeId,
          providerThreadId: run.providerThreadId,
          providerTurnId: supersededTurn
            ? ProviderTurnId.make("provider-turn:superseded")
            : providerTurns[0]!.id,
          nativeItemRef: null,
          parentItemId: null,
          // Later than the running turn's own item, so only its turn tells them apart.
          ordinal: supersededTurn ? 1_000_060 : 1_000_050,
          type: "compaction",
          driver,
          status,
          title:
            status === "running"
              ? "Compacting context"
              : status === "completed"
                ? "Context compacted"
                : "Context compaction failed",
          startedAt: now,
          completedAt: status === "running" ? null : now,
          updatedAt: now,
        },
      },
    ],
  });
});

it.effect.each([
  { notice: "delegated task question", goal: null, compacting: false, steers: true },
  {
    notice: "delegated task question",
    goal: null,
    compacting: false,
    steers: false,
    summary: "/review is ready",
  },
  // A steer would abort the compaction, so the question waits for the turn to end.
  { notice: "delegated task question", goal: null, compacting: true, steers: false },
  // Answered after the steer was planned and before it was sent: the provider never gets it.
  { notice: "answered delegated task question", goal: null, compacting: false, steers: false },
  // The same once the worker has begun: the answer commits while it prepares the offer.
  {
    notice: "delegated task question answered during the offer",
    goal: null,
    compacting: false,
    steers: false,
  },
  { notice: "background command", goal: "active", compacting: false, steers: true },
  { notice: "background command", goal: "active", compacting: true, steers: true },
  { notice: "background command", goal: null, compacting: false, steers: false },
] as const)(
  "a $notice notice steers into the running turn: $steers (goal $goal, compacting $compacting)",
  ({ notice, goal, compacting, steers, ...row }) =>
    Effect.gen(function* () {
      const answeredDuringOffer = notice === "delegated task question answered during the offer";
      const answered = answeredDuringOffer || notice === "answered delegated task question";
      let steerTargetReads = 0;
      let answerDuringOffer: Effect.Effect<void> = Effect.void;
      const { started, steered, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
        `notice-steer-${notice.replaceAll(" ", "-")}-${goal ?? "none"}-${compacting}-${steers}`,
        false,
        true,
        Effect.void,
        true,
        undefined,
        undefined,
        [],
        !compacting,
        // The steer reads its target on entry and again to load the session; the second read
        // is past anything checked on entry.
        () => (++steerTargetReads === 2 ? answerDuringOffer : Effect.void),
      );
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const threadId = yield* startFirstTurn;
        if (compacting) yield* writeCompaction(threadId, "running");
        const { thread } = yield* orchestrator.getThreadProjection(threadId);
        yield* (yield* EventSink.EventSinkV2).write({
          events: [
            {
              id: EventId.make("event:composer-selection"),
              type: "thread.model-selection-updated",
              threadId,
              providerInstanceId: instanceId,
              occurredAt: yield* DateTime.now,
              payload: { ...thread, modelSelection: composerSelection },
            },
          ],
        });
        if (goal !== null) {
          const providerThread = (yield* orchestrator.getThreadProjection(threadId))
            .providerThreads[0]!;
          yield* (yield* EventSink.EventSinkV2).write({
            events: [
              {
                id: EventId.make("event:goal-active"),
                type: "provider-thread.updated",
                threadId,
                occurredAt: yield* DateTime.now,
                payload: { ...providerThread, goal: { objective: "Ship it", status: goal } },
              },
            ],
          });
        }
        const childThreadId = ThreadId.make("thread:question-child");
        const requestId = RuntimeRequestId.make("request:question");
        const writeQuestion = Effect.fnUntraced(function* (status: "pending" | "resolved") {
          const now = yield* DateTime.now;
          yield* (yield* EventSink.EventSinkV2).write({
            events: [
              {
                id: EventId.make(`event:question-${status}`),
                type: "runtime-request.updated",
                threadId: childThreadId,
                occurredAt: now,
                payload: {
                  id: requestId,
                  nodeId: NodeId.make("node:question"),
                  providerTurnId: null,
                  nativeRequestRef: null,
                  kind: "user_input",
                  status,
                  responseCapability: { type: "message" },
                  createdAt: now,
                  resolvedAt: status === "pending" ? null : now,
                },
              },
            ],
          });
        });
        if (answered) {
          yield* orchestrator.dispatch({
            type: "thread.create",
            commandId: CommandId.make("create-question-child"),
            threadId: childThreadId,
            projectId: ProjectId.make("project:question-child"),
            title: "Asking child",
            modelSelection: runSelection,
            runtimeMode: "full-access",
            interactionMode: "default",
            branch: null,
            worktreePath: null,
            createdBy: "user",
            creationSource: "web",
          });
          yield* writeQuestion("pending");
        }
        // The reactor's wake id names the question it asks about.
        const messageId = MessageId.make(
          answered ? `message:delegated-task-question:${threadId}:${requestId}` : "message:notice",
        );
        const notification =
          notice !== "background command"
            ? {
                source: {
                  kind: "delegated_task" as const,
                  taskIds: [NodeId.make("task:question")],
                  childThreadId,
                },
                outcome: "updated" as const,
                summary: "summary" in row ? row.summary : "Delegated task has a question",
              }
            : {
                source: { kind: "command" as const },
                outcome: "completed" as const,
                summary: "Background command finished",
              };
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make("notice-wake"),
          threadId,
          messageId,
          text: `${notification.summary}\nexit 0: built 12 files`,
          notification,
          attachments: [],
          dispatchMode: { type: "queue_after_active" },
          createdBy: "agent",
          creationSource: "server",
        });
        if (answeredDuringOffer) {
          steerTargetReads = 0;
          answerDuringOffer = writeQuestion("resolved").pipe(
            Effect.provideService(EventSink.EventSinkV2, yield* EventSink.EventSinkV2),
            Effect.orDie,
          );
        } else if (answered) yield* writeQuestion("resolved");
        yield* worker.drain();
        const after = yield* orchestrator.getThreadProjection(threadId);
        assert.deepEqual(
          steered,
          steers ? [`${notification.summary}\nexit 0: built 12 files`] : [],
        );
        assert.lengthOf(started, 1);
        assert.equal(
          after.runs.some((run) => run.status === "queued"),
          !steers && !answered,
        );
        const message = after.messages.find((candidate) => candidate.id === messageId);
        assert.deepEqual(message?.notification, notification);
        if (steers || answered) assert.equal(message?.runId, started[0]?.runId);
        assert.deepEqual(after.thread.modelSelection, composerSelection);
      }).pipe(Effect.provide(layer));
    }),
);

it.effect("sends a selected held message once and preserves the remaining queue order", () =>
  Effect.gen(function* () {
    const { started, steered, layer, startFirstTurn, capabilitiesGate } =
      yield* nextTurnSelectionHarness("send-held-message");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      yield* Effect.forEach(["queued-first", "selected", "queued-last"], (text) =>
        orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make(text),
          threadId,
          messageId: MessageId.make(`message:${text}`),
          text,
          attachments: [],
          modelSelection: runSelection,
          dispatchMode: { type: "queue_after_active" },
          createdBy: "user",
          creationSource: "web",
        }),
      );
      const before = yield* orchestrator.getThreadProjection(threadId);
      const selected = before.runs.find(
        (run) => run.userMessageId === MessageId.make("message:selected"),
      )!;
      const sink = yield* EventSink.EventSinkV2;
      const now = yield* DateTime.now;
      // Restart recovery cancels the active run and holds every queued message.
      yield* sink.write({
        events: [
          ...before.runs.map((run) => ({
            id: EventId.make(`restart:${run.id}`),
            type: "run.updated" as const,
            threadId,
            runId: run.id,
            occurredAt: now,
            payload:
              run.status === "queued"
                ? { ...run, queueHeld: true }
                : { ...run, status: "cancelled" as const, completedAt: now },
          })),
          ...before.providerTurns.map((turn) => ({
            id: EventId.make(`restart:${turn.id}`),
            type: "provider-turn.updated" as const,
            threadId,
            occurredAt: now,
            payload: { ...turn, status: "cancelled" as const, completedAt: now },
          })),
        ],
      });
      const command = {
        type: "queued-run.start" as const,
        commandId: CommandId.make("send-selected"),
        threadId,
        runId: selected.id,
      };
      // The client's socket drops right after the commit; the resend only replays its receipt.
      const committed = yield* Deferred.make<void>();
      const release = yield* Deferred.make<void>();
      // The start after the commit then blocks on a probe; the interrupt must not wait for it.
      const startEntered = yield* Deferred.make<void>();
      const startRelease = yield* Deferred.make<void>();
      const startedBy: Array<number> = [];
      const blockStart = Deferred.succeed(startEntered, undefined).pipe(
        Effect.andThen(Deferred.await(startRelease)),
        Effect.andThen(Effect.fiberId),
        Effect.map((id) => void startedBy.push(id)),
      );
      const commitCommand = sink.commitCommand;
      using _commit = vi.spyOn(sink, "commitCommand").mockImplementation((input) =>
        input.commandId === command.commandId
          ? commitCommand(input).pipe(
              Effect.tap(() =>
                Effect.sync(() => {
                  capabilitiesGate.entered = blockStart;
                }),
              ),
              Effect.tap(() => Deferred.succeed(committed, undefined)),
              Effect.tap(() => Deferred.await(release)),
            )
          : commitCommand(input),
      );
      const dropped = yield* orchestrator
        .dispatch(command)
        .pipe(Effect.forkChild({ startImmediately: true }));
      yield* Deferred.await(committed);
      const interrupting = yield* Fiber.interrupt(dropped).pipe(
        Effect.forkChild({ startImmediately: true }),
      );
      yield* Deferred.succeed(release, undefined);
      yield* Deferred.await(startEntered);
      yield* Deferred.succeed(startRelease, undefined);
      yield* Fiber.join(interrupting);
      capabilitiesGate.entered = Effect.void;
      yield* worker.drain();
      yield* orchestrator.dispatch(command);
      yield* worker.drain();
      // The background retry started it, not the interrupted call.
      assert.isNotEmpty(startedBy);
      assert.notInclude(startedBy, dropped.id);
      const after = yield* orchestrator.getThreadProjection(threadId);
      assert.equal(after.runs.find((run) => run.id === selected.id)?.status, "running");
      assert.deepEqual(
        started.map((input) => input.message.text),
        ["first", "selected"],
      );
      assert.equal(started[1]?.runId, selected.id);
      assert.equal(started[1]?.message.messageId, selected.userMessageId);
      assert.deepEqual(started[1]?.modelSelection, runSelection);
      assert.deepEqual(steered, []);
      assert.lengthOf(
        after.messages.filter((message) => message.id === selected.userMessageId),
        1,
      );
      assert.deepEqual(
        after.runs.filter((run) => run.status === "queued").map((run) => run.userMessageId),
        [MessageId.make("message:queued-first"), MessageId.make("message:queued-last")],
      );
      assert.isFalse(after.runs.some((run) => run.status === "queued" && run.queueHeld));
      // Another client's tap on the started message has nothing left to send.
      const duplicate = yield* orchestrator.dispatch({
        ...command,
        commandId: CommandId.make("send-selected-again"),
      });
      assert.deepEqual(duplicate.storedEvents, []);
      yield* worker.drain();
      assert.lengthOf(started, 2);
      const busy = yield* orchestrator
        .dispatch({
          ...command,
          commandId: CommandId.make("send-queued-first"),
          runId: after.runs.find(
            (run) => run.userMessageId === MessageId.make("message:queued-first"),
          )!.id,
        })
        .pipe(Effect.flip);
      assert.equal(busy._tag, "OrchestratorDispatchError");
      // A move sent before the selected run started lands after it: the moved run goes first.
      yield* orchestrator.dispatch({
        type: "queued-run.reorder",
        commandId: CommandId.make("move-before-selected"),
        threadId,
        runId: before.runs.find(
          (run) => run.userMessageId === MessageId.make("message:queued-last"),
        )!.id,
        beforeRunId: selected.id,
      });
      assert.deepEqual(
        (yield* orchestrator.getThreadProjection(threadId)).runs
          .filter((run) => run.status === "queued")
          .toSorted((left, right) => (left.queuePosition ?? 0) - (right.queuePosition ?? 0))
          .map((run) => run.userMessageId),
        [MessageId.make("message:queued-last"), MessageId.make("message:queued-first")],
      );
      const runOf = (projection: typeof before, text: string) =>
        projection.runs.find((run) => run.userMessageId === MessageId.make(`message:${text}`))!.id;
      // A target cancelled before it started was never ahead of the queue: the move is refused.
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("queued-dropped"),
        threadId,
        messageId: MessageId.make("message:queued-dropped"),
        text: "queued-dropped",
        attachments: [],
        modelSelection: runSelection,
        dispatchMode: { type: "queue_after_active" },
        createdBy: "user",
        creationSource: "web",
      });
      const withDropped = yield* orchestrator.getThreadProjection(threadId);
      yield* orchestrator.dispatch({
        type: "queued-run.cancel",
        commandId: CommandId.make("cancel-queued-dropped"),
        threadId,
        runId: runOf(withDropped, "queued-dropped"),
      });
      const cancelledTarget = yield* orchestrator
        .dispatch({
          type: "queued-run.reorder",
          commandId: CommandId.make("move-before-cancelled"),
          threadId,
          runId: runOf(withDropped, "queued-first"),
          beforeRunId: runOf(withDropped, "queued-dropped"),
        })
        .pipe(Effect.flip);
      assert.equal(cancelledTarget._tag, "OrchestratorDispatchError");
      // A head that started is ahead of the queue even after it ended: the move goes first.
      yield* orchestrator.dispatch({
        type: "queued-run.reorder",
        commandId: CommandId.make("move-before-ended-head"),
        threadId,
        runId: runOf(withDropped, "queued-first"),
        beforeRunId: runOf(withDropped, "first"),
      });
      assert.deepEqual(
        (yield* orchestrator.getThreadProjection(threadId)).runs
          .filter((run) => run.status === "queued")
          .toSorted((left, right) => (left.queuePosition ?? 0) - (right.queuePosition ?? 0))
          .map((run) => run.userMessageId),
        [MessageId.make("message:queued-first"), MessageId.make("message:queued-last")],
      );
    }).pipe(Effect.provide(layer));
  }),
);

it.effect.each(
  (["preparing", "starting", "running"] as const).flatMap((status) =>
    [false, true].map((direct) => ({ status, direct })),
  ),
)("delivers a deferred steer for a $status run (direct: $direct)", ({ status, direct }) =>
  Effect.gen(function* () {
    const restarting = status === "starting" && !direct;
    const { started, steered, layer, startFirstTurn, events } = yield* nextTurnSelectionHarness(
      `deferred-steer-${status}-${direct}`,
      restarting,
      !restarting,
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const before = yield* orchestrator.getThreadProjection(threadId);
      const run = before.runs[0]!;
      const turn = before.providerTurns[0]!;
      const now = yield* DateTime.now;
      // A compaction the replaced attempt left unfinished must not hold a steer for this one.
      yield* writeCompaction(threadId, "failed", true);
      yield* (yield* EventSink.EventSinkV2).write({
        events: [
          {
            id: EventId.make("setup-run"),
            type: "run.updated",
            threadId,
            occurredAt: now,
            payload: { ...run, status },
          },
          {
            id: EventId.make("setup-turn"),
            type: "provider-turn.updated",
            threadId,
            occurredAt: now,
            payload: { ...turn, status: "pending" },
          },
        ],
      });
      if (status === "preparing" && !direct) {
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make("edited-steer"),
          threadId,
          messageId: MessageId.make("message:edited-steer"),
          text: "edit this steer",
          attachments: [],
          deliveryIntent: "steer",
          dispatchMode: { type: "queue_after_active" },
          createdBy: "user",
          creationSource: "web",
        });
        const edited = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
          (candidate) => candidate.userMessageId === MessageId.make("message:edited-steer"),
        )!;
        assert.equal(edited.steerWhenReadyRunId, run.id);
        yield* orchestrator.dispatch({
          type: "queued-run.edit",
          commandId: CommandId.make("edit-steer-to-compact"),
          modelSelection: composerSelection,
          threadId,
          runId: edited.id,
          text: "/compact",
        });
        const compact = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
          (candidate) => candidate.id === edited.id,
        )!;
        assert.isUndefined(compact.steerWhenReadyRunId);
        assert.deepEqual(compact.modelSelection, composerSelection);
        assert.equal(compact.queuePosition, edited.queuePosition);
      }
      if (direct) {
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make("earlier-queue"),
          threadId,
          messageId: MessageId.make("message:earlier-queue"),
          text: "earlier queued selection",
          modelSelection: composerSelection,
          attachments: [],
          dispatchMode: { type: "queue_after_active" },
          createdBy: "user",
          creationSource: "web",
        });
      }
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("queue"),
        threadId,
        messageId: MessageId.make("message:steer-after-setup"),
        text: "steer after setup",
        attachments: [],
        dispatchMode: { type: "queue_after_active" },
        ...(direct ? { deliveryIntent: "steer" as const } : {}),
        createdBy: "user",
        creationSource: "web",
      });
      const queued = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
        (candidate) => candidate.userMessageId === MessageId.make("message:steer-after-setup"),
      )!;
      assert.deepEqual(
        queued.modelSelection,
        status === "preparing" && !direct ? composerSelection : runSelection,
      );
      for (const text of ["/compact", "/logout"]) {
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make(`queue-${text}`),
          threadId,
          messageId: MessageId.make(`message:${text}`),
          text,
          attachments: [],
          dispatchMode: { type: "queue_after_active" },
          createdBy: "user",
          creationSource: "web",
        });
        const maintenance = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
          (candidate) => candidate.userMessageId === MessageId.make(`message:${text}`),
        )!;
        yield* orchestrator.dispatch({
          type: "queued-message.promote-to-steer",
          commandId: CommandId.make(`promote-${text}`),
          threadId,
          queuedRunId: maintenance.id,
          targetRunId: run.id,
        });
        const promoted = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
          (candidate) => candidate.id === maintenance.id,
        )!;
        assert.equal(promoted.status, "queued");
        assert.equal(promoted.queuePosition, 1);
      }
      const command = {
        type: "queued-message.promote-to-steer" as const,
        commandId: CommandId.make("promote"),
        threadId,
        queuedRunId: queued.id,
        targetRunId: run.id,
      };
      if (!direct) {
        yield* orchestrator.dispatch(command);
        yield* orchestrator.dispatch(command);
      }
      let secondQueuedId: typeof queued.id | undefined;
      if (restarting) {
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make("second"),
          threadId,
          messageId: MessageId.make("message:second"),
          text: "second steer",
          attachments: [],
          dispatchMode: { type: "queue_after_active" },
          createdBy: "user",
          creationSource: "web",
        });
        secondQueuedId = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
          (candidate) => candidate.userMessageId === MessageId.make("message:second"),
        )!.id;
        yield* orchestrator.dispatch({
          ...command,
          commandId: CommandId.make("promote-second"),
          queuedRunId: secondQueuedId,
        });
      }
      const secondReDeferred = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) =>
            event.type === "run.updated" &&
            event.payload.id === secondQueuedId &&
            event.payload.status === "queued",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      const secondPromoted = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) =>
            event.type === "run.updated" &&
            event.payload.id === secondQueuedId &&
            event.payload.status === "cancelled",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      yield* worker.drain();
      assert.deepEqual(steered, []);
      const promoted = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) =>
            event.type === "run.updated" &&
            event.payload.id === queued.id &&
            event.payload.status === "cancelled",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      yield* (yield* EventSink.EventSinkV2).write({
        events: [
          {
            id: EventId.make("ready-run"),
            type: "run.updated",
            threadId,
            occurredAt: now,
            payload: run,
          },
        ],
      });
      yield* Queue.offer(events, { type: "provider_turn.updated", driver, providerTurn: turn });
      yield* Fiber.join(promoted);
      if (restarting) yield* Fiber.join(secondReDeferred);
      yield* worker.drain();
      if (restarting) {
        yield* Fiber.join(secondPromoted);
        yield* worker.drain();
        assert.deepEqual(
          started.map((turn) => turn.message.text),
          ["first", "steer after setup", "second steer"],
        );
      } else {
        assert.deepEqual(steered, ["steer after setup"]);
        assert.lengthOf(started, 1);
      }
      const after = yield* orchestrator.getThreadProjection(threadId);
      assert.equal(
        after.messages.find((message) => message.id === queued.userMessageId)?.runId,
        run.id,
      );
      assert.equal(after.runs.find((candidate) => candidate.id === run.id)?.status, "running");
    }).pipe(Effect.provide(layer));
  }),
);

it.effect.each([
  { native: true, settles: "completed", label: "reach the turn at once" },
  { native: false, settles: "completed", label: "wait for the compaction" },
  // A turn that ends mid-compaction settles it as failed; a steer promoted then is never read.
  { native: false, settles: "failed", label: "run as later turns when the turn ends" },
] as const)("steers sent while the turn compacts $label (native: $native)", ({ native, settles }) =>
  Effect.gen(function* () {
    const { started, steered, layer, startFirstTurn, events } = yield* nextTurnSelectionHarness(
      `steer-during-compaction-${native}-${settles}`,
      false,
      true,
      Effect.void,
      true,
      undefined,
      undefined,
      [],
      native,
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const run = (yield* orchestrator.getThreadProjection(threadId)).runs[0]!;
      yield* writeCompaction(threadId, "running");
      const send = (text: string, steer: boolean) =>
        orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make(text),
          threadId,
          messageId: MessageId.make(`message:${text}`),
          text,
          attachments: [],
          dispatchMode: { type: "queue_after_active" },
          ...(steer ? { deliveryIntent: "steer" as const } : {}),
          createdBy: "user",
          creationSource: "web",
        });
      const queuedRun = (text: string) =>
        orchestrator
          .getThreadProjection(threadId)
          .pipe(
            Effect.map((projection) =>
              projection.runs.find(
                (candidate) =>
                  candidate.userMessageId === MessageId.make(`message:${text}`) &&
                  candidate.status === "queued",
              ),
            ),
          );
      // Stop left an older message held; steering the turn must not resume it.
      yield* send("held", false);
      yield* send("promoted", false);
      yield* (yield* EventSink.EventSinkV2).write({
        events: [
          {
            id: EventId.make("hold-queue"),
            type: "run.updated",
            threadId,
            occurredAt: yield* DateTime.now,
            payload: { ...(yield* queuedRun("held"))!, queueHeld: true },
          },
        ],
      });
      yield* send("steered", true);
      const promotedRun = (yield* queuedRun("promoted"))!;
      yield* orchestrator.dispatch({
        type: "queued-message.promote-to-steer",
        commandId: CommandId.make("promote"),
        threadId,
        queuedRunId: promotedRun.id,
        targetRunId: run.id,
      });
      yield* worker.drain();
      assert.isTrue((yield* queuedRun("held"))?.queueHeld);
      if (native) {
        assert.deepEqual(steered, ["steered", "promoted"]);
        return;
      }
      assert.deepEqual(steered, []);
      assert.equal((yield* queuedRun("steered"))?.steerWhenReadyRunId, run.id);
      assert.equal((yield* queuedRun("promoted"))?.steerWhenReadyRunId, run.id);
      // The steer itself is not parked with it: if its turn ends first, it runs next.
      assert.isNotTrue((yield* queuedRun("steered"))?.queueHeld);
      // Claude reports the turn running on each usage frame, also right before it ends the turn.
      const reportRunning = Effect.gen(function* () {
        yield* Queue.offer(events, {
          type: "provider_turn.updated",
          driver,
          providerTurn: (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!,
        });
      });
      yield* reportRunning;
      if (settles === "failed") {
        const ended = yield* orchestrator.streamDomainEvents.pipe(
          Stream.filter(
            (event) =>
              event.type === "run.updated" &&
              event.payload.id === run.id &&
              event.payload.status !== "running",
          ),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped,
        );
        yield* writeCompaction(threadId, "failed");
        yield* reportRunning;
        // Neither a late signal from a turn the run replaced nor another automatic release
        // delivers them now.
        yield* writeCompaction(threadId, "completed", true);
        const promote = (commandId: string, text: string) =>
          queuedRun(text).pipe(
            Effect.flatMap((queued) =>
              orchestrator.dispatch({
                type: "queued-message.promote-to-steer",
                commandId: CommandId.make(commandId),
                threadId,
                queuedRunId: queued!.id,
                targetRunId: run.id,
              }),
            ),
            Effect.andThen(worker.drain()),
          );
        yield* promote("command:deferred-steer:after-failure", "promoted");
        assert.deepEqual(steered, []);
        // "Steer now" goes in, as a steer typed now would.
        yield* promote("steer-now", "steered");
        assert.deepEqual(steered, ["steered"]);
        const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!;
        yield* Queue.offer(events, {
          type: "provider_turn.updated",
          driver,
          providerTurn: { ...turn, status: "interrupted", completedAt: yield* DateTime.now },
        });
        yield* Queue.offer(events, {
          type: "turn.terminal",
          driver,
          providerThreadId: turn.providerThreadId,
          providerTurnId: turn.id,
          runOrdinal: run.ordinal,
          status: "interrupted",
          failure: null,
          threadDisposition: "reusable",
        });
        yield* Fiber.join(ended);
        yield* worker.drain();
        // The steer the turn never took runs as a turn of its own.
        assert.deepEqual(steered, ["steered"]);
        assert.equal(started.at(-1)?.runId, promotedRun.id);
        return;
      }
      const delivered = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) =>
            event.type === "run.updated" &&
            event.payload.steerWhenReadyRunId === run.id &&
            event.payload.status === "cancelled",
        ),
        Stream.take(2),
        Stream.runDrain,
        Effect.forkScoped,
      );
      yield* writeCompaction(threadId, "completed");
      yield* Fiber.join(delivered);
      yield* worker.drain();
      // Held steers reach the turn in queue order.
      assert.deepEqual(steered, ["promoted", "steered"]);
      assert.lengthOf(started, 1);
      const after = yield* orchestrator.getThreadProjection(threadId);
      assert.deepEqual(
        after.runs
          .filter((candidate) => candidate.status === "queued")
          .map((queued) => queued.userMessageId),
        [MessageId.make("message:held")],
      );
      for (const text of ["steered", "promoted"]) {
        assert.equal(
          after.messages.find((message) => message.id === MessageId.make(`message:${text}`))?.runId,
          run.id,
        );
      }
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("force sends multiple queued messages and can force send a requeued message again", () =>
  Effect.gen(function* () {
    const { started, steered, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
      "multiple-promoted-messages",
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const queue = (text: string, commandId: string) =>
        orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make(commandId),
          threadId,
          messageId: MessageId.make(`message:${text}`),
          text,
          attachments: [],
          dispatchMode: { type: "queue_after_active" },
          createdBy: "user",
          creationSource: "web",
        });
      for (const text of ["one", "two", "remaining"]) yield* queue(text, `queue-${text}`);
      const promote = Effect.fn("test.promote")(function* (text: string, commandId: string) {
        const projection = yield* orchestrator.getThreadProjection(threadId);
        const queued = projection.runs.find(
          (run) =>
            run.status === "queued" && run.userMessageId === MessageId.make(`message:${text}`),
        )!;
        yield* orchestrator.dispatch({
          type: "queued-message.promote-to-steer",
          commandId: CommandId.make(commandId),
          threadId,
          queuedRunId: queued.id,
          targetRunId: started[0]!.runId,
        });
      });
      yield* promote("one", "promote-one");
      yield* promote("two", "promote-two");
      yield* worker.drain();
      assert.deepEqual(steered, ["one", "two"]);

      // The completion-race fallback redispatches the same message into the queue.
      yield* queue("one", "requeue-one");
      const requeued = yield* orchestrator.getThreadProjection(threadId);
      assert.equal(
        requeued.turnItems.find(
          (item) => item.type === "user_message" && item.messageId === "message:one",
        )?.status,
        "pending",
      );
      yield* promote("one", "promote-one-again");
      yield* worker.drain();
      assert.deepEqual(steered, ["one", "two", "one"]);
      assert.lengthOf(started, 1);
      const final = yield* orchestrator.getThreadProjection(threadId);
      assert.deepEqual(
        final.runs.filter((run) => run.status === "queued").map((run) => run.userMessageId),
        [MessageId.make("message:remaining")],
      );
      for (const text of ["one", "two"]) {
        const items = final.turnItems.filter(
          (item) => item.type === "user_message" && item.messageId === `message:${text}`,
        );
        assert.lengthOf(items, 1);
        assert.equal(items[0]?.status, "completed");
      }
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("keeps a message queued when send now races with a running turn", () =>
  Effect.gen(function* () {
    const { started, layer, startFirstTurn } = yield* nextTurnSelectionHarness("send-queued-race");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("queue"),
        threadId,
        messageId: MessageId.make("message:queued"),
        text: "queued",
        attachments: [],
        dispatchMode: { type: "queue_after_active" },
        createdBy: "user",
        creationSource: "web",
      });
      const before = yield* orchestrator.getThreadProjection(threadId);
      const queued = before.runs.find((run) => run.status === "queued")!;
      const error = yield* orchestrator
        .dispatch({
          type: "queued-run.start",
          commandId: CommandId.make("send-now"),
          threadId,
          runId: queued.id,
        })
        .pipe(Effect.flip);
      yield* worker.drain();
      assert.equal(error._tag, "OrchestratorDispatchError");
      assert.equal(started.length, 1);
      const after = yield* orchestrator.getThreadProjection(threadId);
      assert.deepEqual(after.runs, before.runs);
    }).pipe(Effect.provide(layer));
  }),
);

// The turn has ended and its checkpoint capture has not landed yet.
const endTurnBeforeCapture = Effect.fn("endTurnBeforeCapture")(function* (
  events: Queue.Queue<ProviderAdapter.ProviderAdapterV2Event>,
  threadId: ThreadId,
  first: ProviderAdapter.ProviderAdapterV2TurnInput,
) {
  const orchestrator = yield* Orchestrator.OrchestratorV2;
  const ended = yield* orchestrator.streamDomainEvents.pipe(
    Stream.filter(
      (event) =>
        event.type === "run.updated" &&
        event.payload.id === first.runId &&
        event.payload.status === "completed",
    ),
    Stream.take(1),
    Stream.runDrain,
    Effect.forkScoped,
  );
  const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns.find(
    (candidate) => candidate.runAttemptId === first.attemptId,
  )!;
  yield* Queue.offer(events, {
    type: "provider_turn.updated",
    driver,
    providerTurn: { ...turn, status: "completed", completedAt: yield* DateTime.now },
  });
  yield* Queue.offer(events, {
    type: "turn.terminal",
    driver,
    providerThreadId: turn.providerThreadId,
    providerTurnId: turn.id,
    runOrdinal: first.runOrdinal,
    status: "completed",
    failure: null,
    threadDisposition: "reusable",
  });
  yield* Fiber.join(ended);
  return (yield* orchestrator.getThreadProjection(threadId)).runs.find(
    (run) => run.id === first.runId,
  )!;
});

it.effect("sends the picked queued message when Send now lands before the turn's capture", () =>
  Effect.gen(function* () {
    const { events, started, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
      "send-queued-during-capture",
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      for (const text of ["second", "third"]) {
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make(`queue-${text}`),
          threadId,
          messageId: MessageId.make(`message:${text}`),
          text,
          attachments: [],
          dispatchMode: { type: "queue_after_active" },
          createdBy: "user",
          creationSource: "web",
        });
      }
      yield* endTurnBeforeCapture(events, threadId, started[0]!);
      const nextStarted = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) =>
            event.type === "run.updated" &&
            event.payload.id !== started[0]!.runId &&
            event.payload.status !== "queued",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      yield* orchestrator.dispatch({
        type: "queued-run.start",
        commandId: CommandId.make("send-third-now"),
        threadId,
        runId: (yield* orchestrator.getThreadProjection(threadId)).runs.find(
          (run) => run.userMessageId === MessageId.make("message:third"),
        )!.id,
      });
      yield* worker.drain();
      yield* Fiber.join(nextStarted);
      yield* worker.drain();
      assert.deepEqual(
        started.map((turn) => turn.message.messageId),
        [MessageId.make("message:first"), MessageId.make("message:third")],
      );
    }).pipe(Effect.provide(layer));
  }),
);

it.effect.each(["stop", "unstartable", "unreadable", "another cancelled"] as const)(
  "a Send now before the turn's capture keeps to its own message: %s",
  (scenario) =>
    Effect.gen(function* () {
      const queueReads = { failures: 0 };
      const { events, started, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
        `send-queued-during-capture-${scenario}`,
        undefined,
        undefined,
        undefined,
        undefined,
        undefined,
        undefined,
        undefined,
        undefined,
        undefined,
        queueReads,
      );
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const threadId = yield* startFirstTurn;
        const send = (text: string) =>
          orchestrator.dispatch({
            type: "message.dispatch",
            commandId: CommandId.make(`queue-${text}`),
            threadId,
            messageId: MessageId.make(`message:${text}`),
            text,
            attachments: [],
            dispatchMode: { type: "queue_after_active" },
            createdBy: "user",
            creationSource: "web",
          });
        const runOf = Effect.fnUntraced(function* (text: string) {
          return (yield* orchestrator.getThreadProjection(threadId)).runs.find(
            (run) => run.userMessageId === MessageId.make(`message:${text}`),
          )!;
        });
        const settled = (predicate: (event: OrchestrationV2DomainEvent) => boolean) =>
          orchestrator.streamDomainEvents.pipe(
            Stream.filter(predicate),
            Stream.take(1),
            Stream.runDrain,
            Effect.forkScoped,
          );
        yield* send("second");
        yield* send("third");
        const second = yield* runOf("second");
        const third = yield* runOf("third");
        if (scenario === "another cancelled") yield* send("fourth");
        yield* endTurnBeforeCapture(events, threadId, started[0]!);
        if (scenario === "unstartable") {
          yield* (yield* EventSink.EventSinkV2).write({
            events: [
              {
                id: EventId.make("event:unstartable"),
                type: "run.updated",
                threadId,
                runId: third.id,
                occurredAt: yield* DateTime.now,
                payload: { ...third, activeAttemptId: null },
              },
            ],
          });
        }
        yield* orchestrator.dispatch({
          type: "queued-run.start",
          commandId: CommandId.make("send-third-now"),
          threadId,
          runId: third.id,
        });
        if (scenario === "another cancelled") {
          // A later message cancelled before it ran is not a newer turn.
          yield* orchestrator.dispatch({
            type: "queued-run.cancel",
            commandId: CommandId.make("cancel-fourth"),
            threadId,
            runId: (yield* runOf("fourth")).id,
          });
          const picked = yield* settled(
            (event) =>
              event.type === "run.updated" &&
              (event.payload.id === second.id || event.payload.id === third.id) &&
              event.payload.status !== "queued",
          );
          yield* worker.drain();
          yield* Fiber.join(picked);
          yield* worker.drain();
          assert.equal(started[1]?.message.messageId, MessageId.make("message:third"));
          return;
        }
        if (scenario === "unreadable") queueReads.failures = 1;
        if (scenario === "unstartable" || scenario === "unreadable") {
          const failed = yield* settled(
            (event) => event.type === "run.updated" && event.payload.status === "failed",
          );
          yield* worker.drain();
          yield* Fiber.join(failed);
          assert.equal((yield* runOf("third")).status, "failed");
          assert.notEqual((yield* runOf("second")).status, "failed");
          if (scenario === "unreadable") return;
          // The failed message never ran, so the turn that follows it is the latest one.
          yield* TestClock.adjust("1 second");
          const secondRunning = yield* settled(
            (event) => event.type === "provider-turn.updated" && event.payload.status === "running",
          );
          yield* orchestrator.dispatch({
            type: "queued-run.start",
            commandId: CommandId.make("send-second-now"),
            threadId,
            runId: second.id,
          });
          yield* worker.drain();
          yield* Fiber.join(secondRunning);
          yield* (yield* ThreadCommandExecutor.ThreadCommandExecutor).withLock(
            threadId,
            Effect.void,
          );
          yield* endTurnBeforeCapture(events, threadId, started[1]!);
          yield* send("fourth");
          assert.equal((yield* runOf("fourth")).status, "queued");
          return;
        }
        // Stop takes the pick back with the queue it holds.
        yield* orchestrator.dispatch({
          type: "thread.stop",
          commandId: CommandId.make("stop-during-capture"),
          threadId,
        });
        yield* send("fourth");
        const nextStarted = yield* settled(
          (event) =>
            event.type === "run.updated" &&
            (event.payload.id === second.id || event.payload.id === third.id) &&
            event.payload.status !== "queued",
        );
        yield* worker.drain();
        yield* Fiber.join(nextStarted);
        yield* worker.drain();
        assert.equal(started[1]?.message.messageId, MessageId.make("message:second"));
      }).pipe(Effect.provide(layer));
    }),
);

it.effect("keeps the turn end of a run archived before its capture lands", () =>
  Effect.gen(function* () {
    const { events, started, layer, startFirstTurn } =
      yield* nextTurnSelectionHarness("archive-during-capture");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const threadId = yield* startFirstTurn;
      const first = started[0]!;
      const running = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
        (run) => run.id === first.runId,
      )!;
      // An open delegation cohort is what archive disposes, rewriting the run.
      yield* (yield* EventSink.EventSinkV2).write({
        events: [
          {
            id: EventId.make("event:archive-during-capture:cohort"),
            type: "run.updated",
            threadId,
            runId: first.runId,
            occurredAt: yield* DateTime.now,
            payload: {
              ...running,
              delegatedCompletion: { disposition: "open", nextGeneration: 1, delivery: null },
            },
          },
        ],
      });
      const ended = yield* endTurnBeforeCapture(events, threadId, first);
      assert.isNotNull(ended.completedAt);
      yield* orchestrator.dispatch({
        type: "thread.archive",
        commandId: CommandId.make("archive"),
        threadId,
      });
      const archived = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
        (run) => run.id === first.runId,
      )!;
      assert.deepEqual(
        [archived.status, archived.completedAt, archived.delegatedCompletion?.disposition],
        ["completed", ended.completedAt, "disposed"],
      );
    }).pipe(Effect.provide(layer));
  }),
);

// regression: #19210 a provider that can restart the turn must still steer, since restarting ends its native subagents
it.effect(
  "steers a changed turn-scoped selection into a running turn the provider could restart",
  () =>
    Effect.gen(function* () {
      const { started, steered, steeredSelections, layer, startFirstTurn } =
        yield* nextTurnSelectionHarness("steering-selection-change", true);
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const threadId = yield* startFirstTurn;
        const steer = (id: string, selection: ModelSelection) =>
          orchestrator
            .dispatch({
              type: "message.dispatch",
              commandId: CommandId.make(id),
              threadId,
              messageId: MessageId.make(`message:${id}`),
              text: id,
              attachments: [],
              modelSelection: selection,
              dispatchMode: { type: "steer_active", targetRunId: started[0]!.runId },
              createdBy: "user",
              creationSource: "web",
            })
            .pipe(Effect.andThen(worker.drain()));

        yield* steer("steer-changed", composerSelection);
        const changed = yield* orchestrator.getThreadProjection(threadId);
        assert.deepEqual(steered, ["steer-changed"]);
        assert.equal(started.length, 1);
        assert.lengthOf(changed.attempts, 1);
        assert.equal(changed.runs[0]?.status, "running");
        assert.deepEqual(changed.runs[0]?.modelSelection, runSelection);
        assert.deepEqual(changed.thread.modelSelection, composerSelection);

        yield* steer("steer-reverted", runSelection);
        const reverted = yield* orchestrator.getThreadProjection(threadId);
        assert.deepEqual(steered, ["steer-changed", "steer-reverted"]);
        assert.lengthOf(reverted.attempts, 1);
        assert.deepEqual(reverted.thread.modelSelection, runSelection);

        const otherSelection = {
          instanceId: ProviderInstanceId.make("codex-work"),
          model: "other",
        };
        const sink = yield* EventSink.EventSinkV2;
        yield* sink.write({
          events: [
            {
              id: EventId.make("switched-away"),
              type: "thread.provider-switched",
              threadId,
              providerInstanceId: otherSelection.instanceId,
              occurredAt: yield* DateTime.now,
              payload: {
                ...reverted.thread,
                providerInstanceId: otherSelection.instanceId,
                modelSelection: otherSelection,
              },
            },
          ],
        });
        yield* steer("steer-back", runSelection);
        const back = yield* orchestrator.getThreadProjection(threadId);
        assert.deepEqual(steered, ["steer-changed", "steer-reverted", "steer-back"]);
        // The adapter gets each steer's selection so it can switch the live turn, and back.
        assert.deepEqual(steeredSelections, [composerSelection, runSelection, runSelection]);
        assert.equal(back.thread.providerInstanceId, instanceId);
        assert.deepEqual(back.thread.modelSelection, runSelection);
      }).pipe(Effect.provide(layer));
    }),
);

it.effect("restarts the turn for an image when native steering takes text only", () =>
  Effect.gen(function* () {
    const { started, steered, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
      "steering-text-only-attachment",
      true,
      true,
      Effect.void,
      false,
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const before = yield* orchestrator.getThreadProjection(threadId);
      const originalStart = before.runs[0]!.startedAt;
      assert.isNotNull(originalStart);
      yield* TestClock.adjust("10 seconds");
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("steer-image"),
        threadId,
        messageId: MessageId.make("message:steer-image"),
        text: "look at this",
        attachments: [
          {
            type: "image",
            id: ChatAttachmentId.make("steer-image"),
            name: "screenshot.png",
            mimeType: "image/png",
            sizeBytes: 4,
          },
        ],
        modelSelection: runSelection,
        dispatchMode: { type: "steer_active", targetRunId: started[0]!.runId },
        createdBy: "user",
        creationSource: "web",
      });
      yield* worker.drain();
      const after = yield* orchestrator.getThreadProjection(threadId);
      assert.equal(after.runs[0]?.id, before.runs[0]?.id);
      assert.deepEqual(after.runs[0]?.startedAt, originalStart);
      assert.equal(after.attempts[0]?.status, "superseded");
      assert.equal(after.attempts[0]?.userMessageId, before.runs[0]?.userMessageId);
      assert.equal(after.runs[0]?.userMessageId, "message:steer-image");
      const restartedAt = yield* DateTime.now;
      assert.deepEqual(after.attempts[1]?.startedAt, restartedAt);
      assert.deepEqual(
        after.nodes.find((node) => node.id === after.runs[0]?.rootNodeId)?.startedAt,
        restartedAt,
      );
      assert.deepEqual(steered, []);
      assert.deepEqual(
        started.map((turn) => [turn.message.text, turn.message.attachments.length]),
        [
          ["first", 0],
          ["look at this", 1],
        ],
      );
    }).pipe(Effect.provide(layer));
  }),
);

it.effect.each(["interrupted", "failed"] as const)(
  "returns a steer that missed a turn ending %s to the queue, held",
  (status) =>
    Effect.gen(function* () {
      const { events, started, steered, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
        `steering-missed-${status}`,
      );
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const threadId = yield* startFirstTurn;
        const first = started[0]!;
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make("steer"),
          threadId,
          messageId: MessageId.make("message:steer"),
          text: "late steer",
          attachments: [],
          dispatchMode: { type: "steer_active", targetRunId: first.runId },
          createdBy: "user",
          creationSource: "web",
        });
        const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!;
        const ended = yield* orchestrator.streamDomainEvents.pipe(
          Stream.filter(
            (event) =>
              event.type === "run.updated" &&
              event.payload.id === first.runId &&
              event.payload.status === status,
          ),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped,
        );
        yield* Queue.offer(events, {
          type: "provider_turn.updated",
          driver,
          providerTurn: { ...turn, status, completedAt: yield* DateTime.now },
        });
        const terminal = {
          type: "turn.terminal",
          driver,
          providerThreadId: turn.providerThreadId,
          providerTurnId: turn.id,
          runOrdinal: first.runOrdinal,
          threadDisposition: "reusable",
        } as const;
        yield* Queue.offer(
          events,
          status === "failed"
            ? {
                ...terminal,
                status,
                failureItemOrdinal: 1_000_060,
                failure: makeProviderFailure({ message: "Overloaded" }),
              }
            : { ...terminal, status, failure: null },
        );
        yield* Fiber.join(ended);
        yield* worker.drain();

        const projection = yield* orchestrator.getThreadProjection(threadId);
        assert.deepEqual(steered, []);
        assert.lengthOf(started, 1);
        const message = projection.messages.find((row) => row.id === "message:steer");
        const queued = projection.runs.find((run) => run.id === message?.runId);
        assert.equal(queued?.status, "queued");
        assert.isTrue(queued?.queueHeld);
        // The user's next message releases it, as it releases any held queue.
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make("next"),
          threadId,
          messageId: MessageId.make("message:next"),
          text: "next",
          attachments: [],
          dispatchMode: { type: "queue_after_active" },
          createdBy: "user",
          creationSource: "web",
        });
        yield* worker.drain();
        assert.isNotTrue(
          (yield* orchestrator.getThreadProjection(threadId)).runs.find(
            (run) => run.id === queued?.id,
          )?.queueHeld,
        );
      }).pipe(Effect.provide(layer));
    }),
);

it.effect("queues a steer that missed a completed turn Stop reached without holding it", () =>
  Effect.gen(function* () {
    const { events, started, steered, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
      "steering-missed-after-stop",
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const first = started[0]!;
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("steer"),
        threadId,
        messageId: MessageId.make("message:steer"),
        text: "late steer",
        attachments: [],
        dispatchMode: { type: "steer_active", targetRunId: first.runId },
        createdBy: "user",
        creationSource: "web",
      });
      const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!;
      const ended = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) =>
            event.type === "run.updated" &&
            event.payload.id === first.runId &&
            event.payload.status !== "running",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      // The turn finishes by itself before the worker delivers either the steer or the stop.
      yield* orchestrator.dispatch({
        type: "run.interrupt",
        commandId: CommandId.make("stop"),
        threadId,
        runId: first.runId,
        holdQueue: true,
      });
      yield* Queue.offer(events, {
        type: "provider_turn.updated",
        driver,
        providerTurn: { ...turn, status: "completed", completedAt: yield* DateTime.now },
      });
      yield* Queue.offer(events, {
        type: "turn.terminal",
        driver,
        providerThreadId: turn.providerThreadId,
        providerTurnId: turn.id,
        runOrdinal: first.runOrdinal,
        status: "completed",
        failure: null,
        threadDisposition: "reusable",
      });
      yield* Fiber.join(ended);
      yield* worker.drain();

      // Stop does not hold the steer its turn never read: it runs once the stopped run settles.
      const projection = yield* orchestrator.getThreadProjection(threadId);
      assert.deepEqual(steered, []);
      assert.lengthOf(started, 1);
      const message = projection.messages.find((row) => row.id === "message:steer");
      const queued = projection.runs.find((run) => run.id === message?.runId);
      assert.equal(queued?.status, "queued");
      assert.isNotTrue(queued?.queueHeld);
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("releases a steer whose hold was written after its turn left it", () =>
  Effect.gen(function* () {
    const { layer, startFirstTurn, capabilitiesGate } = yield* nextTurnSelectionHarness(
      "steer-hold-after-turn-left",
      false,
      true,
      Effect.void,
      true,
      undefined,
      undefined,
      [],
      false,
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const threadId = yield* startFirstTurn;
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("queued"),
        threadId,
        messageId: MessageId.make("message:queued"),
        text: "queued",
        attachments: [],
        dispatchMode: { type: "queue_after_active" },
        createdBy: "user",
        creationSource: "web",
      });
      const { runs } = yield* orchestrator.getThreadProjection(threadId);
      const target = runs[0]!;
      const queued = runs[1]!;
      yield* writeCompaction(threadId, "running");
      // The promote has read the compacting turn and is about to mark the steer as waiting on it.
      const paused = yield* Deferred.make<void>();
      const resume = yield* Deferred.make<void>();
      capabilitiesGate.entered = Deferred.succeed(paused, undefined).pipe(
        Effect.andThen(Deferred.await(resume)),
      );
      const promoting = yield* orchestrator
        .dispatch({
          type: "queued-message.promote-to-steer",
          commandId: CommandId.make("promote"),
          threadId,
          queuedRunId: queued.id,
          targetRunId: target.id,
        })
        .pipe(Effect.forkChild);
      yield* Deferred.await(paused);
      capabilitiesGate.entered = Effect.void;
      // Recovery returns the turn to the queue, which the run-end reaction does not see.
      yield* (yield* EventSink.EventSinkV2).write({
        commandId: CommandId.make("command:runtime-reconcile:steer-hold-after-turn-left"),
        events: [
          {
            id: EventId.make("requeue-target"),
            type: "run.updated",
            threadId,
            runId: target.id,
            occurredAt: yield* DateTime.now,
            payload: { ...target, status: "queued", queueHeld: true },
          },
        ],
      });
      // The scan runs behind the promote. Had it read at once, it would have found no mark, and
      // the one written next would never be released.
      yield* orchestrator.recoverDelegatedTasks;
      yield* Deferred.succeed(resume, undefined);
      const marked = yield* Fiber.join(promoting);
      const released = yield* (yield* EventSink.EventSinkV2)
        .stream({ afterSequence: marked.sequence, eventType: "run.updated" })
        .pipe(
          Stream.filter((stored) => stored.event.runId === queued.id),
          Stream.take(1),
          Stream.runCollect,
        );
      const run = released[0]?.event.type === "run.updated" ? released[0].event.payload : null;
      assert.equal(run?.status, "queued");
      assert.isUndefined(run?.steerWhenReadyRunId);
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("starts the queue when Resume lands before a held steer is released", () =>
  Effect.gen(function* () {
    const { layer, startFirstTurn, started } = yield* nextTurnSelectionHarness(
      "resume-before-steer-release",
      false,
      true,
      Effect.void,
      true,
      undefined,
      undefined,
      [],
      false,
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const sink = yield* EventSink.EventSinkV2;
      const locks = yield* ThreadCommandExecutor.ThreadCommandExecutor;
      const threadId = yield* startFirstTurn;
      yield* writeCompaction(threadId, "running");
      const target = (yield* orchestrator.getThreadProjection(threadId)).runs[0]!;
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("steer"),
        threadId,
        messageId: MessageId.make("message:steered"),
        text: "steered",
        attachments: [],
        dispatchMode: { type: "steer_active", targetRunId: target.id },
        createdBy: "user",
        creationSource: "web",
      });
      const held = (yield* orchestrator.getThreadProjection(threadId)).runs[1]!;
      assert.equal(held.steerWhenReadyRunId, target.id);

      // Holding the thread's lock puts the release's read first and Resume ahead of its command.
      const locked = yield* Deferred.make<void>();
      const unlock = yield* Deferred.make<void>();
      const holder = yield* locks
        .withLock(
          threadId,
          Deferred.succeed(locked, undefined).pipe(Effect.andThen(Deferred.await(unlock))),
        )
        .pipe(Effect.forkChild);
      yield* Deferred.await(locked);
      const now = yield* DateTime.now;
      // Recovery stopped the turn and held what was queued behind it.
      yield* sink.write({
        commandId: CommandId.make("command:runtime-reconcile:resume-before-steer-release"),
        events: [
          {
            id: EventId.make("recovery-holds-steer"),
            type: "run.updated",
            threadId,
            runId: held.id,
            occurredAt: now,
            payload: { ...held, queueHeld: true },
          },
          {
            id: EventId.make("recovery-cancels-target"),
            type: "run.updated",
            threadId,
            runId: target.id,
            occurredAt: now,
            payload: { ...target, status: "cancelled", completedAt: now },
          },
        ],
      });
      yield* orchestrator.recoverDelegatedTasks;
      const resume = yield* orchestrator
        .dispatch({ type: "queue.resume", commandId: CommandId.make("resume"), threadId })
        .pipe(Effect.forkChild({ startImmediately: true }));
      yield* Deferred.succeed(unlock, undefined);
      yield* Fiber.join(holder);
      const resumed = yield* Fiber.join(resume);
      yield* sink.stream({ afterSequence: resumed.sequence, eventType: "run.updated" }).pipe(
        Stream.filter(
          (stored) =>
            stored.event.type === "run.updated" &&
            stored.event.runId === held.id &&
            stored.event.payload.status !== "queued",
        ),
        Stream.take(1),
        Stream.runDrain,
      );
      yield* worker.drain();
      assert.equal(started.at(-1)?.runId, held.id);
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("resumes a stopped queue for a steer that missed a completed turn", () =>
  Effect.gen(function* () {
    const { events, started, steered, layer, startFirstTurn, send, read } = yield* goalHarness(
      "steering-missed-held-queue",
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      yield* send("queued", "queued", "queue");
      yield* orchestrator.dispatch({
        type: "thread.stop",
        commandId: CommandId.make("stop"),
        threadId,
      });
      yield* worker.drain();
      const secondRunning = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) => event.type === "provider-turn.updated" && event.payload.status === "running",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("second"),
        threadId,
        messageId: MessageId.make("message:second"),
        text: "second",
        attachments: [],
        dispatchMode: { type: "start_immediately" },
        createdBy: "agent",
        creationSource: "mcp",
      });
      yield* worker.drain();
      yield* Fiber.join(secondRunning);
      const second = started[1]!;
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("steer"),
        threadId,
        messageId: MessageId.make("message:steer"),
        text: "late steer",
        attachments: [],
        dispatchMode: { type: "steer_active", targetRunId: second.runId },
        createdBy: "user",
        creationSource: "web",
      });
      // Held behind the stopped queue; the steer is placed ahead of it, shifting it down.
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("agent-later"),
        threadId,
        messageId: MessageId.make("message:agent-later"),
        text: "agent later",
        attachments: [],
        dispatchMode: { type: "queue_after_active" },
        createdBy: "agent",
        creationSource: "mcp",
      });
      // Still active when the worker finds the turn over, so the steer queues behind it.
      const waiting = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) =>
            event.type === "run.updated" &&
            event.payload.id === second.runId &&
            event.payload.status === "completed",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns.find(
        (candidate) => candidate.runAttemptId === second.attemptId,
      )!;
      yield* Queue.offer(events, {
        type: "provider_turn.updated",
        driver,
        providerTurn: { ...turn, status: "completed", completedAt: yield* DateTime.now },
      });
      yield* Queue.offer(events, {
        type: "turn.terminal",
        driver,
        providerThreadId: turn.providerThreadId,
        providerTurnId: turn.id,
        runOrdinal: second.runOrdinal,
        status: "completed",
        failure: null,
        threadDisposition: "reusable",
      });
      yield* Fiber.join(waiting);
      yield* worker.drain();
      yield* orchestrator.resumeQueuedRuns;
      yield* worker.drain();

      // The steer is the user's next message: it resumes the queue and waits its turn in it.
      assert.deepEqual(steered, []);
      const { queue, started: startedTexts } = yield* read(threadId);
      assert.deepEqual(
        { queue, started: startedTexts },
        {
          queue: ["late steer", "agent later"],
          started: [
            "first",
            `[Onicode: this message came through Onicode's tools, not from the user typing in this chat.]\n\nsecond`,
            "queued",
          ],
        },
      );
      assert.isEmpty(
        (yield* orchestrator.getThreadProjection(threadId)).runs.filter(
          (run) => run.status === "queued" && run.queueHeld === true,
        ),
      );
    }).pipe(Effect.provide(layer));
  }),
);

it.effect.each(["complete", "stop-before-end"] as const)(
  "starts a steer that missed the turn in its place on the saved next-turn selection: %s",
  (timing) =>
    Effect.gen(function* () {
      const { events, started, steered, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
        `steering-selection-follow-up-${timing}`,
      );
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const threadId = yield* startFirstTurn;
        const first = started[0]!;
        if (timing === "stop-before-end") {
          yield* orchestrator.dispatch({
            type: "message.dispatch",
            commandId: CommandId.make("queued-earlier"),
            threadId,
            messageId: MessageId.make("message:queued-earlier"),
            text: "queued earlier",
            attachments: [],
            dispatchMode: { type: "queue_after_active" },
            createdBy: "user",
            creationSource: "web",
          });
        }
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make("steer"),
          threadId,
          messageId: MessageId.make("message:steer"),
          text: "late steer",
          attachments: [],
          modelSelection: composerSelection,
          dispatchMode: { type: "steer_active", targetRunId: first.runId },
          createdBy: "user",
          creationSource: "web",
        });
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make("queued-later"),
          threadId,
          messageId: MessageId.make("message:queued-later"),
          text: "queued later",
          attachments: [],
          dispatchMode: { type: "queue_after_active" },
          createdBy: "user",
          creationSource: "web",
        });
        if (timing === "stop-before-end") {
          yield* orchestrator.dispatch({
            type: "run.interrupt",
            commandId: CommandId.make("stop"),
            threadId,
            runId: first.runId,
            holdQueue: true,
          });
        }
        const settled = yield* orchestrator.streamDomainEvents.pipe(
          Stream.filter(
            (event) =>
              event.type === "run.updated" &&
              event.payload.id === first.runId &&
              event.payload.status === "completed",
          ),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped,
        );
        const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!;
        yield* Queue.offer(events, {
          type: "provider_turn.updated",
          driver,
          providerTurn: { ...turn, status: "completed", completedAt: yield* DateTime.now },
        });
        yield* Queue.offer(events, {
          type: "turn.terminal",
          driver,
          providerThreadId: turn.providerThreadId,
          providerTurnId: turn.id,
          runOrdinal: first.runOrdinal,
          status: "completed",
          failure: null,
          threadDisposition: "reusable",
        });
        yield* Fiber.join(settled);
        yield* worker.drain();
        if (timing === "stop-before-end") {
          const queued = (yield* orchestrator.getThreadProjection(threadId)).runs
            .filter((run) => run.status === "queued")
            .toSorted((left, right) => (left.queuePosition ?? 0) - (right.queuePosition ?? 0));
          // The unread steer goes next and shows first; Stop's hold stays on the rest of the queue.
          assert.deepEqual(
            queued.map((run) => [run.userMessageId, run.queueHeld === true]),
            [
              [MessageId.make("message:steer"), false],
              [MessageId.make("message:queued-earlier"), true],
              [MessageId.make("message:queued-later"), true],
            ],
          );
          return;
        }
        yield* orchestrator.resumeQueuedRuns;
        yield* worker.drain();

        const projection = yield* orchestrator.getThreadProjection(threadId);
        assert.deepEqual(steered, []);
        assert.equal(started.length, 2);
        assert.equal(started[1]?.message.messageId, MessageId.make("message:steer"));
        assert.deepEqual(started[1]?.modelSelection, composerSelection);
        assert.deepEqual(projection.thread.modelSelection, composerSelection);
        const message = projection.turnItems.find(
          (item) =>
            item.type === "user_message" && item.messageId === MessageId.make("message:steer"),
        );
        assert.equal(message?.runId, started[1]!.runId);
        assert.isAbove(
          message!.ordinal,
          Math.max(
            ...projection.turnItems
              .filter((item) => item.runId === first.runId)
              .map((item) => item.ordinal),
          ),
        );
      }).pipe(Effect.provide(layer));
    }),
);

const declinedSteerTimings = [
  "complete",
  "stop-before-decline",
  "stop-after-decline",
  "queued-model",
  "two-steers",
  // A steer that named no model runs on the pick made after its decline.
  "pick-after-decline",
  // A queued message promoted to a steer goes back to its place in the queue.
  "promoted",
  "promoted-after-reorder",
  "promoted-again",
  "promoted-after-later-promotion",
  // Promotions and steers still in flight keep the order they were sent in.
  "promoted-while-earlier-in-flight",
  "promoted-while-steer-in-flight",
] as const;

it.effect.each(declinedSteerTimings)(
  "persists a declined steer without blocking the worker: %s",
  (timing) =>
    Effect.gen(function* () {
      let beforeDecline: Effect.Effect<void> = Effect.void;
      const { events, started, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
        `steering-declined-${timing}`,
        false,
        true,
        Effect.void,
        true,
        undefined,
        (turn) =>
          Effect.suspend(() => beforeDecline).pipe(
            Effect.andThen(
              Effect.fail(
                new ProviderAdapter.ProviderAdapterSteerDeclinedError({
                  driver,
                  providerThreadId: turn.providerThread.id,
                  providerTurnId: turn.providerTurnId,
                }),
              ),
            ),
          ),
      );
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const threadId = yield* startFirstTurn;
        const first = started[0]!;
        const stop = orchestrator
          .dispatch({
            type: "run.interrupt",
            commandId: CommandId.make("stop"),
            threadId,
            runId: first.runId,
            holdQueue: true,
          })
          .pipe(Effect.asVoid, Effect.orDie);
        if (timing === "stop-before-decline") beforeDecline = stop;
        const steerSelection = { ...runSelection, model: "steer-model" };
        if (timing === "queued-model") {
          yield* orchestrator.dispatch({
            type: "message.dispatch",
            commandId: CommandId.make("queued-model"),
            threadId,
            messageId: MessageId.make("message:queued-model"),
            text: "already queued",
            attachments: [],
            modelSelection: composerSelection,
            dispatchMode: { type: "queue_after_active" },
            createdBy: "user",
            creationSource: "web",
          });
          beforeDecline = Effect.forEach(["queued-later", "steer-later", "queued-last"], (id) =>
            orchestrator.dispatch({
              type: "message.dispatch",
              commandId: CommandId.make(id),
              threadId,
              messageId: MessageId.make(`message:${id}`),
              text: id,
              attachments: [],
              dispatchMode:
                id === "steer-later"
                  ? { type: "steer_active", targetRunId: first.runId }
                  : { type: "queue_after_active" },
              createdBy: "user",
              creationSource: "web",
            }),
          ).pipe(Effect.asVoid, Effect.orDie);
        }
        if (timing === "two-steers") {
          // The second steer arrives before the first is declined.
          beforeDecline = orchestrator
            .dispatch({
              type: "message.dispatch",
              commandId: CommandId.make("steer-second"),
              threadId,
              messageId: MessageId.make("message:steer-second"),
              text: "second",
              attachments: [],
              dispatchMode: { type: "steer_active", targetRunId: first.runId },
              createdBy: "user",
              creationSource: "web",
            })
            .pipe(
              Effect.andThen(Effect.sync(() => (beforeDecline = Effect.void))),
              Effect.asVoid,
              Effect.orDie,
            );
        }
        const messageId = MessageId.make("message:declined");
        const queuedLater = MessageId.make("message:queued-later");
        const queuedLast = MessageId.make("message:queued-last");
        const queueMessage = (id: MessageId) =>
          orchestrator.dispatch({
            type: "message.dispatch",
            commandId: CommandId.make(`queue-${id}`),
            threadId,
            messageId: id,
            text: id,
            attachments: [],
            dispatchMode: { type: "queue_after_active" },
            createdBy: "user",
            creationSource: "web",
          });
        const queuedRunOf = (id: MessageId) =>
          orchestrator
            .getThreadProjection(threadId)
            .pipe(
              Effect.map((projection) =>
                projection.runs.find((run) => run.userMessageId === id && run.status === "queued")!,
              ),
            );
        const promoteInFlight = (id: MessageId, commandId: string) =>
          Effect.gen(function* () {
            yield* orchestrator.dispatch({
              type: "queued-message.promote-to-steer",
              commandId: CommandId.make(commandId),
              threadId,
              queuedRunId: (yield* queuedRunOf(id)).id,
              targetRunId: first.runId,
            });
          });
        const promote = (id: MessageId, commandId: string) =>
          promoteInFlight(id, commandId).pipe(Effect.andThen(worker.drain()));
        if (timing === "promoted-again") {
          // Declined once, it goes ahead of what was queued after it was sent; promoted and
          // declined again, it keeps that place.
          yield* orchestrator.dispatch({
            type: "message.dispatch",
            commandId: CommandId.make("steer-first"),
            threadId,
            messageId,
            text: "follow up",
            attachments: [],
            dispatchMode: { type: "steer_active", targetRunId: first.runId },
            createdBy: "user",
            creationSource: "web",
          });
          yield* queueMessage(queuedLater);
          yield* worker.drain();
          yield* promote(messageId, "steer");
        } else if (timing === "promoted-while-steer-in-flight") {
          yield* orchestrator.dispatch({
            type: "message.dispatch",
            commandId: CommandId.make("steer"),
            threadId,
            messageId,
            text: "follow up",
            attachments: [],
            dispatchMode: { type: "steer_active", targetRunId: first.runId },
            createdBy: "user",
            creationSource: "web",
          });
          yield* queueMessage(queuedLater);
          yield* promoteInFlight(queuedLater, "promote-later");
        } else if (timing === "promoted-while-earlier-in-flight") {
          yield* queueMessage(messageId);
          yield* queueMessage(queuedLater);
          yield* promoteInFlight(messageId, "steer");
          yield* promoteInFlight(queuedLater, "promote-later");
        } else if (timing.startsWith("promoted")) {
          yield* queueMessage(messageId);
          yield* queueMessage(queuedLater);
          if (timing === "promoted-after-reorder") {
            yield* queueMessage(queuedLast);
            yield* orchestrator.dispatch({
              type: "queued-run.reorder",
              commandId: CommandId.make("reorder"),
              threadId,
              runId: (yield* queuedRunOf(queuedLater)).id,
              beforeRunId: (yield* queuedRunOf(messageId)).id,
            });
          }
          if (timing === "promoted-after-later-promotion") {
            yield* promote(queuedLater, "promote-later");
          }
          yield* promote(messageId, "steer");
        } else {
          yield* orchestrator.dispatch({
            type: "message.dispatch",
            commandId: CommandId.make("steer"),
            threadId,
            messageId,
            text: "follow up",
            attachments: [],
            ...(timing === "queued-model" ? { modelSelection: steerSelection } : {}),
            dispatchMode: { type: "steer_active", targetRunId: first.runId },
            createdBy: "user",
            creationSource: "web",
          });
        }
        yield* worker.drain();
        const pending = yield* orchestrator.getThreadProjection(threadId);
        const queued = pending.runs.find(
          (run) =>
            run.userMessageId === messageId && run.id !== first.runId && run.status !== "cancelled",
        );
        const effects = yield* (yield* EffectOutbox.EffectOutboxV2).listByCommandId(
          CommandId.make("steer"),
        );
        assert.deepEqual(
          effects.map((effect) => [effect.request.type, effect.status]),
          [["provider-turn.steer", "succeeded"]],
        );
        assert.equal(started.length, 1);
        assert.equal(queued?.status, "queued");
        assert.equal(queued?.userMessageId, messageId);
        // Stop keeps a steer the provider then declines: it runs next, not held.
        if (timing === "stop-before-decline") {
          assert.isNotTrue(queued?.queueHeld);
          return;
        }
        if (timing === "queued-model") {
          assert.deepEqual(queued?.modelSelection, steerSelection);
          assert.deepEqual(
            pending.runs
              .filter((run) => run.status === "queued")
              .toSorted(
                (left, right) =>
                  (left.queuePosition ?? left.ordinal) - (right.queuePosition ?? right.ordinal),
              )
              .map((run) => run.userMessageId),
            [
              MessageId.make("message:queued-model"),
              messageId,
              MessageId.make("message:queued-later"),
              MessageId.make("message:steer-later"),
              MessageId.make("message:queued-last"),
            ],
          );
          return;
        }
        if (timing.startsWith("promoted")) {
          assert.deepEqual(
            pending.runs
              .filter((run) => run.status === "queued")
              .toSorted(
                (left, right) =>
                  (left.queuePosition ?? left.ordinal) - (right.queuePosition ?? right.ordinal),
              )
              .map((run) => run.userMessageId),
            timing === "promoted-after-reorder"
              ? [queuedLater, messageId, queuedLast]
              : [messageId, queuedLater],
          );
          return;
        }
        if (timing === "two-steers") {
          assert.deepEqual(
            pending.runs
              .filter((run) => run.status === "queued")
              .toSorted(
                (left, right) =>
                  (left.queuePosition ?? left.ordinal) - (right.queuePosition ?? right.ordinal),
              )
              .map((run) => run.userMessageId),
            [messageId, MessageId.make("message:steer-second")],
          );
          return;
        }
        if (timing === "stop-after-decline") {
          yield* stop;
          yield* worker.drain();
          yield* orchestrator.resumeQueuedRuns;
          yield* worker.drain();
          const held = yield* orchestrator.getThreadProjection(threadId);
          assert.equal(held.runs.find((run) => run.id === queued?.id)?.queueHeld, true);
          assert.equal(started.length, 1);
          return;
        }
        if (timing === "pick-after-decline") {
          yield* orchestrator.dispatch({
            type: "thread.model-selection.set",
            commandId: CommandId.make("select-composer"),
            threadId,
            modelSelection: composerSelection,
          });
        }
        const settled = yield* orchestrator.streamDomainEvents.pipe(
          Stream.filter(
            (event) =>
              event.type === "run.updated" &&
              event.payload.id === first.runId &&
              event.payload.status === "completed",
          ),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped,
        );
        const turn = pending.providerTurns[0]!;
        yield* Queue.offer(events, {
          type: "provider_turn.updated",
          driver,
          providerTurn: { ...turn, status: "completed", completedAt: yield* DateTime.now },
        });
        yield* Queue.offer(events, {
          type: "turn.terminal",
          driver,
          providerThreadId: turn.providerThreadId,
          providerTurnId: turn.id,
          runOrdinal: first.runOrdinal,
          status: "completed",
          failure: null,
          threadDisposition: "reusable",
        });
        yield* Fiber.join(settled);
        yield* worker.drain();
        yield* orchestrator.resumeQueuedRuns;
        yield* worker.drain();
        assert.equal(started.length, 2);
        assert.equal(started[1]?.message.messageId, messageId);
        assert.equal(started[1]?.runId, queued?.id);
        assert.deepEqual(
          started[1]?.modelSelection,
          timing === "pick-after-decline" ? composerSelection : runSelection,
        );
      }).pipe(Effect.provide(layer));
    }),
);

// regression: #41509 a declined steer of a delegated completion wake must still queue the wake
it.effect("queues a delegated completion wake whose steer the provider declined", () =>
  Effect.gen(function* () {
    const { events, started, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
      "steering-declined-delegated-completion",
      false,
      true,
      Effect.void,
      true,
      undefined,
      (turn) =>
        Effect.fail(
          new ProviderAdapter.ProviderAdapterSteerDeclinedError({
            driver,
            providerThreadId: turn.providerThread.id,
            providerTurnId: turn.providerTurnId,
          }),
        ),
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const first = started[0]!;
      const messageId = MessageId.make("message:declined-wake");
      const taskId = NodeId.make("task:declined-wake");
      const parentRun = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
        (run) => run.id === first.runId,
      )!;
      const now = yield* DateTime.now;
      yield* (yield* EventSink.EventSinkV2).write({
        events: [
          {
            id: EventId.make("event:declined-wake:cohort"),
            type: "run.updated",
            threadId,
            runId: first.runId,
            occurredAt: now,
            payload: {
              ...parentRun,
              delegatedCompletion: {
                disposition: "open",
                nextGeneration: 2,
                delivery: { generation: 1, messageId, taskIds: [taskId] },
              },
            },
          },
          {
            id: EventId.make("event:declined-wake:task"),
            type: "subagent.updated",
            threadId,
            runId: first.runId,
            nodeId: taskId,
            occurredAt: now,
            payload: {
              id: taskId,
              threadId,
              runId: first.runId,
              parentNodeId: first.rootNodeId,
              origin: "app_owned",
              createdBy: "agent",
              driver,
              providerInstanceId: instanceId,
              providerThreadId: null,
              childThreadId: null,
              nativeTaskRef: null,
              prompt: "Do background work",
              title: "Background test",
              model: null,
              completionWake: "always",
              completionDelivery: { state: "claimed", observedByRunId: null },
              status: "completed",
              result: "done",
              startedAt: now,
              completedAt: now,
              updatedAt: now,
            },
          },
        ],
      });
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("wake"),
        threadId,
        messageId,
        text: "Delegated task completed",
        attachments: [],
        dispatchMode: { type: "queue_after_active" },
        delegatedCompletion: { parentRunId: first.runId, generation: 1, taskIds: [taskId] },
        createdBy: "agent",
        creationSource: "server",
      });
      yield* worker.drain();
      const effects = yield* (yield* EffectOutbox.EffectOutboxV2).listByCommandId(
        CommandId.make("wake"),
      );
      assert.deepEqual(
        effects.map((effect) => [effect.request.type, effect.status]),
        [["provider-turn.steer", "succeeded"]],
        "the declined steer's queue fallback was rejected",
      );
      const pending = yield* orchestrator.getThreadProjection(threadId);
      const queued = pending.runs.find(
        (run) => run.userMessageId === messageId && run.id !== first.runId,
      );
      assert.equal(queued?.status, "queued");

      const turn = pending.providerTurns[0]!;
      const settled = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) =>
            event.type === "run.updated" &&
            event.payload.id === first.runId &&
            event.payload.status === "completed",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      yield* Queue.offer(events, {
        type: "provider_turn.updated",
        driver,
        providerTurn: { ...turn, status: "completed", completedAt: yield* DateTime.now },
      });
      yield* Queue.offer(events, {
        type: "turn.terminal",
        driver,
        providerThreadId: turn.providerThreadId,
        providerTurnId: turn.id,
        runOrdinal: first.runOrdinal,
        status: "completed",
        failure: null,
        threadDisposition: "reusable",
      });
      yield* Fiber.join(settled);
      yield* worker.drain();
      yield* orchestrator.resumeQueuedRuns;
      yield* worker.drain();
      assert.equal(started[1]?.message.messageId, messageId, "the parent was never woken");
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("steers a notice that missed its turn into the next running turn", () =>
  Effect.gen(function* () {
    const { events, started, steered, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
      "steering-follow-up-next-turn",
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const first = started[0]!;
      const messageId = MessageId.make("message:missed-notice");
      const notice = {
        type: "message.dispatch" as const,
        threadId,
        messageId,
        text: "Delegated task has a question",
        notification: {
          source: {
            kind: "delegated_task" as const,
            taskIds: [NodeId.make("task:missed-notice")],
            childThreadId: ThreadId.make("thread:missed-notice-child"),
          },
          outcome: "updated" as const,
          summary: "Delegated task has a question",
        },
        attachments: [],
        dispatchMode: { type: "queue_after_active" as const },
        createdBy: "agent" as const,
        creationSource: "server" as const,
      };
      yield* orchestrator.dispatch({ ...notice, commandId: CommandId.make("notice") });
      yield* worker.drain();
      assert.deepEqual(steered, [notice.text]);

      // The first turn ends before the provider reads the notice; the next one starts.
      const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!;
      const settled = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) =>
            event.type === "run.updated" &&
            event.payload.id === first.runId &&
            event.payload.status === "completed",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      yield* Queue.offer(events, {
        type: "provider_turn.updated",
        driver,
        providerTurn: { ...turn, status: "completed", completedAt: yield* DateTime.now },
      });
      yield* Queue.offer(events, {
        type: "turn.terminal",
        driver,
        providerThreadId: turn.providerThreadId,
        providerTurnId: turn.id,
        runOrdinal: first.runOrdinal,
        status: "completed",
        failure: null,
        threadDisposition: "reusable",
      });
      yield* Fiber.join(settled);
      yield* worker.drain();
      const running = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) => event.type === "provider-turn.updated" && event.payload.status === "running",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("second"),
        threadId,
        messageId: MessageId.make("message:second"),
        text: "second",
        attachments: [],
        modelSelection: runSelection,
        dispatchMode: { type: "start_immediately" },
        createdBy: "user",
        creationSource: "web",
      });
      yield* worker.drain();
      yield* Fiber.join(running);
      yield* (yield* ThreadCommandExecutor.ThreadCommandExecutor).withLock(threadId, Effect.void);
      const second = started[1]!;

      // The worker's follow-up for the missed notice names the turn that missed it.
      yield* orchestrator.dispatch({
        ...notice,
        commandId: CommandId.make("notice-follow-up"),
        steerFollowUpOfRunId: first.runId,
      });
      yield* worker.drain();
      const after = yield* orchestrator.getThreadProjection(threadId);
      assert.deepEqual(steered, [notice.text, notice.text]);
      assert.isFalse(after.runs.some((run) => run.status === "queued"));
      assert.equal(after.messages.find((m) => m.id === messageId)?.runId, second.runId);
    }).pipe(Effect.provide(layer));
  }),
);

// regression: #26695 cancelling a Copilot prompt to steer it kills its background agents
it.effect("queues a steer until the turn ends when the provider cannot steer it", () =>
  Effect.gen(function* () {
    const { events, started, steered, interrupted, layer, startFirstTurn } =
      yield* nextTurnSelectionHarness("steering-unsupported", false, false);
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const first = started[0]!;
      const messageId = MessageId.make("message:steer");
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("steer"),
        threadId,
        messageId,
        text: "follow up",
        attachments: [],
        modelSelection: runSelection,
        dispatchMode: { type: "start_immediately" },
        deliveryIntent: "steer",
        createdBy: "user",
        creationSource: "web",
      });
      yield* worker.drain();

      const held = yield* orchestrator.getThreadProjection(threadId);
      assert.deepEqual(steered, []);
      assert.deepEqual(interrupted, []);
      assert.equal(started.length, 1);
      assert.equal(held.runs.find((run) => run.id === first.runId)?.status, "running");
      const queued = held.runs.find((run) => run.userMessageId === messageId);
      assert.equal(queued?.status, "queued");

      const settled = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) =>
            event.type === "run.updated" &&
            event.payload.id === first.runId &&
            event.payload.status === "completed",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      const turn = held.providerTurns[0]!;
      yield* Queue.offer(events, {
        type: "provider_turn.updated",
        driver,
        providerTurn: { ...turn, status: "completed", completedAt: yield* DateTime.now },
      });
      yield* Queue.offer(events, {
        type: "turn.terminal",
        driver,
        providerThreadId: turn.providerThreadId,
        providerTurnId: turn.id,
        runOrdinal: first.runOrdinal,
        status: "completed",
        failure: null,
        threadDisposition: "reusable",
      });
      yield* Fiber.join(settled);
      yield* worker.drain();
      yield* orchestrator.resumeQueuedRuns;
      yield* worker.drain();

      assert.deepEqual(interrupted, []);
      assert.equal(started.length, 2);
      assert.equal(started[1]?.runId, queued?.id);
      assert.equal(started[1]?.message.text, "follow up");
    }).pipe(Effect.provide(layer));
  }),
);

it.effect(
  "refuses a limited caller's steer into a turn started before its thread was lowered",
  () =>
    Effect.gen(function* () {
      const { steered, layer, startFirstTurn } =
        yield* nextTurnSelectionHarness("steer-above-limit");
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const threadId = yield* startFirstTurn;
        // Codex keeps its session, and its running turn the full-access policy it started with.
        yield* orchestrator.dispatch({
          type: "thread.runtime-mode.set",
          commandId: CommandId.make("lower"),
          threadId,
          runtimeMode: "approval-required",
        });
        const steer = {
          type: "message.dispatch",
          commandId: CommandId.make("steer"),
          threadId,
          messageId: MessageId.make("message:steer"),
          text: "follow up",
          attachments: [],
          dispatchMode: { type: "start_immediately" },
          deliveryIntent: "steer",
          createdBy: "agent",
          creationSource: "mcp",
        } as const;
        const refused = yield* orchestrator
          .dispatch(steer)
          .pipe(Effect.provideService(DispatchModeLimit, supervised), Effect.flip);
        assert.ok(refused._tag === "OrchestratorThreadAboveModeLimitError");
        assert.equal(refused.runtimeMode, "full-access");
        yield* worker.drain();
        assert.deepEqual(steered, []);

        // The refusal records nothing, and a caller without the limit still reaches the turn.
        yield* orchestrator.dispatch(steer);
        yield* worker.drain();
        assert.lengthOf(steered, 1);

        // The turn's own record decides: one that started within the limit takes the steer.
        const attempt = (yield* orchestrator.getThreadProjection(threadId)).attempts[0]!;
        assert.deepEqual(attempt.startedModes, {
          runtimeMode: "full-access",
          interactionMode: "default",
        });
        yield* (yield* EventSink.EventSinkV2).write({
          events: [
            {
              id: EventId.make("started-supervised"),
              type: "run-attempt.updated",
              threadId,
              occurredAt: yield* DateTime.now,
              payload: { ...attempt, startedModes: supervised },
            },
          ],
        });
        yield* orchestrator
          .dispatch({
            ...steer,
            commandId: CommandId.make("steer-within"),
            messageId: MessageId.make("message:steer-within"),
          })
          .pipe(Effect.provideService(DispatchModeLimit, supervised));
        yield* worker.drain();
        assert.lengthOf(steered, 2);
      }).pipe(Effect.provide(layer));
    }),
);

it.effect("queues a limited caller's steer into a turn that is not running yet", () =>
  Effect.gen(function* () {
    const { started, layer, startFirstTurn } = yield* nextTurnSelectionHarness("steer-not-ready");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const threadId = yield* startFirstTurn;
      const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!;
      yield* (yield* EventSink.EventSinkV2).write({
        events: [
          {
            id: EventId.make("turn-not-ready"),
            type: "provider-turn.updated",
            threadId,
            occurredAt: yield* DateTime.now,
            payload: { ...turn, status: "pending" },
          },
        ],
      });
      const steer = (name: string) =>
        ({
          type: "message.dispatch",
          commandId: CommandId.make(name),
          threadId,
          messageId: MessageId.make(`message:${name}`),
          text: "follow up",
          attachments: [],
          dispatchMode: { type: "steer_active", targetRunId: started[0]!.runId },
          createdBy: "agent",
          creationSource: "mcp",
        }) as const;
      const queuedBy = Effect.fn("queuedBy")(function* (name: string) {
        return (yield* orchestrator.getThreadProjection(threadId)).runs.find(
          (run) => run.userMessageId === MessageId.make(`message:${name}`),
        )!;
      });
      // Every MCP caller carries a limit; one at full access parks its steer like the owner.
      yield* orchestrator.dispatch(steer("parked")).pipe(
        Effect.provideService(DispatchModeLimit, {
          runtimeMode: "full-access",
          interactionMode: "default",
        }),
      );
      const parked = yield* queuedBy("parked");
      assert.equal(parked.steerWhenReadyRunId, started[0]!.runId);

      yield* orchestrator.dispatch({
        type: "thread.runtime-mode.set",
        commandId: CommandId.make("lower"),
        threadId,
        runtimeMode: "approval-required",
      });
      // A parked steer is delivered by the server itself, which carries no caller limit.
      yield* orchestrator
        .dispatch({
          type: "queued-run.edit",
          commandId: CommandId.make("edit-parked"),
          threadId,
          runId: parked.id,
          text: "rewritten",
        })
        .pipe(Effect.provideService(DispatchModeLimit, supervised));
      assert.isUndefined((yield* queuedBy("parked")).steerWhenReadyRunId);

      yield* orchestrator
        .dispatch(steer("steer"))
        .pipe(Effect.provideService(DispatchModeLimit, supervised));
      const queued = yield* queuedBy("steer");
      assert.isUndefined(queued.steerWhenReadyRunId);

      yield* orchestrator
        .dispatch({
          type: "queued-message.promote-to-steer",
          commandId: CommandId.make("promote"),
          threadId,
          queuedRunId: queued.id,
          targetRunId: started[0]!.runId,
        })
        .pipe(Effect.provideService(DispatchModeLimit, supervised), Effect.flip);
      const kept = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
        (run) => run.id === queued.id,
      )!;
      assert.equal(kept.status, "queued");
      assert.isUndefined(kept.steerWhenReadyRunId);
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("refuses a limited caller's answer to a turn started above its limit", () =>
  Effect.gen(function* () {
    const { started, layer, startFirstTurn } = yield* nextTurnSelectionHarness("answer-above");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const threadId = yield* startFirstTurn;
      const now = yield* DateTime.now;
      const requestId = RuntimeRequestId.make("request:question");
      const session = (yield* orchestrator.getThreadProjection(threadId)).providerSessions[0]!;
      yield* (yield* EventSink.EventSinkV2).write({
        events: [
          {
            id: EventId.make("question"),
            type: "runtime-request.updated",
            threadId,
            occurredAt: now,
            payload: {
              id: requestId,
              nodeId: started[0]!.rootNodeId,
              providerTurnId: null,
              nativeRequestRef: null,
              kind: "command",
              status: "pending",
              responseCapability: { type: "live", providerSessionId: session.id },
              createdAt: now,
              resolvedAt: null,
            },
          },
        ],
      });
      yield* orchestrator.dispatch({
        type: "thread.runtime-mode.set",
        commandId: CommandId.make("lower"),
        threadId,
        runtimeMode: "approval-required",
      });
      const respond = {
        type: "runtime-request.respond",
        commandId: CommandId.make("respond"),
        threadId,
        requestId,
        decision: "accept",
      } as const;
      const refused = yield* orchestrator
        .dispatch(respond)
        .pipe(Effect.provideService(DispatchModeLimit, supervised), Effect.flip);
      assert.ok(refused._tag === "OrchestratorThreadAboveModeLimitError");
      assert.equal(refused.runtimeMode, "full-access");

      yield* orchestrator.dispatch(respond);
      assert.equal(
        (yield* orchestrator.getThreadProjection(threadId)).runtimeRequests[0]?.status,
        "resolved",
      );
    }).pipe(Effect.provide(layer));
  }),
);

// regression: #39404 a later agent message steers ahead of one queued behind /compact
it.effect("delivers agent messages queued behind /compact before a later one steers", () =>
  Effect.gen(function* () {
    const { events, started, steered, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
      "agent-order-after-compact",
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const nextRunning = Effect.fn("nextRunning")(function* () {
        return yield* orchestrator.streamDomainEvents.pipe(
          Stream.filter(
            (event) => event.type === "provider-turn.updated" && event.payload.status === "running",
          ),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped,
        );
      });
      const lock = (yield* ThreadCommandExecutor.ThreadCommandExecutor).withLock(
        threadId,
        Effect.void,
      );
      const finishTurn = Effect.fn("finishTurn")(function* (index: number) {
        const turnInput = started[index]!;
        const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns.find(
          (candidate) => candidate.runAttemptId === turnInput.attemptId,
        )!;
        const settled = yield* orchestrator.streamDomainEvents.pipe(
          Stream.filter(
            (event) =>
              event.type === "run.updated" &&
              event.payload.id === turnInput.runId &&
              event.payload.status === "completed",
          ),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped,
        );
        yield* Queue.offer(events, {
          type: "provider_turn.updated",
          driver,
          providerTurn: { ...turn, status: "completed", completedAt: yield* DateTime.now },
        });
        yield* Queue.offer(events, {
          type: "turn.terminal",
          driver,
          providerThreadId: turn.providerThreadId,
          providerTurnId: turn.id,
          runOrdinal: turnInput.runOrdinal,
          status: "completed",
          failure: null,
          threadDisposition: "reusable",
        });
        yield* Fiber.join(settled);
        yield* worker.drain();
        yield* orchestrator.resumeQueuedRuns;
        yield* worker.drain();
      });
      yield* finishTurn(0);
      const compactRunning = yield* nextRunning();
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("compact"),
        threadId,
        messageId: MessageId.make("message:compact"),
        text: "/compact",
        attachments: [],
        dispatchMode: { type: "start_immediately" },
        createdBy: "user",
        creationSource: "web",
      });
      yield* worker.drain();
      yield* Fiber.join(compactRunning);
      yield* lock;
      const sendFromAgent = (codeword: string) =>
        Effect.gen(function* () {
          const active = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
            (run) => run.status === "running",
          )!;
          yield* orchestrator.dispatch({
            type: "message.dispatch",
            commandId: CommandId.make(`send-${codeword}`),
            threadId,
            messageId: MessageId.make(`message:${codeword}`),
            text: codeword,
            attachments: [],
            dispatchMode: { type: "steer_active", targetRunId: active.id },
            deliveryIntent: "auto",
            createdBy: "agent",
            creationSource: "mcp",
            senderThreadId: ThreadId.make("thread:orchestrator"),
          });
          yield* worker.drain();
        });
      yield* sendFromAgent("ALPHA");
      yield* sendFromAgent("BRAVO");
      const queued = (yield* orchestrator.getThreadProjection(threadId)).runs.filter(
        (run) => run.status === "queued",
      );
      assert.lengthOf(queued, 2);
      assert.deepEqual(steered, []);

      const alphaRunning = yield* nextRunning();
      yield* finishTurn(1);
      yield* Fiber.join(alphaRunning);
      yield* lock;
      assert.deepEqual(
        started.map((turn) => turn.message.text.split("\n").at(-1)),
        ["first", "/compact", "ALPHA"],
      );

      yield* sendFromAgent("CHARLIE");
      const delivered = [...steered, ...started.slice(3).map((turn) => turn.message.text)]
        .map((text) => text.split("\n").at(-1))
        .filter((text) => text === "BRAVO" || text === "CHARLIE");
      // Whatever reaches the agent so far does so in send order: CHARLIE never before BRAVO.
      assert.deepEqual(delivered, ["BRAVO", "CHARLIE"].slice(0, delivered.length));
    }).pipe(Effect.provide(layer));
  }),
);

const linkedSender = (environmentId: string, threadId: string) => ({
  environmentId: EnvironmentId.make(environmentId),
  threadId: ThreadId.make(threadId),
  title: "Linked sender",
});

// regression: #41455 one sender's queued message holds back another sender's automatic steer
it.effect.each([
  {
    name: "steers past another thread's queued message",
    first: { senderThreadId: ThreadId.make("thread:sender-a") },
    urgent: { senderThreadId: ThreadId.make("thread:sender-c") },
    delivered: ["urgent"],
  },
  {
    name: "steers past another linked environment's queued message",
    first: { linkedSender: linkedSender("environment:a", "thread:sender") },
    urgent: { linkedSender: linkedSender("environment:b", "thread:sender") },
    delivered: ["urgent"],
  },
  {
    name: "steers past an unnamed agent's queued message",
    first: {},
    urgent: { linkedSender: linkedSender("environment:a", "thread:sender") },
    delivered: ["urgent"],
  },
  {
    name: "steers past a queued message when neither agent is named",
    first: {},
    urgent: {},
    delivered: ["urgent"],
  },
  {
    name: "waits behind its own linked thread's queued message",
    first: { linkedSender: linkedSender("environment:a", "thread:sender") },
    urgent: { linkedSender: linkedSender("environment:a", "thread:sender") },
    delivered: [],
  },
])("an agent's automatic send $name", (testCase) =>
  Effect.gen(function* () {
    const { started, steered, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
      "agent-steer-past-other-sender",
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const active = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
        (run) => run.status === "running",
      )!;
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("send-first"),
        threadId,
        messageId: MessageId.make("message:queued-first"),
        text: "queued first",
        attachments: [],
        dispatchMode: { type: "queue_after_active" },
        createdBy: "agent",
        creationSource: "mcp",
        ...testCase.first,
      });
      yield* worker.drain();
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("send-urgent"),
        threadId,
        messageId: MessageId.make("message:urgent"),
        text: "urgent",
        attachments: [],
        dispatchMode: { type: "steer_active", targetRunId: active.id },
        deliveryIntent: "auto",
        createdBy: "agent",
        creationSource: "mcp",
        ...testCase.urgent,
      });
      yield* worker.drain();

      assert.lengthOf(started, 1);
      assert.deepEqual(
        steered.map((text) => text.split("\n").at(-1)),
        testCase.delivered,
      );
    }).pipe(Effect.provide(layer));
  }),
);

// regression: #33454 a steer that restarts a completion wake run left its cohort claimed forever
const restartCompletionWake = Effect.fn("restartCompletionWake")(function* (
  harness: Pick<
    Effect.Success<ReturnType<typeof nextTurnSelectionHarness>>,
    "events" | "started" | "startFirstTurn"
  >,
  siblingFinishesFirst: boolean,
  userRunBeforeWake = false,
  intentNamesTarget = false,
) {
  const orchestrator = yield* Orchestrator.OrchestratorV2;
  const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
  const sink = yield* EventSink.EventSinkV2;
  const threadId = yield* harness.startFirstTurn;
  const watchRun = (runId: string, status: string) =>
    orchestrator.streamDomainEvents.pipe(
      Stream.filter(
        (event) =>
          event.type === "run.updated" &&
          event.payload.id === runId &&
          event.payload.status === status,
      ),
      Stream.take(1),
      Stream.runDrain,
      Effect.forkScoped,
    );
  const watchTurnRunning = (ordinal: number) =>
    orchestrator.streamDomainEvents.pipe(
      Stream.filter(
        (event) =>
          event.type === "provider-turn.updated" &&
          event.payload.status === "running" &&
          event.payload.ordinal === ordinal,
      ),
      Stream.take(1),
      Stream.runDrain,
      Effect.forkScoped,
    );
  const finishTurn = (turn: ProviderAdapter.ProviderAdapterV2TurnInput) =>
    Effect.gen(function* () {
      const providerTurn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns.find(
        (candidate) => candidate.runAttemptId === turn.attemptId,
      )!;
      const settled = yield* watchRun(turn.runId, "completed");
      yield* Queue.offer(harness.events, {
        type: "provider_turn.updated",
        driver,
        providerTurn: {
          ...providerTurn,
          status: "completed",
          completedAt: yield* DateTime.now,
        },
      });
      yield* Queue.offer(harness.events, {
        type: "turn.terminal",
        driver,
        providerThreadId: providerTurn.providerThreadId,
        providerTurnId: providerTurn.id,
        runOrdinal: turn.runOrdinal,
        status: "completed",
        failure: null,
        threadDisposition: "reusable",
      });
      yield* Fiber.join(settled);
      yield* worker.drain();
    });

  const origin = harness.started[0]!;
  const firstTaskId = NodeId.make("task:steered-wake-first");
  const siblingTaskId = NodeId.make("task:steered-wake-sibling");
  const now = yield* DateTime.now;
  yield* sink.write({
    events: [firstTaskId, siblingTaskId].map((taskId) => ({
      id: EventId.make(`event:seed:${taskId}`),
      type: "subagent.updated" as const,
      threadId,
      runId: origin.runId,
      nodeId: taskId,
      occurredAt: now,
      payload: {
        id: taskId,
        threadId,
        runId: origin.runId,
        parentNodeId: origin.rootNodeId,
        origin: "app_owned" as const,
        createdBy: "agent" as const,
        driver,
        providerInstanceId: instanceId,
        providerThreadId: null,
        childThreadId: null,
        remoteChild: {
          environmentId: EnvironmentId.make("environment:box"),
          threadId: ThreadId.make(`thread:remote:${taskId}`),
          label: "box",
        },
        nativeTaskRef: null,
        prompt: "Do background work",
        title: null,
        model: null,
        completionWake: "always" as const,
        completionDelivery: { state: "pending" as const, observedByRunId: null },
        status: "running" as const,
        result: null,
        startedAt: now,
        completedAt: null,
        updatedAt: now,
      },
    })),
  });
  yield* finishTurn(origin);
  // The wake queues behind a user run that started after the origin run finished.
  if (userRunBeforeWake) {
    const userRunning = yield* watchTurnRunning(2);
    yield* orchestrator.dispatch({
      type: "message.dispatch",
      commandId: CommandId.make("command:user-run"),
      threadId,
      messageId: MessageId.make("message:user-run"),
      text: "unrelated question",
      attachments: [],
      modelSelection: runSelection,
      dispatchMode: { type: "start_immediately" },
      createdBy: "user",
      creationSource: "web",
    });
    yield* worker.drain();
    yield* Fiber.join(userRunning);
  }

  const completeTask = (taskId: NodeId) =>
    orchestrator.dispatch({
      type: "delegated_task.remote.complete",
      commandId: CommandId.make(`command:remote-complete:${taskId}`),
      parentThreadId: threadId,
      taskId,
      status: "completed",
      result: `${taskId} finished`,
      stoppedThere: "ran-out",
    });
  yield* completeTask(firstTaskId);
  const firstDelivery = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
    (run) => run.id === origin.runId,
  )!.delegatedCompletion!.delivery!;
  assert.deepEqual(firstDelivery.taskIds, [firstTaskId]);
  yield* orchestrator.dispatch({
    type: "message.dispatch",
    commandId: CommandId.make("command:first-wake"),
    threadId,
    messageId: firstDelivery.messageId,
    text: "Delegated task completed",
    attachments: [],
    modelSelection: runSelection,
    dispatchMode: { type: "queue_after_active" },
    delegatedCompletion: {
      parentRunId: origin.runId,
      generation: firstDelivery.generation,
      taskIds: firstDelivery.taskIds,
    },
    createdBy: "agent",
    creationSource: "server",
  });
  const wakeOrdinal = userRunBeforeWake ? 3 : 2;
  const wakeRunning = yield* watchTurnRunning(wakeOrdinal);
  if (userRunBeforeWake) yield* finishTurn(harness.started[1]!);
  yield* orchestrator.resumeQueuedRuns;
  yield* worker.drain();
  yield* Fiber.join(wakeRunning);
  const wake = harness.started[wakeOrdinal - 1]!;
  assert.equal(wake.message.messageId, firstDelivery.messageId);
  if (userRunBeforeWake) {
    // The wake's run holds a compaction, so the origin run's records are older than the window.
    const compactedAt = yield* DateTime.now;
    yield* sink.write({
      events: [
        {
          id: EventId.make("event:wake-compaction"),
          type: "turn-item.updated" as const,
          threadId,
          runId: wake.runId,
          nodeId: wake.rootNodeId,
          occurredAt: compactedAt,
          payload: {
            id: TurnItemId.make("item:wake-compaction"),
            threadId,
            runId: wake.runId,
            nodeId: wake.rootNodeId,
            providerThreadId: null,
            providerTurnId: null,
            nativeItemRef: null,
            parentItemId: null,
            ordinal: 1_000,
            status: "completed" as const,
            title: null,
            startedAt: compactedAt,
            completedAt: compactedAt,
            updatedAt: compactedAt,
            type: "compaction" as const,
            driver,
          },
        },
      ],
    });
  }

  if (siblingFinishesFirst) {
    yield* completeTask(siblingTaskId);
  }

  // The provider cannot steer natively, so the steer restarts the wake run.
  const steeredRunning = yield* watchTurnRunning(wakeOrdinal + 1);
  yield* orchestrator.dispatch({
    type: "message.dispatch",
    commandId: CommandId.make("command:steer-wake"),
    threadId,
    messageId: MessageId.make("message:steer-wake"),
    text: "also check the logs",
    attachments: [],
    modelSelection: runSelection,
    ...(intentNamesTarget
      ? { dispatchMode: { type: "start_immediately" as const }, deliveryIntent: "restart" as const }
      : { dispatchMode: { type: "steer_active" as const, targetRunId: wake.runId } }),
    createdBy: "user",
    creationSource: "web",
  });
  yield* worker.drain();
  yield* Fiber.join(steeredRunning);
  const steered = harness.started[wakeOrdinal]!;
  assert.equal(steered.runId, wake.runId);
  assert.equal(
    (yield* orchestrator.getThreadProjection(threadId)).attempts.find(
      (attempt) => attempt.id === steered.attemptId,
    )?.reason,
    "steering_restart",
  );
  return {
    threadId,
    origin,
    wake,
    steered,
    firstTaskId,
    siblingTaskId,
    watchRun,
    finishTurn,
    completeTask,
  };
});

it.effect("wakes the parent for a sibling result after a steer restarts the completion wake", () =>
  Effect.gen(function* () {
    const harness = yield* nextTurnSelectionHarness("steered-completion-wake", true, false);
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const {
        threadId,
        origin,
        wake,
        steered,
        firstTaskId,
        siblingTaskId,
        watchRun,
        finishTurn,
        completeTask,
      } = yield* restartCompletionWake(harness, false);

      // A follow-up queued behind the wake starts once the terminal-run listener has settled it.
      const followUp = MessageId.make("message:follow-up");
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("command:follow-up"),
        threadId,
        messageId: followUp,
        text: "follow up",
        attachments: [],
        modelSelection: runSelection,
        dispatchMode: { type: "queue_after_active" },
        createdBy: "user",
        creationSource: "web",
      });
      const followUpRun = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
        (run) => run.userMessageId === followUp,
      )!;
      const followUpStarted = yield* watchRun(followUpRun.id, "starting");
      yield* finishTurn(steered);
      yield* Fiber.join(followUpStarted);
      assert.equal(
        (yield* orchestrator.getThreadProjection(threadId)).runs.find(
          (run) => run.id === wake.runId,
        )?.status,
        "completed",
      );

      yield* completeTask(siblingTaskId);
      const final = yield* orchestrator.getThreadProjection(threadId);
      const tasks = new Map(
        final.subagents.map((task) => [task.id, task.completionDelivery?.state]),
      );
      assert.deepInclude(
        final.runs.find((run) => run.id === origin.runId)?.delegatedCompletion?.delivery ?? {},
        { generation: 2, taskIds: [siblingTaskId] },
        "the sibling finished after the steered wake but no new completion wake was reserved",
      );
      assert.equal(tasks.get(siblingTaskId), "claimed");
      assert.equal(
        tasks.get(firstTaskId),
        "delivered",
        "the steered wake run's own task stays claimed after that run completed",
      );
    }).pipe(Effect.provide(harness.layer));
  }),
);

it.effect.each([false, true])(
  "settles a steered completion wake whose parent run is before the last compaction (restart by intent: %s)",
  (intentNamesTarget) =>
    Effect.gen(function* () {
      const harness = yield* nextTurnSelectionHarness(
        `steered-wake-after-compaction-${intentNamesTarget}`,
        true,
        false,
      );
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const { threadId, steered, firstTaskId, finishTurn } = yield* restartCompletionWake(
          harness,
          false,
          true,
          intentNamesTarget,
        );
        yield* finishTurn(steered);
        const tasks = (yield* orchestrator.getThreadProjection(threadId)).subagents;
        assert.equal(
          tasks.find((task) => task.id === firstTaskId)?.completionDelivery?.state,
          "delivered",
        );
      }).pipe(Effect.provide(harness.layer));
    }),
);

it.effect(
  "offers the next completion wake when a steer restarts a wake a sibling finished behind",
  () =>
    Effect.gen(function* () {
      const harness = yield* nextTurnSelectionHarness(
        "steered-completion-wake-behind",
        true,
        false,
      );
      const offers: ProviderContinuationRequests.ProviderContinuationRequest[] = [];
      const requests = Layer.succeed(ProviderContinuationRequests.ProviderContinuationRequests, {
        offer: (request) => Effect.sync(() => void offers.push(request)),
        take: Effect.never,
      });
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const { threadId, origin, firstTaskId, siblingTaskId } = yield* restartCompletionWake(
          harness,
          true,
        );
        const projection = yield* orchestrator.getThreadProjection(threadId);
        const delivery = projection.runs.find((run) => run.id === origin.runId)?.delegatedCompletion
          ?.delivery;
        assert.deepInclude(delivery ?? {}, { generation: 2, taskIds: [siblingTaskId] });
        const tasks = new Map(
          projection.subagents.map((task) => [task.id, task.completionDelivery?.state]),
        );
        assert.equal(tasks.get(firstTaskId), "delivered");
        assert.equal(tasks.get(siblingTaskId), "claimed");
        assert.deepEqual(offers.at(-1)?.delegatedCompletion, {
          parentRunId: origin.runId,
          generation: 2,
          messageId: delivery!.messageId,
        });
      }).pipe(Effect.provide(harness.layer.pipe(Layer.provide(requests))));
    }),
);

// Codex flushes held drafts after the final answer; reserve their pre-steer position before turn end.
it.effect("keeps a held commentary draft before the steer when turn end flushes it", () =>
  Effect.gen(function* () {
    const { events, started, layer, startFirstTurn } =
      yield* nextTurnSelectionHarness("steering-held-draft");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const first = started[0]!;
      const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!;
      const now = yield* DateTime.now;
      const assistantItem = (id: string, text: string, streaming: boolean) =>
        ({
          type: "turn_item.updated",
          driver,
          turnItem: {
            id: TurnItemId.make(id),
            threadId,
            runId: first.runId,
            nodeId: first.rootNodeId,
            providerThreadId: turn.providerThreadId,
            providerTurnId: turn.id,
            nativeItemRef: null,
            parentItemId: null,
            ordinal: 0,
            status: streaming ? "running" : "completed",
            title: null,
            startedAt: now,
            completedAt: streaming ? null : now,
            updatedAt: now,
            type: "assistant_message",
            messageId: MessageId.make(`message:${id}`),
            text,
            streaming,
          },
        }) satisfies ProviderAdapter.ProviderAdapterV2Event;
      const watch = (predicate: (event: OrchestrationV2DomainEvent) => boolean) =>
        orchestrator.streamDomainEvents.pipe(
          Stream.filter(predicate),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped,
        );

      yield* Queue.offer(events, assistantItem("draft", "Let me look at the", true));
      const commentaryWritten = yield* watch(
        (event) => event.type === "turn-item.updated" && event.payload.id === "commentary",
      );
      yield* Queue.offer(events, assistantItem("commentary", "Checking the popover.", false));
      yield* Fiber.join(commentaryWritten);

      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("steer"),
        threadId,
        messageId: MessageId.make("message:steer"),
        text: "fix the popover",
        attachments: [],
        dispatchMode: { type: "steer_active", targetRunId: first.runId },
        createdBy: "user",
        creationSource: "web",
      });
      yield* worker.drain();

      const settled = yield* watch(
        (event) =>
          event.type === "run.updated" &&
          event.payload.id === first.runId &&
          event.payload.status === "completed",
      );
      yield* Queue.offer(events, assistantItem("answer", "All fixed.", false));
      yield* Queue.offer(events, assistantItem("draft", "Let me look at the", false));
      yield* Queue.offer(events, {
        type: "provider_turn.updated",
        driver,
        providerTurn: { ...turn, status: "completed", completedAt: yield* DateTime.now },
      });
      yield* Queue.offer(events, {
        type: "turn.terminal",
        driver,
        providerThreadId: turn.providerThreadId,
        providerTurnId: turn.id,
        runOrdinal: first.runOrdinal,
        status: "completed",
        failure: null,
        threadDisposition: "reusable",
      });
      yield* Fiber.join(settled);
      yield* worker.drain();

      const items = (yield* orchestrator.getThreadProjection(threadId)).turnItems
        .toSorted((left, right) => left.ordinal - right.ordinal)
        .flatMap((item) =>
          item.type === "user_message"
            ? [String(item.messageId)]
            : item.type === "assistant_message"
              ? [String(item.id)]
              : [],
        );
      assert.deepEqual(items, ["message:first", "draft", "commentary", "message:steer", "answer"]);
    }).pipe(Effect.provide(layer));
  }),
);

it.effect.each(["auto", "deferred", "promote-edited"] as const)(
  "rejects expanded steering input before commit: %s",
  (delivery) =>
    Effect.scoped(
      Effect.gen(function* () {
        const { started, steered, interrupted, layer, startFirstTurn } =
          yield* nextTurnSelectionHarness(`oversized-steer-${delivery}`, true);
        yield* Effect.gen(function* () {
          const orchestrator = yield* Orchestrator.OrchestratorV2;
          const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
          const outbox = yield* EffectOutbox.EffectOutboxV2;
          const sink = yield* EventSink.EventSinkV2;
          const threadId = yield* startFirstTurn;
          const first = started[0]!;
          const citation = serializeAssistantCitation({
            version: 1,
            environmentId: EnvironmentId.make("env"),
            threadId,
            messageId: MessageId.make("assistant"),
            text: "\u0001".repeat(8_000),
            start: 0,
            end: 8_000,
            prefix: "",
            suffix: "",
          });
          const text = `${"x".repeat(80_000)}${citation}`;
          // Context excerpts are not part of the limit, matching the web composer's check.
          const largeContextText =
            "Read [one](oni-context://v1/terminal/one) and [two](oni-context://v1/terminal/two)";
          const context = {
            version: 1 as const,
            records: (["one", "two"] as const).map((id) => ({
              version: 1 as const,
              kind: "terminal" as const,
              contextId: ComposerContextId.make(id),
              label: id,
              terminalId: "terminal",
              terminalLabel: "Terminal",
              lineStart: 1,
              lineEnd: 1,
              text: "y".repeat(COMPOSER_CONTEXT_TERMINAL_TEXT_MAX_CHARS),
            })),
          };
          assert.isBelow(text.length, PROVIDER_SEND_TURN_MAX_INPUT_CHARS);
          assert.isAbove(
            expandAssistantCitationsForProvider(text).length,
            PROVIDER_SEND_TURN_MAX_INPUT_CHARS,
          );
          assert.isAbove(
            projectComposerContextForProvider({
              text: largeContextText,
              records: context.records,
            }).length,
            PROVIDER_SEND_TURN_MAX_INPUT_CHARS,
          );
          if (delivery === "deferred") {
            const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!;
            yield* sink.write({
              events: [
                {
                  id: EventId.make("turn-not-ready"),
                  type: "provider-turn.updated",
                  threadId,
                  occurredAt: yield* DateTime.now,
                  payload: { ...turn, status: "pending" },
                },
              ],
            });
          }
          const commandId = CommandId.make("oversized-steer");
          let command: Parameters<typeof orchestrator.dispatch>[0] = {
            type: "message.dispatch",
            commandId,
            threadId,
            messageId: MessageId.make("oversized"),
            text,
            attachments: [],
            dispatchMode: { type: "start_immediately" },
            deliveryIntent: delivery === "deferred" ? "steer" : "auto",
            createdBy: "user",
            creationSource: "mobile",
          };
          if (delivery === "promote-edited") {
            yield* orchestrator.dispatch({
              ...command,
              commandId: CommandId.make("queue"),
              text: "Initially valid queued message",
              dispatchMode: { type: "queue_after_active" },
              deliveryIntent: undefined,
            });
            const queued = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
              (run) => run.status === "queued",
            )!;
            yield* orchestrator.dispatch({
              type: "queued-run.edit",
              commandId: CommandId.make("edit"),
              threadId,
              runId: queued.id,
              text,
            });
            command = {
              type: "queued-message.promote-to-steer",
              commandId,
              threadId,
              queuedRunId: queued.id,
              targetRunId: first.runId,
            };
          }
          const before = yield* orchestrator.getThreadProjection(threadId);
          const sequence = yield* orchestrator.getThreadEventSequence(threadId);
          const error = yield* orchestrator.dispatch(command).pipe(Effect.flip);
          assert.instanceOf(error, Orchestrator.OrchestratorCommandRejectedError);
          assert.equal(
            userFacingDispatchErrorMessage(error),
            "Message exceeds 120,000 characters after expanding quotes. Shorten it and try again.",
          );
          assert.deepEqual(yield* outbox.listByCommandId(commandId), []);
          assert.deepEqual(yield* sink.readByCommandId({ commandId }).pipe(Stream.runCollect), []);
          yield* worker.drain();
          assert.equal(yield* orchestrator.getThreadEventSequence(threadId), sequence);
          assert.deepEqual(yield* orchestrator.getThreadProjection(threadId), before);
          assert.deepEqual(steered, []);
          assert.deepEqual(interrupted, []);
          assert.lengthOf(started, 1);
          if (delivery === "auto") {
            yield* orchestrator.dispatch({
              type: "message.dispatch",
              commandId: CommandId.make("large-context-steer"),
              threadId,
              messageId: MessageId.make("large-context"),
              text: largeContextText,
              context,
              attachments: [],
              dispatchMode: { type: "start_immediately" },
              deliveryIntent: "auto",
              createdBy: "user",
              creationSource: "mobile",
            });
            yield* worker.drain();
            assert.lengthOf(steered, 1);
          }
        }).pipe(Effect.provide(layer));
      }),
    ),
);

it.effect("steers a promoted queued message with the selection it was queued with", () =>
  Effect.gen(function* () {
    const { started, steered, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
      "promote-queued-selection",
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("queued"),
        threadId,
        messageId: MessageId.make("message:queued"),
        text: "queued",
        attachments: [],
        modelSelection: composerSelection,
        dispatchMode: { type: "queue_after_active" },
        createdBy: "user",
        creationSource: "web",
      });
      const queuedRun = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
        (run) => run.status === "queued",
      )!;
      yield* orchestrator.dispatch({
        type: "queued-message.promote-to-steer",
        commandId: CommandId.make("promote"),
        threadId,
        queuedRunId: queuedRun.id,
        targetRunId: started[0]!.runId,
      });
      yield* worker.drain();

      const projection = yield* orchestrator.getThreadProjection(threadId);
      assert.deepEqual(steered, ["queued"]);
      assert.deepEqual(projection.thread.modelSelection, composerSelection);
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("steers a promoted scheduled delivery with the thread's current selection", () =>
  Effect.scoped(
    Effect.gen(function* () {
      const { started, steered, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
        "promote-scheduled-selection",
      );
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const threadId = yield* startFirstTurn;
        // Bound scheduled tasks send like ScheduledTaskService: no model of their own.
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make("scheduled"),
          threadId,
          messageId: MessageId.make("message:scheduled"),
          scheduledTaskId: ScheduledTaskId.make("task:scheduled"),
          text: "scheduled",
          attachments: [],
          dispatchMode: { type: "queue_after_active" },
          createdBy: "user",
          creationSource: "web",
        });
        const queuedRun = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
          (run) => run.status === "queued",
        )!;
        assert.deepEqual(queuedRun.modelSelection, runSelection);
        yield* orchestrator.dispatch({
          type: "thread.model-selection.set",
          commandId: CommandId.make("select-composer"),
          threadId,
          modelSelection: composerSelection,
        });
        yield* orchestrator.dispatch({
          type: "queued-message.promote-to-steer",
          commandId: CommandId.make("promote"),
          threadId,
          queuedRunId: queuedRun.id,
          targetRunId: started[0]!.runId,
        });
        yield* worker.drain();

        const projection = yield* orchestrator.getThreadProjection(threadId);
        assert.deepEqual(steered, [
          '[Onicode: scheduled task "task:scheduled" sent this prompt, not the user typing in this chat.]\n\nscheduled',
        ]);
        assert.deepEqual(projection.thread.modelSelection, composerSelection);
      }).pipe(Effect.provide(layer));
    }),
  ),
);

it.effect("steers a deferred agent message on a pick made while it waited", () =>
  Effect.scoped(
    Effect.gen(function* () {
      const { started, steered, steeredSelections, layer, startFirstTurn, events } =
        yield* nextTurnSelectionHarness("deferred-agent-steer-selection");
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const threadId = yield* startFirstTurn;
        const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!;
        yield* (yield* EventSink.EventSinkV2).write({
          events: [
            {
              id: EventId.make("turn-not-ready"),
              type: "provider-turn.updated",
              threadId,
              occurredAt: yield* DateTime.now,
              payload: { ...turn, status: "pending" },
            },
          ],
        });
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make("agent-steer"),
          threadId,
          messageId: MessageId.make("message:agent-steer"),
          text: "agent steer",
          attachments: [],
          dispatchMode: { type: "steer_active", targetRunId: started[0]!.runId },
          createdBy: "agent",
          creationSource: "mcp",
        });
        const deferred = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
          (run) => run.status === "queued",
        )!;
        assert.equal(deferred.steerWhenReadyRunId, started[0]!.runId);
        yield* orchestrator.dispatch({
          type: "thread.model-selection.set",
          commandId: CommandId.make("select-composer"),
          threadId,
          modelSelection: composerSelection,
        });
        const promoted = yield* orchestrator.streamDomainEvents.pipe(
          Stream.filter(
            (event) =>
              event.type === "run.updated" &&
              event.payload.id === deferred.id &&
              event.payload.status === "cancelled",
          ),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped,
        );
        yield* Queue.offer(events, { type: "provider_turn.updated", driver, providerTurn: turn });
        yield* Fiber.join(promoted);
        yield* worker.drain();

        assert.lengthOf(steered, 1);
        assert.deepEqual(steeredSelections, [composerSelection]);
        const projection = yield* orchestrator.getThreadProjection(threadId);
        assert.deepEqual(projection.thread.modelSelection, composerSelection);
        // Should the turn leave it unread, it is queued again on the pick of that moment.
        assert.isTrue(
          projection.messages.find(
            (message) => message.id === MessageId.make("message:agent-steer"),
          )?.followsThreadSelection,
        );
      }).pipe(Effect.provide(layer));
    }),
  ),
);

const claudeSelection = {
  instanceId: ProviderInstanceId.make("claudeAgent"),
  model: "claude-test",
};
const claudeAdapter: ProviderAdapter.ProviderAdapterV2["Service"] = {
  instanceId: claudeSelection.instanceId,
  driver: ProviderDriverKind.make("claudeAgent"),
  getCapabilities: () => Effect.succeed(CodexProviderCapabilitiesV2),
  planSelectionTransition: () => Effect.succeed({ type: "apply_on_next_turn" }),
  openSession: () => Effect.die("the Claude instance never starts a turn here"),
};

// Queues a scheduled delivery behind the Codex turn, stops that turn (Stop holds the queue), then
// switches the thread to Claude: a thread changes instance only while no run is active.
const scheduledDeliveryAfterClaudeSwitch = (name: string, text: string) =>
  Effect.gen(function* () {
    const harness = yield* nextTurnSelectionHarness(
      name,
      false,
      true,
      Effect.void,
      true,
      undefined,
      undefined,
      [claudeAdapter],
    );
    const queueAndSwitch = Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const threadId = yield* harness.startFirstTurn;
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("scheduled"),
        threadId,
        messageId: MessageId.make("message:scheduled"),
        scheduledTaskId: ScheduledTaskId.make("task:scheduled"),
        text,
        attachments: [],
        dispatchMode: { type: "queue_after_active" },
        createdBy: "user",
        creationSource: "web",
      });
      const queuedRun = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
        (run) => run.status === "queued",
      )!;
      assert.equal(queuedRun.providerInstanceId, instanceId);
      // A prompt pinned to the Codex instance waits behind it.
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("pinned"),
        threadId,
        messageId: MessageId.make("message:pinned"),
        text: "pinned",
        attachments: [],
        modelSelection: runSelection,
        dispatchMode: { type: "queue_after_active" },
        createdBy: "user",
        creationSource: "web",
      });
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const first = harness.started[0]!;
      yield* orchestrator.dispatch({
        type: "run.interrupt",
        commandId: CommandId.make("stop"),
        threadId,
        runId: first.runId,
        holdQueue: true,
      });
      yield* worker.drain();
      const stopped = (yield* orchestrator.getThreadProjection(threadId)).runs;
      assert.equal(stopped.find((run) => run.id === first.runId)?.status, "interrupted");
      const held = stopped.find((run) => run.id === queuedRun.id);
      assert.equal(held?.status, "queued");
      assert.isTrue(held?.queueHeld);
      yield* orchestrator.dispatch({
        type: "thread.model-selection.set",
        commandId: CommandId.make("select-claude"),
        threadId,
        modelSelection: claudeSelection,
      });
      return { threadId, queuedRun };
    });
    return { ...harness, queueAndSwitch };
  });

it.effect("checks an edited scheduled /goal against the thread's current provider", () =>
  Effect.scoped(
    Effect.gen(function* () {
      const { layer, queueAndSwitch } = yield* scheduledDeliveryAfterClaudeSwitch(
        "edit-scheduled-goal",
        "scheduled",
      );
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const { threadId, queuedRun } = yield* queueAndSwitch;
        const editError = yield* orchestrator
          .dispatch({
            type: "queued-run.edit",
            commandId: CommandId.make("edit"),
            threadId,
            runId: queuedRun.id,
            text: "/goal pause",
          })
          .pipe(Effect.flip);

        assert.match(String(editError.cause), /Claude goals cannot be paused or resumed/);
        assert.equal(
          (yield* orchestrator.getThreadProjection(threadId)).messages.find(
            (message) => message.id === queuedRun.userMessageId,
          )?.text,
          "scheduled",
        );
      }).pipe(Effect.provide(layer));
    }),
  ),
);

it.effect("checks a queued scheduled /goal against the thread's current provider at start", () =>
  Effect.scoped(
    Effect.gen(function* () {
      const { layer, queueAndSwitch } = yield* scheduledDeliveryAfterClaudeSwitch(
        "start-scheduled-goal",
        "/goal pause",
      );
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const { threadId, queuedRun } = yield* queueAndSwitch;
        yield* orchestrator.dispatch({
          type: "queue.resume",
          commandId: CommandId.make("resume"),
          threadId,
        });
        yield* orchestrator.resumeQueuedRuns;
        yield* worker.drain();

        const projection = yield* orchestrator.getThreadProjection(threadId);
        const failure = projection.turnItems.find(
          (item) => item.type === "error" && item.runId === queuedRun.id,
        );
        assert.equal(projection.runs.find((run) => run.id === queuedRun.id)?.status, "failed");
        // The refusal is the message's own fault, so the prompt behind it still starts.
        const pinned = projection.runs.find(
          (run) => run.userMessageId === MessageId.make("message:pinned"),
        );
        assert.equal(pinned?.status, "starting");
        assert.isNotTrue(pinned?.queueHeld);
        assert.deepEqual(
          failure?.type === "error"
            ? { code: failure.failure.code, message: failure.failure.message }
            : null,
          {
            code: "queued_start_failed",
            message:
              "Claude goals cannot be paused or resumed. Send /goal clear to remove the goal.",
          },
        );
      }).pipe(Effect.provide(layer));
    }),
  ),
);

it.effect("marks the model switch on a promoted message that restarts the turn", () =>
  Effect.gen(function* () {
    const { started, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
      "promote-restart-switch",
      true,
      false,
    );
    const otherSelection = { ...runSelection, model: "other-model" };
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("queued"),
        threadId,
        messageId: MessageId.make("message:queued"),
        text: "queued",
        attachments: [],
        modelSelection: otherSelection,
        dispatchMode: { type: "queue_after_active" },
        createdBy: "user",
        creationSource: "web",
      });
      const queuedRun = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
        (run) => run.status === "queued",
      )!;
      yield* orchestrator.dispatch({
        type: "queued-message.promote-to-steer",
        commandId: CommandId.make("promote"),
        threadId,
        queuedRunId: queuedRun.id,
        targetRunId: started[0]!.runId,
      });
      yield* worker.drain();

      const promoted = (yield* orchestrator.getThreadProjection(threadId)).turnItems.find(
        (item) => item.type === "user_message" && item.messageId === "message:queued",
      );
      assert.deepEqual(promoted?.type === "user_message" ? promoted.modelSwitch : undefined, {
        from: runSelection,
        to: otherSelection,
      });
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("keeps a queued message queued when promoted into a run that Stop reached", () =>
  Effect.gen(function* () {
    const { started, steered, layer, startFirstTurn } =
      yield* nextTurnSelectionHarness("steering-after-stop");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const first = started[0]!;
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("queued"),
        threadId,
        messageId: MessageId.make("message:queued"),
        text: "queued",
        attachments: [],
        modelSelection: runSelection,
        dispatchMode: { type: "queue_after_active" },
        createdBy: "user",
        creationSource: "web",
      });
      const queuedRun = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
        (run) => run.status === "queued",
      )!;
      // The worker has not delivered the interrupt yet, so the run still reads as running.
      yield* orchestrator.dispatch({
        type: "run.interrupt",
        commandId: CommandId.make("stop"),
        threadId,
        runId: first.runId,
        holdQueue: true,
      });

      const promoteError = yield* orchestrator
        .dispatch({
          type: "queued-message.promote-to-steer",
          commandId: CommandId.make("promote"),
          threadId,
          queuedRunId: queuedRun.id,
          targetRunId: first.runId,
        })
        .pipe(Effect.flip);
      yield* worker.drain();

      const projection = yield* orchestrator.getThreadProjection(threadId);
      assert.equal(promoteError._tag, "OrchestratorDispatchError");
      assert.deepEqual(steered, []);
      const queued = projection.runs.find((run) => run.id === queuedRun.id);
      assert.equal(queued?.status, "queued");
      assert.isTrue(queued?.queueHeld);
    }).pipe(Effect.provide(layer));
  }),
);

// regression: #32891 a restart that Stop already reached is lost
it.effect.each([false, true])(
  "refuses a steer after Stop (provider turn ready: %s)",
  (providerTurnReady) =>
    Effect.gen(function* () {
      const { started, steered, layer, startFirstTurn } =
        yield* nextTurnSelectionHarness("restart-after-stop");
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const threadId = yield* startFirstTurn;
        const first = started[0]!;
        // The worker has not delivered the interrupt yet, so the run still reads as running.
        yield* orchestrator.dispatch({
          type: "run.interrupt",
          commandId: CommandId.make("stop"),
          threadId,
          runId: first.runId,
        });
        const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!;
        if (!providerTurnReady) {
          yield* (yield* EventSink.EventSinkV2).write({
            events: [
              {
                id: EventId.make("turn-not-ready"),
                type: "provider-turn.updated",
                threadId,
                occurredAt: yield* DateTime.now,
                payload: { ...turn, status: "pending" },
              },
            ],
          });
        }
        const steerError = yield* orchestrator
          .dispatch({
            type: "message.dispatch",
            commandId: CommandId.make("steer"),
            threadId,
            messageId: MessageId.make("message:steer"),
            text: "steer",
            attachments: [],
            modelSelection: runSelection,
            dispatchMode: { type: "steer_active", targetRunId: first.runId },
            createdBy: "user",
            creationSource: "web",
          })
          .pipe(Effect.flip);
        assert.equal(steerError._tag, "OrchestratorDispatchError");
        assert.include(String(steerError.cause), "is stopping and cannot be steered");
        if (!providerTurnReady) {
          yield* (yield* EventSink.EventSinkV2).write({
            events: [
              {
                id: EventId.make("restore-ready-turn"),
                type: "provider-turn.updated",
                threadId,
                occurredAt: yield* DateTime.now,
                payload: turn,
              },
            ],
          });
        }
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make("restart"),
          threadId,
          messageId: MessageId.make("message:restart"),
          text: "restart",
          attachments: [],
          modelSelection: runSelection,
          dispatchMode: { type: "restart_active", targetRunId: first.runId },
          createdBy: "user",
          creationSource: "web",
        });
        yield* worker.drain();

        assert.deepEqual(steered, []);
        assert.deepEqual(
          started.map((turn) => turn.message.text),
          ["first", "restart"],
        );
      }).pipe(Effect.provide(layer));
    }),
);

// regression: #32891 a restart that arrives after the turn ended is lost
it.effect.each([
  { status: "completed", settled: true },
  { status: "completed", settled: false },
  { status: "interrupted", settled: true },
  { status: "interrupted", settled: false },
  { status: "cancelled", settled: false },
] as const)(
  "starts a restart that missed the turn ($status, run settled: $settled) as a new turn",
  ({ status, settled: runSettled }) =>
    Effect.gen(function* () {
      const { events, started, steered, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
        `restart-after-${status}-${runSettled ? "settled" : "unsettled"}`,
      );
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const threadId = yield* startFirstTurn;
        const first = started[0]!;
        const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!;
        const turnEnded = yield* orchestrator.streamDomainEvents.pipe(
          Stream.filter(
            (event) =>
              event.type === "provider-turn.updated" &&
              event.payload.id === turn.id &&
              event.payload.status === status,
          ),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped,
        );
        yield* Queue.offer(events, {
          type: "provider_turn.updated",
          driver,
          providerTurn: { ...turn, status, completedAt: yield* DateTime.now },
        });
        yield* Fiber.join(turnEnded);
        const settleRun = Effect.gen(function* () {
          const settled = yield* orchestrator.streamDomainEvents.pipe(
            Stream.filter(
              (event) =>
                event.type === "run.updated" &&
                event.payload.id === first.runId &&
                event.payload.status !== "running",
            ),
            Stream.take(1),
            Stream.runDrain,
            Effect.forkScoped,
          );
          yield* Queue.offer(events, {
            type: "turn.terminal",
            driver,
            providerThreadId: turn.providerThreadId,
            providerTurnId: turn.id,
            runOrdinal: first.runOrdinal,
            status,
            failure: null,
            threadDisposition: "reusable",
          });
          yield* Fiber.join(settled);
          yield* worker.drain();
        });
        if (runSettled) yield* settleRun;

        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make("restart"),
          threadId,
          messageId: MessageId.make("message:restart"),
          text: "restart",
          attachments: [],
          modelSelection: runSelection,
          dispatchMode: { type: "restart_active", targetRunId: first.runId },
          createdBy: "user",
          creationSource: "web",
        });
        yield* worker.drain();
        if (!runSettled) {
          const restartLeftQueue = yield* orchestrator.streamDomainEvents.pipe(
            Stream.filter(
              (event) =>
                event.type === "run.updated" &&
                event.payload.userMessageId === "message:restart" &&
                event.payload.status !== "queued",
            ),
            Stream.take(1),
            Stream.runDrain,
            Effect.forkScoped,
          );
          yield* settleRun;
          yield* Fiber.join(restartLeftQueue);
          yield* worker.drain();
        }

        assert.deepEqual(steered, []);
        assert.deepEqual(
          started.map((turn) => turn.message.text),
          ["first", "restart"],
        );
      }).pipe(Effect.provide(layer));
    }),
);

it.effect("completes a run restarted while its turn was ending", () =>
  Effect.gen(function* () {
    const { events, started, layer, startFirstTurn, selectionGate } =
      yield* nextTurnSelectionHarness("restart-races-turn-end", true);
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const first = started[0]!;
      const endTurn = (turn: ProviderAdapter.ProviderAdapterV2TurnInput) =>
        Effect.gen(function* () {
          const waiting = yield* orchestrator.streamDomainEvents.pipe(
            Stream.filter(
              (event) =>
                event.type === "run.updated" &&
                event.payload.id === turn.runId &&
                event.payload.activeAttemptId === turn.attemptId &&
                event.payload.status === "completed",
            ),
            Stream.take(1),
            Stream.runDrain,
            Effect.forkScoped,
          );
          const providerTurn = (yield* orchestrator.getThreadProjection(
            threadId,
          )).providerTurns.find((candidate) => candidate.runAttemptId === turn.attemptId)!;
          yield* Queue.offer(events, {
            type: "provider_turn.updated",
            driver,
            providerTurn: {
              ...providerTurn,
              status: "completed",
              completedAt: yield* DateTime.now,
            },
          });
          yield* Queue.offer(events, {
            type: "turn.terminal",
            driver,
            providerThreadId: providerTurn.providerThreadId,
            providerTurnId: providerTurn.id,
            runOrdinal: turn.runOrdinal,
            status: "completed",
            failure: null,
            threadDisposition: "reusable",
          });
          yield* Fiber.join(waiting);
        });

      const paused = yield* Deferred.make<void>();
      const resume = yield* Deferred.make<void>();
      selectionGate.entered = Deferred.succeed(paused, undefined).pipe(
        Effect.andThen(Deferred.await(resume)),
      );
      const restarting = yield* orchestrator
        .dispatch({
          type: "message.dispatch",
          commandId: CommandId.make("restart"),
          threadId,
          messageId: MessageId.make("message:restart"),
          text: "restart",
          attachments: [],
          modelSelection: composerSelection,
          dispatchMode: { type: "restart_active", targetRunId: first.runId },
          createdBy: "user",
          creationSource: "web",
        })
        .pipe(Effect.forkChild);
      // The restart read the run as running; its turn ends and the run waits before it commits.
      yield* Deferred.await(paused);
      selectionGate.entered = Effect.void;
      yield* endTurn(first);
      const restartRunning = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) =>
            event.type === "provider-turn.updated" &&
            event.payload.status === "running" &&
            event.payload.runAttemptId !== first.attemptId,
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      yield* Deferred.succeed(resume, undefined);
      yield* Fiber.join(restarting);
      yield* worker.drain();
      assert.deepEqual(
        started.map((turn) => turn.message.text),
        ["first", "restart"],
      );
      yield* Fiber.join(restartRunning);
      yield* (yield* ThreadCommandExecutor.ThreadCommandExecutor).withLock(threadId, Effect.void);

      yield* endTurn(started[1]!);
      yield* worker.drain();
      const projection = yield* orchestrator.getThreadProjection(threadId);
      assert.equal(projection.runs.find((run) => run.id === first.runId)?.status, "completed");
    }).pipe(Effect.provide(layer));
  }),
);

// regression: #32891 a restart into a failed provider turn is lost
it.effect("queues a restart sent into a provider turn that failed before its run settled", () =>
  Effect.gen(function* () {
    const { events, started, steered, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
      "restart-after-failed-unsettled",
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const first = started[0]!;
      const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!;
      const turnFailed = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) =>
            event.type === "provider-turn.updated" &&
            event.payload.id === turn.id &&
            event.payload.status === "failed",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      yield* Queue.offer(events, {
        type: "provider_turn.updated",
        driver,
        providerTurn: { ...turn, status: "failed", completedAt: yield* DateTime.now },
      });
      yield* Fiber.join(turnFailed);

      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("restart"),
        threadId,
        messageId: MessageId.make("message:restart"),
        text: "restart",
        attachments: [],
        modelSelection: runSelection,
        dispatchMode: { type: "restart_active", targetRunId: first.runId },
        createdBy: "user",
        creationSource: "web",
      });
      yield* worker.drain();

      assert.deepEqual(steered, []);
      const projection = yield* orchestrator.getThreadProjection(threadId);
      assert.equal(
        projection.runs.find((run) => run.userMessageId === "message:restart")?.status,
        "queued",
      );
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("queues a plain send behind the stopped queue and releases its hold", () =>
  Effect.gen(function* () {
    const interruptReached = yield* Deferred.make<void>();
    const releaseInterrupt = yield* Deferred.make<void>();
    const { started, steered, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
      "plain-send-after-stop",
      false,
      true,
      Deferred.succeed(interruptReached, undefined).pipe(
        Effect.andThen(Deferred.await(releaseInterrupt)),
      ),
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const send = (id: string, delivery: "queue" | "auto") =>
        orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make(id),
          threadId,
          messageId: MessageId.make(`message:${id}`),
          text: id,
          attachments: [],
          modelSelection: runSelection,
          ...(delivery === "queue"
            ? { dispatchMode: { type: "queue_after_active" } }
            : { dispatchMode: { type: "start_immediately" }, deliveryIntent: "auto" }),
          createdBy: "user",
          creationSource: "web",
        });
      yield* send("queued", "queue");
      yield* orchestrator.dispatch({
        type: "run.interrupt",
        commandId: CommandId.make("stop"),
        threadId,
        runId: started[0]!.runId,
        holdQueue: true,
      });
      // The provider has not stopped yet, so a plain send still resolves to a steer.
      yield* send("sent", "auto");
      // Once the stop settles, the run-update reactor promotes the queue on
      // its own fiber, so read the queue while the provider is still stopping.
      const draining = yield* worker.drain().pipe(Effect.forkScoped);
      yield* Deferred.await(interruptReached);

      const projection = yield* orchestrator.getThreadProjection(threadId);
      assert.equal(started.length, 1);
      const queued = projection.runs
        .filter((run) => run.status === "queued")
        .sort((left, right) => (left.queuePosition ?? 0) - (right.queuePosition ?? 0));
      assert.deepEqual(
        queued.map(
          (run) => projection.messages.find((message) => message.id === run.userMessageId)?.text,
        ),
        ["queued", "sent"],
      );
      // The user sent it, so the queue they stopped runs again once the provider stops.
      assert.isFalse(queued.some((run) => run.queueHeld === true));
      yield* Deferred.succeed(releaseInterrupt, undefined);
      yield* Fiber.join(draining);
      assert.deepEqual(steered, []);
    }).pipe(Effect.provide(layer));
  }),
);

it.effect(
  "ends the run as interrupted when the provider finishes before an accepted Stop reaches it",
  () =>
    Effect.gen(function* () {
      const interruptReached = yield* Deferred.make<void>();
      const releaseInterrupt = yield* Deferred.make<void>();
      const { events, started, layer, startFirstTurn } = yield* nextTurnSelectionHarness(
        "stop-loses-to-completion",
        false,
        true,
        Deferred.succeed(interruptReached, undefined).pipe(
          Effect.andThen(Deferred.await(releaseInterrupt)),
        ),
      );
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const threadId = yield* startFirstTurn;
        const first = started[0]!;
        yield* orchestrator.dispatch({
          type: "run.interrupt",
          commandId: CommandId.make("stop"),
          threadId,
          runId: first.runId,
        });
        const draining = yield* worker.drain().pipe(Effect.forkScoped);
        yield* Deferred.await(interruptReached);
        const completed = yield* orchestrator.streamDomainEvents.pipe(
          Stream.filter(
            (event) =>
              event.type === "run.updated" &&
              event.payload.id === first.runId &&
              event.payload.status === "completed",
          ),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped,
        );
        const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!;
        yield* Queue.offer(events, {
          type: "provider_turn.updated",
          driver,
          providerTurn: { ...turn, status: "completed", completedAt: yield* DateTime.now },
        });
        yield* Queue.offer(events, {
          type: "turn.terminal",
          driver,
          providerThreadId: turn.providerThreadId,
          providerTurnId: turn.id,
          runOrdinal: first.runOrdinal,
          status: "completed",
          failure: null,
          threadDisposition: "reusable",
        });
        yield* Fiber.join(completed);
        yield* Deferred.succeed(releaseInterrupt, undefined);
        yield* Fiber.join(draining);
        yield* worker.drain();

        const projection = yield* orchestrator.getThreadProjection(threadId);
        const run = projection.runs.find((candidate) => candidate.id === first.runId);
        assert.equal(run?.status, "interrupted");
        assert.isNotNull(run?.checkpointId);
        assert.deepEqual(
          projection.turnItems.flatMap((item) =>
            item.type === "run_interrupt_result" ? [item.message] : [],
          ),
          ["Stopped by user"],
        );
      }).pipe(Effect.provide(layer));
    }),
);

const goalHarness = Effect.fn("goalHarness")(function* (name: string, nativeGoals?: boolean) {
  const harness = yield* nextTurnSelectionHarness(
    name,
    false,
    true,
    Effect.void,
    true,
    nativeGoals,
  );
  const send = (id: string, text: string, delivery: "queue" | "auto", freshContext = false) =>
    Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make(id),
        threadId: ThreadId.make(`thread:${name}`),
        messageId: MessageId.make(`message:${id}`),
        text,
        attachments: [],
        modelSelection: runSelection,
        ...(freshContext ? { freshContext } : {}),
        ...(delivery === "queue"
          ? { dispatchMode: { type: "queue_after_active" } }
          : { dispatchMode: { type: "start_immediately" }, deliveryIntent: "auto" }),
        createdBy: "user",
        creationSource: "web",
      });
    });
  const read = (threadId: ThreadId) =>
    Effect.gen(function* () {
      const projection = yield* (yield* Orchestrator.OrchestratorV2).getThreadProjection(threadId);
      const textOf = (run: (typeof projection.runs)[number]) =>
        projection.messages.find((message) => message.id === run.userMessageId)?.text;
      return {
        queue: projection.runs
          .filter((run) => run.status === "queued")
          .toSorted((left, right) => (left.queuePosition ?? 0) - (right.queuePosition ?? 0))
          .map(textOf),
        started: harness.started.map((turn) => turn.message.text),
        stopRequested: projection.turnItems.some(
          (item) =>
            item.type === "run_interrupt_request" && item.runId === harness.started[0]?.runId,
        ),
      };
    });
  return { ...harness, send, read };
});

it.effect.each([undefined, false, true])(
  "goal dispatch stops the active turn only with native support: %s",
  (nativeGoals) =>
    Effect.gen(function* () {
      const { steered, layer, startFirstTurn, send, read } = yield* goalHarness(
        "goal-steer",
        nativeGoals,
      );
      yield* Effect.gen(function* () {
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const threadId = yield* startFirstTurn;
        yield* send("queued", "queued", "queue");
        yield* send("goal", "/goal all tests pass", "auto");
        yield* worker.drain();

        assert.deepEqual(steered, []);
        assert.deepEqual(yield* read(threadId), {
          queue: nativeGoals === false ? ["queued", "/goal all tests pass"] : ["queued"],
          started: nativeGoals === false ? ["first"] : ["first", "/goal all tests pass"],
          stopRequested: nativeGoals !== false,
        });
      }).pipe(Effect.provide(layer));
    }),
);

it.effect("starts a goal that names no model on the thread's model ahead of a queued pick", () =>
  Effect.gen(function* () {
    const { started, layer, startFirstTurn, read } = yield* goalHarness("goal-ahead-of-pick");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      for (const [id, text, modelSelection] of [
        ["picked", "picked", composerSelection],
        ["goal", "/goal all tests pass", undefined],
      ] as const) {
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make(id),
          threadId,
          messageId: MessageId.make(`message:${id}`),
          text,
          attachments: [],
          ...(modelSelection === undefined ? {} : { modelSelection }),
          dispatchMode:
            id === "picked" ? { type: "queue_after_active" } : { type: "start_immediately" },
          ...(id === "picked" ? {} : { deliveryIntent: "auto" as const }),
          createdBy: "user",
          creationSource: "web",
        });
      }
      yield* worker.drain();

      assert.deepEqual(yield* read(threadId), {
        queue: ["picked"],
        started: ["first", "/goal all tests pass"],
        stopRequested: true,
      });
      assert.deepEqual(started[1]?.modelSelection, runSelection);
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("restarts by stopping the running turn when the provider cannot interrupt-restart", () =>
  Effect.gen(function* () {
    const { started, steered, layer, startFirstTurn, send, read } = yield* goalHarness(
      "restart-without-interrupt-restart",
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      yield* send("queued", "queued", "queue");
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("restart"),
        threadId,
        messageId: MessageId.make("message:restart"),
        text: "do this instead",
        attachments: [],
        modelSelection: runSelection,
        dispatchMode: { type: "restart_active", targetRunId: started[0]!.runId },
        createdBy: "agent",
        creationSource: "server",
      });
      yield* worker.drain();

      assert.deepEqual(steered, []);
      assert.deepEqual(yield* read(threadId), {
        queue: ["queued"],
        started: ["first", "do this instead"],
        stopRequested: true,
      });
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("starts a restart that stops the running turn ahead of an older held queue", () =>
  Effect.gen(function* () {
    const { started, layer, startFirstTurn, send, read } =
      yield* goalHarness("restart-past-held-queue");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      yield* send("queued", "queued", "queue");
      yield* orchestrator.dispatch({
        type: "thread.stop",
        commandId: CommandId.make("stop"),
        threadId,
      });
      yield* worker.drain();
      const secondRunning = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) => event.type === "provider-turn.updated" && event.payload.status === "running",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("second"),
        threadId,
        messageId: MessageId.make("message:second"),
        text: "second",
        attachments: [],
        dispatchMode: { type: "start_immediately" },
        createdBy: "agent",
        creationSource: "mcp",
      });
      yield* worker.drain();
      yield* Fiber.join(secondRunning);
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("restart"),
        threadId,
        messageId: MessageId.make("message:restart"),
        text: "do this instead",
        attachments: [],
        dispatchMode: { type: "restart_active", targetRunId: started[1]!.runId },
        createdBy: "agent",
        creationSource: "mcp",
      });
      yield* worker.drain();

      const { queue, started: startedTexts } = yield* read(threadId);
      assert.deepEqual(
        { queue, started: startedTexts },
        {
          queue: ["queued"],
          started: [
            "first",
            ...["second", "do this instead"].map(
              (text) =>
                `[Onicode: this message came through Onicode's tools, not from the user typing in this chat.]\n\n${text}`,
            ),
          ],
        },
      );
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("reports a restart that stops the running turn as restarted", () =>
  Effect.gen(function* () {
    const { started, layer, startFirstTurn } = yield* goalHarness("restart-send-delivery");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const threadManagement = yield* ThreadManagementService.ThreadManagementService;
      const threadId = yield* startFirstTurn;
      const input = {
        projectId: ProjectId.make("project:restart-send-delivery"),
        commandId: CommandId.make("restart"),
        threadId,
        messageId: MessageId.make("message:restart"),
        text: "do this instead",
        attachments: [],
        mode: "restart",
        createdBy: "agent",
        creationSource: "mcp",
      } as const;
      const sent = yield* threadManagement.sendToThread(input);
      // Stop holds the replacement before it starts; a replay must still report the restart.
      yield* orchestrator.dispatch({
        type: "thread.stop",
        commandId: CommandId.make("stop-after-restart"),
        threadId,
      });
      const replayed = yield* threadManagement.sendToThread(input);

      assert.equal(started.length, 1);
      assert.deepEqual([sent.delivery, replayed.delivery], ["restarted", "restarted"]);
    }).pipe(
      Effect.provide(
        ThreadManagementService.layer.pipe(
          Layer.provideMerge(layer),
          Layer.provide(ServerSettings.layerTest()),
          Layer.provide(
            CommandReceiptStore.layer.pipe(Layer.provide(SqlitePersistence.layerMemory)),
          ),
        ),
      ),
    );
  }),
);

it.effect("replays an accepted steer after its target turn completes", () =>
  Effect.gen(function* () {
    const name = "steer-retry-after-completion";
    const { events, started, steered, layer, startFirstTurn } =
      yield* nextTurnSelectionHarness(name);
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadManagement = yield* ThreadManagementService.ThreadManagementService;
      const threadId = yield* startFirstTurn;
      const input = {
        projectId: ProjectId.make(`project:${name}`),
        commandId: CommandId.make("steer"),
        threadId,
        messageId: MessageId.make("message:steer"),
        text: "Please report the test result",
        attachments: [],
        mode: "steer",
        createdBy: "agent",
        creationSource: "mcp",
      } as const;
      const accepted = yield* threadManagement.sendToThread(input);
      assert.equal(accepted.delivery, "steered");
      yield* worker.drain();

      const waiting = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) =>
            event.type === "run.updated" &&
            event.payload.id === accepted.run.id &&
            event.payload.status === "completed",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped({ startImmediately: true }),
      );
      const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!;
      yield* Queue.offer(events, {
        type: "provider_turn.updated",
        driver,
        providerTurn: { ...turn, status: "completed", completedAt: yield* DateTime.now },
      });
      yield* Queue.offer(events, {
        type: "turn.terminal",
        driver,
        providerThreadId: turn.providerThreadId,
        providerTurnId: turn.id,
        runOrdinal: accepted.run.ordinal,
        status: "completed",
        failure: null,
        threadDisposition: "reusable",
      });
      yield* Fiber.join(waiting);
      yield* worker.drain();

      const replayed = yield* threadManagement.sendToThread(input);
      assert.equal(replayed.dispatch.replayed, true);
      assert.equal(replayed.delivery, accepted.delivery);
      assert.equal(replayed.message.id, accepted.message.id);
      assert.equal(replayed.run.id, accepted.run.id);
      assert.equal(replayed.run.status, "completed");

      const fresh = yield* threadManagement
        .sendToThread({
          ...input,
          commandId: CommandId.make("fresh-steer"),
          messageId: MessageId.make("message:fresh-steer"),
        })
        .pipe(Effect.flip);
      assert.equal(fresh._tag, "ThreadManagementNoSteerableRunError");
      yield* worker.drain();
      assert.deepEqual(steered, [
        `[Onicode: this message came through Onicode's tools, not from the user typing in this chat.]\n\n${input.text}`,
      ]);
      assert.lengthOf(started, 1);
    }).pipe(
      Effect.provide(
        ThreadManagementService.layer.pipe(
          Layer.provideMerge(layer),
          Layer.provide(ServerSettings.layerTest()),
          Layer.provide(
            CommandReceiptStore.layer.pipe(Layer.provide(SqlitePersistence.layerMemory)),
          ),
        ),
      ),
    );
  }),
);

it.effect("queues a goal clear without stopping work when no goal drives it", () =>
  Effect.gen(function* () {
    const { steered, layer, startFirstTurn, send, read } = yield* goalHarness("goal-clear");
    yield* Effect.gen(function* () {
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      yield* send("queued", "queued", "queue");
      yield* send("clear", "/goal clear", "auto");
      yield* worker.drain();

      assert.deepEqual(steered, []);
      assert.deepEqual(yield* read(threadId), {
        queue: ["queued", "/goal clear"],
        started: ["first"],
        stopRequested: false,
      });
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("promoting a queued /context moves it first and does not steer", () =>
  Effect.gen(function* () {
    const { started, steered, layer, startFirstTurn, send, read } =
      yield* goalHarness("slash-promote");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      yield* send("queued", "queued", "queue");
      yield* send("context", "/context", "queue");
      const queuedRun = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
        (run) => run.userMessageId === MessageId.make("message:context"),
      )!;
      yield* orchestrator.dispatch({
        type: "queued-message.promote-to-steer",
        commandId: CommandId.make("promote"),
        threadId,
        queuedRunId: queuedRun.id,
        targetRunId: started[0]!.runId,
      });
      yield* worker.drain();

      assert.deepEqual(steered, []);
      assert.deepEqual(yield* read(threadId), {
        queue: ["/context", "queued"],
        started: ["first"],
        stopRequested: false,
      });
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("restarts the turn for a slash command sent as an explicit restart", () =>
  Effect.gen(function* () {
    const { started, steered, interrupted, layer, startFirstTurn } =
      yield* nextTurnSelectionHarness("slash-restart", true);
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("restart-context"),
        threadId,
        messageId: MessageId.make("message:restart-context"),
        text: "/context",
        attachments: [],
        modelSelection: runSelection,
        dispatchMode: { type: "restart_active", targetRunId: started[0]!.runId },
        createdBy: "user",
        creationSource: "web",
      });
      yield* worker.drain();

      assert.deepEqual(steered, []);
      assert.lengthOf(interrupted, 1);
      assert.deepEqual(
        started.map((turn) => turn.message.text),
        ["first", "/context"],
      );
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("promotes a queued goal by stopping the running turn", () =>
  Effect.gen(function* () {
    const { started, steered, layer, startFirstTurn, send, read } =
      yield* goalHarness("goal-promote");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      yield* send("queued", "queued", "queue");
      yield* send("goal", "/goal ship it", "queue");
      const goalRun = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
        (run) => run.userMessageId === MessageId.make("message:goal"),
      )!;
      yield* orchestrator.dispatch({
        type: "queued-message.promote-to-steer",
        commandId: CommandId.make("promote"),
        threadId,
        queuedRunId: goalRun.id,
        targetRunId: started[0]!.runId,
      });
      yield* worker.drain();

      assert.deepEqual(steered, []);
      assert.deepEqual(yield* read(threadId), {
        queue: ["queued"],
        started: ["first", "/goal ship it"],
        stopRequested: true,
      });
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("stops the running turn for a goal an agent steers into it", () =>
  Effect.gen(function* () {
    const { started, steered, layer, startFirstTurn, read } = yield* goalHarness("goal-raw-steer");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      // Agent sends carry a raw steer target and no delivery intent.
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("goal"),
        threadId,
        messageId: MessageId.make("message:goal"),
        text: "/goal ship it",
        attachments: [],
        modelSelection: runSelection,
        dispatchMode: { type: "steer_active", targetRunId: started[0]!.runId },
        createdBy: "agent",
        creationSource: "server",
      });
      yield* worker.drain();

      assert.deepEqual(steered, []);
      assert.deepEqual(yield* read(threadId), {
        queue: [],
        started: ["first", "/goal ship it"],
        stopRequested: true,
      });
    }).pipe(Effect.provide(layer));
  }),
);

// A fresh message skips compaction as its own turn, which steering a running turn would not.
it.effect.each(["next", "/goal ship it", "fresh next"])(
  "promoting %s into a compaction stops it and sends it next",
  (promoted) =>
    Effect.gen(function* () {
      const { events, layer, send, read, started, steered, startFirstTurn } = yield* goalHarness(
        `compaction-promote-${promoted.length}`,
      );
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const threadId = yield* startFirstTurn;
        yield* send("compact", "/compact", "queue");
        yield* send("queued", "queued", "queue");
        yield* send("next", promoted, "queue", promoted.startsWith("fresh"));
        const settled = yield* orchestrator.streamDomainEvents.pipe(
          Stream.filter(
            (event) =>
              event.type === "run.updated" &&
              event.payload.id === started[0]!.runId &&
              event.payload.status === "completed",
          ),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped,
        );
        const compacting = yield* orchestrator.streamDomainEvents.pipe(
          Stream.filter(
            (event) =>
              event.type === "provider-turn.updated" &&
              event.payload.status === "running" &&
              event.payload.runAttemptId !== started[0]!.attemptId,
          ),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped,
        );
        const turn = (yield* orchestrator.getThreadProjection(threadId)).providerTurns[0]!;
        yield* Queue.offer(events, {
          type: "provider_turn.updated",
          driver,
          providerTurn: { ...turn, status: "completed", completedAt: yield* DateTime.now },
        });
        yield* Queue.offer(events, {
          type: "turn.terminal",
          driver,
          providerThreadId: turn.providerThreadId,
          providerTurnId: turn.id,
          runOrdinal: started[0]!.runOrdinal,
          status: "completed",
          failure: null,
          threadDisposition: "reusable",
        });
        yield* Fiber.join(settled);
        yield* worker.drain();
        yield* orchestrator.resumeQueuedRuns;
        yield* worker.drain();
        yield* Fiber.join(compacting);
        const compactRun = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
          (run) => run.userMessageId === MessageId.make("message:compact"),
        )!;
        const nextRun = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
          (run) => run.userMessageId === MessageId.make("message:next"),
        )!;
        yield* orchestrator.dispatch({
          type: "queued-message.promote-to-steer",
          commandId: CommandId.make("promote"),
          threadId,
          queuedRunId: nextRun.id,
          targetRunId: compactRun.id,
        });
        yield* worker.drain();

        const projection = yield* orchestrator.getThreadProjection(threadId);
        assert.deepEqual(steered, []);
        assert.isTrue(
          projection.turnItems.some(
            (item) => item.type === "run_interrupt_request" && item.runId === compactRun.id,
          ),
        );
        assert.deepEqual((yield* read(threadId)).queue, ["queued"]);
        assert.deepEqual(
          started.map((turn) => turn.message.text),
          ["first", "/compact", promoted],
        );
      }).pipe(Effect.provide(layer));
    }),
);

it.effect("stops a turn that is still starting when a goal is sent into it", () =>
  Effect.gen(function* () {
    const { cwd, layer, send, started } = yield* goalHarness("goal-during-start");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = ThreadId.make("thread:goal-during-start");
      yield* orchestrator.dispatch({
        type: "thread.create",
        commandId: CommandId.make("create"),
        threadId,
        projectId: ProjectId.make("project:goal-during-start"),
        title: "Goal during start",
        modelSelection: runSelection,
        runtimeMode: "full-access",
        interactionMode: "default",
        branch: null,
        worktreePath: cwd,
        createdBy: "user",
        creationSource: "web",
      });
      // The stopped run's end starts the queue, which then needs one more drain.
      const goalStarting = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) => event.type === "run.updated" && event.payload.status === "starting",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped({ startImmediately: true }),
      );
      yield* send("first", "first", "auto");
      yield* send("goal", "/goal all tests pass", "auto");
      yield* worker.drain();
      yield* Fiber.join(goalStarting);
      yield* worker.drain();

      const projection = yield* orchestrator.getThreadProjection(threadId);
      const statusOf = (text: string) =>
        projection.runs.find(
          (run) =>
            projection.messages.find((message) => message.id === run.userMessageId)?.text === text,
        )?.status;
      assert.equal(statusOf("first"), "interrupted");
      assert.deepEqual(
        started.map((turn) => turn.message.text),
        ["/goal all tests pass"],
      );
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("stops the running turn when a goal clear ends the goal driving it", () =>
  Effect.gen(function* () {
    const { events, layer, startFirstTurn, send, read, steered } =
      yield* goalHarness("goal-clear-active");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = yield* startFirstTurn;
      const providerThread = (yield* orchestrator.getThreadProjection(threadId))
        .providerThreads[0]!;
      const goalSeen = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) =>
            event.type === "provider-thread.updated" && event.payload.goal?.status === "active",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped({ startImmediately: true }),
      );
      yield* Queue.offer(events, {
        type: "provider_thread.updated",
        driver,
        providerThread: {
          ...providerThread,
          goal: { objective: "all tests pass", status: "active" },
        },
      });
      yield* Fiber.join(goalSeen);
      yield* send("clear", "/goal clear", "auto");
      yield* worker.drain();

      assert.deepEqual(steered, []);
      assert.deepEqual(yield* read(threadId), {
        queue: [],
        started: ["first", "/goal clear"],
        stopRequested: true,
      });
    }).pipe(Effect.provide(layer));
  }),
);

it.effect("a side chat that opens with /goal gets its context on the next question", () =>
  Effect.gen(function* () {
    const { events, layer, started, startFirstTurn } = yield* goalHarness("side-chat-goal");
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const parentId = yield* startFirstTurn;
      const sideChatId = ThreadId.make("thread:side-chat-goal-child");
      yield* orchestrator.dispatch({
        type: "thread.fork",
        commandId: CommandId.make("side-chat"),
        sourceThreadId: parentId,
        targetThreadId: sideChatId,
        sourcePoint: { type: "latest_stable" },
        sideChat: true,
        createdBy: "user",
        creationSource: "web",
      });
      const ask = (id: string, text: string) =>
        orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make(id),
          threadId: sideChatId,
          messageId: MessageId.make(`message:${id}`),
          text,
          attachments: [],
          modelSelection: runSelection,
          dispatchMode: { type: "start_immediately" },
          createdBy: "user",
          creationSource: "web",
        });
      const sideChatEvent = (matches: (event: OrchestrationV2DomainEvent) => boolean) =>
        orchestrator.streamDomainEvents.pipe(
          Stream.filter((event) => event.threadId === sideChatId && matches(event)),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped({ startImmediately: true }),
        );
      const running = yield* sideChatEvent(
        (event) => event.type === "provider-turn.updated" && event.payload.status === "running",
      );
      const settled = yield* sideChatEvent(
        (event) => event.type === "run.updated" && event.payload.status === "completed",
      );
      yield* ask("goal", "/goal clear");
      yield* worker.drain();
      yield* Fiber.join(running);
      const turn = (yield* orchestrator.getThreadProjection(sideChatId)).providerTurns[0]!;
      yield* Queue.offer(events, {
        type: "provider_turn.updated",
        driver,
        providerTurn: { ...turn, status: "completed", completedAt: yield* DateTime.now },
      });
      yield* Queue.offer(events, {
        type: "turn.terminal",
        driver,
        providerThreadId: turn.providerThreadId,
        providerTurnId: turn.id,
        runOrdinal: started[1]!.runOrdinal,
        status: "completed",
        failure: null,
        threadDisposition: "reusable",
      });
      yield* Fiber.join(settled);
      yield* worker.drain();
      yield* ask("question", "What is the main thread doing?");
      yield* worker.drain();

      const [first, goal, question] = started.map((turn) => turn.message.text);
      assert.deepEqual([first, goal], ["first", "/goal clear"]);
      assert.include(question, "You are in a side chat beside the main thread.");
      assert.include(question, "- User: first");
      assert.isTrue(question?.endsWith("What is the main thread doing?"));
    }).pipe(Effect.provide(layer));
  }),
);
