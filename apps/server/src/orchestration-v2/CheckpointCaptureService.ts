import {
  CheckpointScopeId,
  CommandId,
  type OrchestrationV2Checkpoint,
  type OrchestrationV2CheckpointScope,
  type OrchestrationV2ExecutionNode,
  type OrchestrationV2ProviderThread,
  type OrchestrationV2Run,
  type OrchestrationV2TurnItem,
  RunId,
  ThreadId,
} from "@onicode/contracts";
import * as Context from "effect/Context";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

import * as CheckpointService from "./CheckpointService.ts";
import * as EventSink from "./EventSink.ts";
import * as IdAllocator from "@onicode/provider-core/server/IdAllocator";
import * as ProjectionStore from "./ProjectionStore.ts";
import { makeInterruptResultTurnItem } from "./runInterruptResult.ts";
import * as ThreadCommandExecutor from "./ThreadCommandExecutor.ts";

// Git processes are already capped server-wide; this keeps one thread from taking every slot.
const REPOSITORY_CAPTURE_CONCURRENCY = 6;

export class CheckpointCaptureExecutionError extends Schema.TaggedError<CheckpointCaptureExecutionError>()(
  "CheckpointCaptureExecutionError",
  {
    threadId: ThreadId,
    runId: RunId,
    scopeId: CheckpointScopeId,
    cause: Schema.optional(Schema.Defect()),
  },
) {}

const isCheckpointCaptureExecutionError = Schema.is(CheckpointCaptureExecutionError);

const isStopped = (run: OrchestrationV2Run) =>
  run.status === "interrupted" || run.status === "cancelled";

// A checkpoint on a settled run proves this at-least-once effect already committed. Rollback
// uses the same lane and can precede a capture retry; preserve the restored workspace and run.
const isCaptured = (run: OrchestrationV2Run) =>
  run.status === "rolled_back" ||
  (run.checkpointId !== null && (run.status === "completed" || isStopped(run)));

// Runs settle before capture; `waiting` is a run persisted before captures ran after settling, or
// one a command that read it as held wrote back.
const awaitsCapture = (run: OrchestrationV2Run) =>
  run.status === "waiting" || run.status === "completed" || isStopped(run);

export interface CheckpointCaptureServiceV2Shape {
  readonly execute: (input: {
    readonly threadId: ThreadId;
    readonly runId: RunId;
    readonly scopeId: CheckpointScopeId;
  }) => Effect.Effect<void, CheckpointCaptureExecutionError>;
}

export class CheckpointCaptureServiceV2 extends Context.Service<
  CheckpointCaptureServiceV2,
  CheckpointCaptureServiceV2Shape
>()("onicode/orchestration-v2/CheckpointCaptureService/CheckpointCaptureServiceV2") {}

export const layer: Layer.Layer<
  CheckpointCaptureServiceV2,
  never,
  | CheckpointService.CheckpointServiceV2
  | EventSink.EventSinkV2
  | IdAllocator.IdAllocatorV2
  | ProjectionStore.ProjectionStoreV2
  | ThreadCommandExecutor.ThreadCommandExecutor
> = Layer.effect(
  CheckpointCaptureServiceV2,
  Effect.gen(function* () {
    const checkpoints = yield* CheckpointService.CheckpointServiceV2;
    const eventSink = yield* EventSink.EventSinkV2;
    const ids = yield* IdAllocator.IdAllocatorV2;
    const projections = yield* ProjectionStore.ProjectionStoreV2;
    const threadCommands = yield* ThreadCommandExecutor.ThreadCommandExecutor;

    const execute = Effect.fn("orchestrationV2.checkpointCapture.execute")(function* (input: {
      readonly threadId: ThreadId;
      readonly runId: RunId;
      readonly scopeId: CheckpointScopeId;
    }) {
      const context = yield* projections.getCheckpointCaptureContext(input.threadId, input, {
        preservedCheckpoints: "baselines",
      });
      const { run, rootNode, scope, providerThread, preservedCheckpointOrdinals, sideChat } =
        context;

      // A steering restart superseded the attempt this capture was for; the new attempt enqueues its own.
      if (
        run !== undefined &&
        (isCaptured(run) || run.status === "starting" || run.status === "running")
      ) {
        return;
      }

      if (
        run === undefined ||
        !awaitsCapture(run) ||
        rootNode === undefined ||
        scope === undefined ||
        rootNode.checkpointScopeId !== scope.id ||
        providerThread === undefined
      ) {
        return yield* new CheckpointCaptureExecutionError({
          threadId: input.threadId,
          runId: input.runId,
          scopeId: input.scopeId,
          cause: "The persisted checkpoint capture target is incomplete or was discarded.",
        });
      }

      const capturedAt = yield* DateTime.now;
      const baselineOrdinalWithinScope = Math.max(0, run.ordinal - 1);
      const isPreserved = (ordinalWithinScope: number) =>
        preservedCheckpointOrdinals.includes(ordinalWithinScope);
      // The baselines this capture materializes are attributed to the run materializing them. The
      // thread's scope may already be a later run's: one queued meanwhile, or sent after Stop.
      const materializeBaseline = (ordinalWithinScope: number) =>
        checkpoints.materializeBaselineCheckpoint({
          scope,
          ordinalWithinScope,
          nodeId: rootNode.id,
          capturedAt: run.requestedAt,
          forceMissing: sideChat,
        });
      // The tracked Git folders the thread reaches now. One added since the turn began has no
      // baseline, so its first checkpoint lists no files; one removed is simply not captured.
      const repositoryScopes =
        sideChat || context.thread === undefined
          ? []
          : yield* checkpoints.repositoryScopes({ rootScope: scope, ...context.thread });
      const nested =
        repositoryScopes.length === 0
          ? undefined
          : yield* checkpoints.nestedScopePaths([scope, ...repositoryScopes]);
      const excluded = (target: OrchestrationV2CheckpointScope) => {
        const excludePaths = nested?.get(target.id);
        return excludePaths === undefined ? {} : { excludePaths };
      };
      // Each scope is checked on its own, so a retried capture fills only what is missing.
      const captureRoot = Effect.gen(function* () {
        // Materializing a baseline waits on the folder's lock before the capture does.
        const capturesBeforeLock = yield* checkpoints.capturesSoFar(scope.cwd);
        const threadStartCheckpoint =
          baselineOrdinalWithinScope === 0 || isPreserved(0) ? null : yield* materializeBaseline(0);
        const baselineCheckpoint = isPreserved(baselineOrdinalWithinScope)
          ? null
          : yield* materializeBaseline(baselineOrdinalWithinScope);
        const checkpoint = yield* checkpoints.capture({
          scope,
          runId: run.id,
          nodeId: rootNode.id,
          ordinalWithinScope: run.ordinal,
          appRunOrdinal: run.ordinal,
          capturedAt,
          forceMissing: sideChat,
          capturesBeforeLock,
          ...excluded(scope),
        });
        return { threadStartCheckpoint, baselineCheckpoint, checkpoint };
      });
      const recordedScopes = new Map(
        context.repositoryScopes.map((recorded) => [recorded.id, recorded]),
      );
      const captureRepository = Effect.fnUntraced(function* (
        prepared: OrchestrationV2CheckpointScope,
      ) {
        const recorded = recordedScopes.get(prepared.id);
        // A recorded scope keeps the run that first used it; only a renamed folder rewrites it.
        const changed =
          recorded === undefined ||
          recorded.cwd !== prepared.cwd ||
          recorded.label !== prepared.label;
        const target =
          recorded === undefined
            ? prepared
            : changed
              ? {
                  ...prepared,
                  runId: recorded.runId,
                  nodeId: recorded.nodeId,
                  createdAt: recorded.createdAt,
                }
              : recorded;
        const captured = yield* checkpoints.captureRepository({
          scope: target,
          runId: run.id,
          nodeId: rootNode.id,
          ordinalWithinScope: run.ordinal,
          capturedAt,
          baselineCapturedAt: run.requestedAt,
          baselineRecorded: context.repositoryScopesWithBaseline.includes(target.id),
          ...excluded(target),
        });
        // Only a repository with something to show is recorded, so the rest cost no events,
        // rows or bytes on the wire.
        return Option.match(captured, {
          onNone: () => [],
          onSome: (capture) => [{ scope: changed ? target : null, ...capture }],
        });
      });
      const [{ threadStartCheckpoint, baselineCheckpoint, checkpoint }, repositoryCaptures] =
        yield* Effect.all(
          [
            captureRoot,
            Effect.forEach(repositoryScopes, captureRepository, {
              concurrency: REPOSITORY_CAPTURE_CONCURRENCY,
            }),
          ],
          { concurrency: 2 },
        );

      // A run ends under the dispatch lock, so a dispatch that read it active acts before it ends.
      yield* threadCommands.withLock(
        input.threadId,
        Effect.gen(function* () {
          const current = yield* projections.getCheckpointCaptureContext(input.threadId, input, {
            preservedCheckpoints: "baselines",
          });
          const currentRun = current.run;
          const currentRootNode = current.rootNode;
          // A steering restart's new attempt captures its own work once it ends; until then this
          // capture's snapshots are dropped below.
          if (
            currentRun !== undefined &&
            currentRun.activeAttemptId !== run.activeAttemptId &&
            awaitsCapture(currentRun)
          ) {
            return;
          }
          if (
            currentRun === undefined ||
            currentRootNode === undefined ||
            isCaptured(currentRun) ||
            !awaitsCapture(currentRun)
          ) {
            // Left behind, a snapshot at this ordinal would pass for a later turn's baseline.
            // Dropped under the lock: a capture that could write them again starts after it.
            if (
              (currentRun?.checkpointId == null || currentRun.status === "rolled_back") &&
              repositoryScopes.length > 0 &&
              context.thread !== undefined
            ) {
              yield* checkpoints.discardRepositorySnapshots({
                rootScope: scope,
                thread: context.thread,
                ordinals: [run.ordinal],
                scopes: repositoryScopes,
              });
            }
            return;
          }
          // A Stop accepted once the provider's turn had ended stored only its request. That Stop
          // still wins, and gets its result here.
          const stopSignals = isStopped(currentRun)
            ? []
            : (yield* projections.getThreadRecords(input.threadId, ["turnItems"], {
                turnItemTypes: ["run_interrupt_request", "run_interrupt_result"],
                turnItemRunIds: [currentRun.id],
              })).turnItems;
          // A Stop that reached a completed turn with no provider turn to interrupt came after the
          // turn ended: the turn stays completed, as Stop reported no active run.
          const stopRequest = stopSignals.some((item) => item.type === "run_interrupt_result")
            ? undefined
            : stopSignals.find(
                (item) =>
                  item.type === "run_interrupt_request" &&
                  (item.providerTurnId !== null || currentRun.status === "waiting"),
              );
          // Null keeps the status the run settled with; the checkpoint is all that is missing.
          const settledStatus =
            stopRequest !== undefined
              ? ("interrupted" as const)
              : currentRun.status === "waiting"
                ? ("completed" as const)
                : null;
          const completedAt = currentRun.completedAt ?? capturedAt;
          // Omit delegatedCompletion to preserve cohort writes made after capture loaded the run.
          const { delegatedCompletion: _delegatedCompletion, ...runWithoutDelegatedCompletion } =
            currentRun;
          const commandId = CommandId.make(`command:effect:checkpoint.capture:${currentRun.id}`);
          const checkpointEvent = (payload: OrchestrationV2Checkpoint) =>
            Effect.map(ids.allocate.event({ threadId: input.threadId, commandId }), (id) => ({
              id,
              type: "checkpoint.captured" as const,
              threadId: input.threadId,
              ...(payload.runId === null ? {} : { runId: payload.runId }),
              nodeId: payload.nodeId,
              driver: providerThread.driver,
              providerInstanceId: currentRun.providerInstanceId,
              occurredAt: capturedAt,
              payload,
            }));
          const repositoryEvents = yield* Effect.forEach(repositoryCaptures.flat(), (capture) =>
            Effect.gen(function* () {
              return [
                ...(capture.scope === null
                  ? []
                  : [
                      {
                        id: yield* ids.allocate.event({ threadId: input.threadId, commandId }),
                        type: "checkpoint-scope.created" as const,
                        threadId: input.threadId,
                        runId: currentRun.id,
                        nodeId: currentRootNode.id,
                        providerInstanceId: currentRun.providerInstanceId,
                        occurredAt: capturedAt,
                        payload: capture.scope,
                      },
                    ]),
                ...(capture.baseline === null ? [] : [yield* checkpointEvent(capture.baseline)]),
                yield* checkpointEvent(capture.checkpoint),
              ];
            }),
          );
          yield* eventSink.commitCommand({
            commandId,
            threadId: input.threadId,
            commandType: "checkpoint.capture",
            acceptedAt: capturedAt,
            effects: [],
            events: [
              // Root records come last: a client that takes the latest ordinal-0 baseline gets the thread's.
              ...repositoryEvents.flat(),
              ...(threadStartCheckpoint === null
                ? []
                : [
                    {
                      id: yield* ids.allocate.event({ threadId: input.threadId, commandId }),
                      type: "checkpoint.captured" as const,
                      threadId: input.threadId,
                      nodeId: threadStartCheckpoint.nodeId,
                      driver: providerThread.driver,
                      providerInstanceId: currentRun.providerInstanceId,
                      occurredAt: capturedAt,
                      payload: threadStartCheckpoint,
                    },
                  ]),
              ...(baselineCheckpoint === null
                ? []
                : [
                    {
                      id: yield* ids.allocate.event({ threadId: input.threadId, commandId }),
                      type: "checkpoint.captured" as const,
                      threadId: input.threadId,
                      nodeId: baselineCheckpoint.nodeId,
                      driver: providerThread.driver,
                      providerInstanceId: currentRun.providerInstanceId,
                      occurredAt: capturedAt,
                      payload: baselineCheckpoint,
                    },
                  ]),
              {
                id: yield* ids.allocate.event({ threadId: input.threadId, commandId }),
                type: "checkpoint.captured",
                threadId: input.threadId,
                runId: currentRun.id,
                nodeId: currentRootNode.id,
                driver: providerThread.driver,
                providerInstanceId: currentRun.providerInstanceId,
                occurredAt: capturedAt,
                payload: checkpoint,
              },
              {
                id: yield* ids.allocate.event({ threadId: input.threadId, commandId }),
                type: "turn-item.updated",
                threadId: input.threadId,
                runId: currentRun.id,
                nodeId: currentRootNode.id,
                driver: providerThread.driver,
                providerInstanceId: currentRun.providerInstanceId,
                occurredAt: capturedAt,
                payload: makeCheckpointTurnItem({
                  idAllocator: ids,
                  run: currentRun,
                  rootNode: currentRootNode,
                  providerThread,
                  checkpoint,
                  completedAt: capturedAt,
                }),
              },
              ...(stopRequest?.type !== "run_interrupt_request"
                ? []
                : [
                    {
                      id: yield* ids.allocate.event({ threadId: input.threadId, commandId }),
                      type: "turn-item.updated" as const,
                      threadId: input.threadId,
                      runId: currentRun.id,
                      nodeId: currentRootNode.id,
                      providerInstanceId: currentRun.providerInstanceId,
                      occurredAt: capturedAt,
                      payload: makeInterruptResultTurnItem({
                        idAllocator: ids,
                        run: currentRun,
                        rootNode: currentRootNode,
                        providerThread,
                        reason: stopRequest.message,
                        completedAt: capturedAt,
                      }),
                    },
                  ]),
              {
                id: yield* ids.allocate.event({ threadId: input.threadId, commandId }),
                type: "run.updated",
                threadId: input.threadId,
                runId: currentRun.id,
                nodeId: currentRootNode.id,
                providerInstanceId: currentRun.providerInstanceId,
                occurredAt: capturedAt,
                payload:
                  settledStatus === null
                    ? { ...runWithoutDelegatedCompletion, checkpointId: checkpoint.id }
                    : {
                        ...runWithoutDelegatedCompletion,
                        status: settledStatus,
                        completedAt,
                        checkpointId: checkpoint.id,
                      },
              },
              ...(settledStatus === null
                ? []
                : [
                    {
                      id: yield* ids.allocate.event({ threadId: input.threadId, commandId }),
                      type: "node.updated" as const,
                      threadId: input.threadId,
                      runId: currentRun.id,
                      nodeId: currentRootNode.id,
                      providerInstanceId: currentRun.providerInstanceId,
                      occurredAt: capturedAt,
                      payload: {
                        ...currentRootNode,
                        status: settledStatus,
                        completedAt,
                        checkpointScopeId: scope.id,
                      },
                    },
                  ]),
            ],
          });
        }),
      );
    });

    return CheckpointCaptureServiceV2.of({
      execute: Effect.fn("CheckpointCaptureServiceV2.execute")(function (input) {
        return execute(input).pipe(
          Effect.mapError((cause) =>
            isCheckpointCaptureExecutionError(cause)
              ? cause
              : new CheckpointCaptureExecutionError({ ...input, cause }),
          ),
        );
      }),
    });
  }),
);

function makeCheckpointTurnItem(input: {
  readonly idAllocator: IdAllocator.IdAllocatorV2["Service"];
  readonly run: OrchestrationV2Run;
  readonly rootNode: OrchestrationV2ExecutionNode;
  readonly providerThread: OrchestrationV2ProviderThread;
  readonly checkpoint: OrchestrationV2Checkpoint;
  readonly completedAt: DateTime.Utc;
}): OrchestrationV2TurnItem {
  return {
    id: input.idAllocator.derive.turnItemFromProviderItem({
      driver: input.providerThread.driver,
      nativeItemId: `checkpoint:${input.checkpoint.id}`,
    }),
    threadId: input.run.threadId,
    runId: input.run.id,
    nodeId: input.rootNode.id,
    providerThreadId: input.providerThread.id,
    providerTurnId: input.rootNode.providerTurnId,
    nativeItemRef: null,
    parentItemId: null,
    ordinal: input.run.ordinal * 100 + 99,
    status: "completed",
    title: null,
    startedAt: input.completedAt,
    completedAt: input.completedAt,
    updatedAt: input.completedAt,
    type: "checkpoint",
    checkpointId: input.checkpoint.id,
    scopeId: input.checkpoint.scopeId,
    files: input.checkpoint.files,
  };
}
