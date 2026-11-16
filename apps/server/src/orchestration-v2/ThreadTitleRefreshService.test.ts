import { describe, expect, it } from "@effect/vitest";
import {
  CommandId,
  EventId,
  MessageId,
  type OrchestrationV2AppThread,
  type OrchestrationV2DomainEvent,
  type OrchestrationV2GeneratedTitle,
  type OrchestrationV2Run,
  type OrchestrationV2ThreadShell,
  ProjectId,
  ProviderInstanceId,
  RunId,
  ThreadId,
} from "@onicode/contracts";
import * as DateTime from "effect/DateTime";
import * as Deferred from "effect/Deferred";
import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Stream from "effect/Stream";
import * as TestClock from "effect/testing/TestClock";

import * as Orchestrator from "./Orchestrator.ts";
import * as ProjectionStore from "./ProjectionStore.ts";
import * as ThreadTitleRefreshService from "./ThreadTitleRefreshService.ts";
import {
  awaitsTitleSubject,
  canRefreshGeneratedTitle,
  generatedTitleIsStale,
} from "./threadTitleRefresh.ts";

const NOW_MS = Date.parse("2026-11-16T12:00:00.000Z");
const MINUTE_MS = 60_000;
const at = (offset = 0) => DateTime.makeUnsafe(NOW_MS + offset);
const threadId = ThreadId.make("title-thread");
const runId = RunId.make("run-1");
const instanceId = ProviderInstanceId.make("claudeAgent");
const modelSelection = { instanceId, model: "claude-opus-5-5" };

const known: OrchestrationV2GeneratedTitle = {
  needsRefinement: false,
  refinements: 0,
  coveredAt: null,
};
const unknown: OrchestrationV2GeneratedTitle = { ...known, needsRefinement: true };

function threadWith(overrides: Partial<OrchestrationV2AppThread> = {}): OrchestrationV2AppThread {
  return {
    id: threadId,
    projectId: ProjectId.make("project"),
    title: "Slack Thread Check",
    createdBy: "user",
    creationSource: "web",
    providerInstanceId: instanceId,
    modelSelection,
    runtimeMode: "full-access",
    interactionMode: "default",
    branch: null,
    worktreePath: null,
    activeProviderThreadId: null,
    lineage: { rootThreadId: threadId, parentThreadId: null, relationshipToParent: null },
    forkedFrom: null,
    createdAt: at(-10 * MINUTE_MS),
    updatedAt: at(),
    archivedAt: null,
    settledOverride: null,
    settledAt: null,
    lastVisitedAt: at(),
    deletedAt: null,
    generatedTitle: known,
    ...overrides,
  };
}

function shellWith(
  overrides: Partial<OrchestrationV2ThreadShell> = {},
): OrchestrationV2ThreadShell {
  return {
    ...threadWith(),
    latestRunId: runId,
    latestRunCompletedAt: at(-MINUTE_MS),
    activeRunId: null,
    status: "completed",
    pendingRuntimeRequest: null,
    latestVisibleMessage: null,
    latestUserMessageAt: at(-5 * MINUTE_MS),
    latestUserAuthoredMessageAt: at(-5 * MINUTE_MS),
    hasActionableProposedPlan: false,
    pendingBackgroundTasks: [],
    providerInstanceHistory: [],
    itemCount: 2,
    visibleItemCount: 2,
    ...overrides,
  };
}

describe("generated title staleness", () => {
  it.each([
    { name: "a rename by hand", thread: threadWith({ generatedTitle: null }), can: false },
    { name: "a thread titled before refreshes", thread: threadWith({}), drop: true, can: false },
    {
      name: "a generation in flight",
      thread: threadWith({
        titleRegeneration: { requestId: CommandId.make("request"), startedAt: at() },
      }),
      can: false,
    },
    { name: "an archived thread", thread: threadWith({ archivedAt: at() }), can: false },
    { name: "a generated title at rest", thread: threadWith(), can: true },
  ])("refreshes only a title the generator owns: $name", ({ thread, drop, can }) => {
    const { generatedTitle: _generatedTitle, ...withoutState } = thread;
    expect(canRefreshGeneratedTitle(drop ? withoutState : thread)).toBe(can);
  });

  it.each([
    {
      name: "the first run has not finished",
      generated: known,
      shell: { latestRunCompletedAt: null },
      stale: false,
    },
    { name: "only the first message was read", generated: known, shell: {}, stale: true },
    {
      name: "the user wrote after the title was generated",
      generated: { ...known, coveredAt: at(-6 * MINUTE_MS) },
      shell: {},
      stale: true,
    },
    {
      name: "only an agent wake ran since the title was generated",
      generated: { ...known, coveredAt: at(-3 * MINUTE_MS) },
      shell: {},
      stale: false,
    },
    {
      name: "a run finished while the subject is unknown",
      generated: { ...unknown, coveredAt: at(-3 * MINUTE_MS) },
      shell: {},
      stale: true,
    },
    {
      name: "the subject stayed unknown through every allowed refresh",
      generated: { ...unknown, refinements: 3, coveredAt: at(-3 * MINUTE_MS) },
      shell: {},
      stale: false,
    },
  ])("reads the thread again when $name: $stale", ({ generated, shell, stale }) => {
    const thread = threadWith({ generatedTitle: generated });
    if (!canRefreshGeneratedTitle(thread)) throw new Error("fixture must own its title");
    expect(generatedTitleIsStale(thread, shellWith(shell))).toBe(stale);
    expect(awaitsTitleSubject(thread)).toBe(generated.needsRefinement && generated.refinements < 3);
  });
});

describe("ThreadTitleRefreshService", () => {
  const run: OrchestrationV2Run = {
    id: runId,
    threadId,
    ordinal: 1,
    providerInstanceId: instanceId,
    modelSelection,
    providerThreadId: null,
    userMessageId: MessageId.make("message-1"),
    rootNodeId: null,
    activeAttemptId: null,
    status: "completed",
    requestedAt: at(-5 * MINUTE_MS),
    startedAt: at(-5 * MINUTE_MS),
    completedAt: at(-MINUTE_MS),
    checkpointId: null,
    contextHandoffId: null,
  };
  const runEnded: OrchestrationV2DomainEvent = {
    id: EventId.make("run-ended"),
    type: "run.updated",
    threadId,
    runId,
    occurredAt: at(),
    payload: run,
  };
  const settled: OrchestrationV2DomainEvent = {
    id: EventId.make("settled"),
    type: "thread.settled",
    threadId,
    occurredAt: at(),
    payload: threadWith({ settledOverride: "settled", settledAt: at() }),
  };

  const titled = (generatedTitle: OrchestrationV2GeneratedTitle): OrchestrationV2DomainEvent => ({
    id: EventId.make("titled"),
    type: "thread.metadata-updated",
    threadId,
    occurredAt: at(),
    payload: threadWith({ generatedTitle }),
  });

  it.effect.each([
    {
      name: "an unknown subject is retried as soon as a run ends",
      event: runEnded,
      generated: unknown,
      atOnce: 1,
      afterQuiet: 1,
    },
    {
      name: "a first title that finishes after its run is retried at once",
      event: titled(unknown),
      generated: unknown,
      atOnce: 1,
      afterQuiet: 1,
    },
    {
      name: "a metadata change on a title that names its subject starts nothing",
      event: titled(known),
      generated: known,
      atOnce: 0,
      afterQuiet: 0,
    },
    {
      name: "a known subject waits for the thread to go quiet",
      event: runEnded,
      generated: known,
      atOnce: 0,
      afterQuiet: 1,
    },
    {
      name: "a run that started during the wait holds the refresh",
      event: runEnded,
      generated: known,
      activeAfterWait: true,
      atOnce: 0,
      afterQuiet: 0,
    },
    {
      name: "a settled thread is refreshed without the wait",
      event: settled,
      generated: known,
      atOnce: 1,
      afterQuiet: 1,
    },
    {
      name: "a title renamed by hand is left alone",
      event: runEnded,
      generated: null,
      atOnce: 0,
      afterQuiet: 0,
    },
    {
      name: "a title that already covers the thread is left alone",
      event: settled,
      generated: { ...known, coveredAt: at() },
      atOnce: 0,
      afterQuiet: 0,
    },
  ])("$name", (scenario) =>
    Effect.gen(function* () {
      yield* TestClock.setTime(NOW_MS);
      const dispatched: Array<string> = [];
      let waited = false;
      let generated = scenario.generated;
      const streamDone = yield* Deferred.make<void>();
      const dependencies = Layer.mergeAll(
        Layer.mock(Orchestrator.OrchestratorV2)({
          streamDomainEvents: Stream.make(scenario.event).pipe(
            Stream.ensuring(Deferred.succeed(streamDone, undefined)),
          ),
          dispatch: (command) =>
            Effect.sync(() => {
              dispatched.push(command.type);
              // The orchestrator marks the thread covered as the refresh starts.
              if (generated !== null) generated = { ...generated, coveredAt: at(MINUTE_MS) };
              return { sequence: 1, storedEvents: [] };
            }),
        }),
        Layer.mock(ProjectionStore.ProjectionStoreV2)({
          getThread: () => Effect.sync(() => threadWith({ generatedTitle: generated })),
          getThreadShell: () =>
            Effect.succeed(
              shellWith(waited && scenario.activeAfterWait ? { activeRunId: runId } : {}),
            ),
        }),
      );
      yield* Effect.gen(function* () {
        const service = yield* ThreadTitleRefreshService.make;
        yield* service.start();
        yield* Deferred.await(streamDone);
        yield* TestClock.adjust(Duration.millis(1));
        expect(dispatched).toHaveLength(scenario.atOnce);
        waited = true;
        yield* TestClock.adjust(ThreadTitleRefreshService.TITLE_REFRESH_QUIET);
        expect(dispatched).toHaveLength(scenario.afterQuiet);
        expect(new Set(dispatched)).toEqual(
          new Set(scenario.afterQuiet > 0 ? ["thread.title.refresh"] : []),
        );
      }).pipe(Effect.provide(dependencies));
    }),
  );
});
