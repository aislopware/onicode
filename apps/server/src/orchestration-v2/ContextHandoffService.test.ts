import { assert, it } from "@effect/vitest";
import {
  ContextTransferId,
  MessageId,
  ProjectId,
  ProviderInstanceId,
  ProviderThreadId,
  RunId,
  ScheduledTaskId,
  ThreadId,
  TurnItemId,
  type OrchestrationV2AppThread,
  type OrchestrationV2Run,
  type OrchestrationV2TurnItem,
} from "@onicode/contracts";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";

import * as ContextHandoffService from "./ContextHandoffService.ts";
import * as IdAllocator from "@onicode/provider-core/server/IdAllocator";
import { ProjectionStoreThreadNotFoundError } from "./ProjectionStore.ts";

const layerTest = ContextHandoffService.layer.pipe(Layer.provide(IdAllocator.layer));

function importedItem(
  input:
    | {
        readonly role: "user";
        readonly id: string;
        readonly text: string;
        readonly ordinal: number;
      }
    | {
        readonly role: "assistant";
        readonly id: string;
        readonly text: string;
        readonly ordinal: number;
      },
): OrchestrationV2TurnItem {
  const now = DateTime.makeUnsafe("2026-01-01T00:00:00.000Z");
  const base = {
    id: TurnItemId.make(`turn-item:${input.id}`),
    threadId: ThreadId.make("thread:legacy-context"),
    runId: null,
    nodeId: null,
    providerThreadId: null,
    providerTurnId: null,
    nativeItemRef: null,
    parentItemId: null,
    ordinal: input.ordinal,
    status: "completed" as const,
    title: null,
    startedAt: now,
    completedAt: now,
    updatedAt: now,
    messageId: MessageId.make(`message:${input.id}`),
    text: input.text,
  };
  return input.role === "user"
    ? {
        ...base,
        createdBy: "user",
        creationSource: "server",
        type: "user_message",
        inputIntent: "turn_start",
        attachments: [],
      }
    : {
        ...base,
        type: "assistant_message",
        streaming: false,
      };
}

it.layer(layerTest)("ContextHandoffService legacy import", (it) => {
  it.effect("prepares imported history for the first native v2 turn", () =>
    Effect.gen(function* () {
      const service = yield* ContextHandoffService.ContextHandoffServiceV2;
      const handoff = yield* service.prepareLegacyImport({
        threadId: ThreadId.make("thread:legacy-context"),
        targetRunId: RunId.make("run:first-v2"),
        toProviderThreadId: ProviderThreadId.make("provider-thread:first-v2"),
        toProviderInstanceId: ProviderInstanceId.make("codex"),
        items: [
          importedItem({ role: "user", id: "one", text: "What did we decide?", ordinal: 1 }),
          importedItem({
            role: "assistant",
            id: "two",
            text: "We decided to keep the migration lightweight.",
            ordinal: 2,
          }),
        ],
        createdAt: DateTime.makeUnsafe("2026-01-02T00:00:00.000Z"),
      });

      assert.equal(handoff.strategy, "manual_context");
      assert.deepStrictEqual(handoff.fromProviderThreadIds, []);
      assert.include(handoff.summaryText, "What did we decide?");
      assert.include(handoff.summaryText, "keep the migration lightweight");
    }),
  );

  it.effect("preserves role attribution when truncating imported history", () =>
    Effect.gen(function* () {
      const service = yield* ContextHandoffService.ContextHandoffServiceV2;
      const handoff = yield* service.prepareLegacyImport({
        threadId: ThreadId.make("thread:legacy-context"),
        targetRunId: RunId.make("run:first-v2"),
        toProviderThreadId: ProviderThreadId.make("provider-thread:first-v2"),
        toProviderInstanceId: ProviderInstanceId.make("codex"),
        items: [
          importedItem({
            role: "user",
            id: "long",
            text: `${"x".repeat(35_000)} retained final words`,
            ordinal: 1,
          }),
        ],
        createdAt: DateTime.makeUnsafe("2026-01-02T00:00:00.000Z"),
      });

      assert.isAtMost(handoff.summaryText.length, 32_000);
      assert.include(handoff.summaryText, "User:\n... retained final words");
      assert.notMatch(handoff.summaryText, /\n+x+ retained final words/);
    }),
  );

  it.effect("retains the newest oversized import even when it has no whitespace", () =>
    Effect.gen(function* () {
      const service = yield* ContextHandoffService.ContextHandoffServiceV2;
      const handoff = yield* service.prepareLegacyImport({
        threadId: ThreadId.make("thread:legacy-context"),
        targetRunId: RunId.make("run:first-v2"),
        toProviderThreadId: ProviderThreadId.make("provider-thread:first-v2"),
        toProviderInstanceId: ProviderInstanceId.make("codex"),
        items: [
          importedItem({
            role: "assistant",
            id: "older",
            text: "older message",
            ordinal: 1,
          }),
          importedItem({
            role: "user",
            id: "long-single-token",
            text: `${"🧪".repeat(20_000)}LATEST_SINGLE_TOKEN`,
            ordinal: 2,
          }),
        ],
        createdAt: DateTime.makeUnsafe("2026-01-02T00:00:00.000Z"),
      });

      assert.include(
        handoff.history?.omittedItemIds ?? [],
        TurnItemId.make("turn-item:long-single-token"),
      );
      assert.isAtMost(handoff.summaryText.length, 32_000);
      assert.include(handoff.summaryText, "User:\n... ");
      assert.include(handoff.summaryText, "LATEST_SINGLE_TOKEN");
      assert.notInclude(handoff.summaryText, "\ufffd");
    }),
  );
});

it.layer(layerTest)("ContextHandoffService token cap", (it) => {
  it.effect("retains a long reply within the configured token cap", () =>
    Effect.gen(function* () {
      const service = yield* ContextHandoffService.ContextHandoffServiceV2;
      const replyText = "Detailed plan.\n".repeat(1_400);
      const handoff = yield* service.prepareProviderHandoff({
        threadId: ThreadId.make("thread:legacy-context"),
        targetRunId: RunId.make("run:handoff"),
        transferId: null,
        fromProviderThreadIds: [ProviderThreadId.make("provider-thread:source")],
        toProviderThreadId: ProviderThreadId.make("provider-thread:target"),
        fromProviderInstanceId: ProviderInstanceId.make("codex"),
        toProviderInstanceId: ProviderInstanceId.make("claude"),
        coveredRunOrdinals: { from: 1, to: 1 },
        createdAt: DateTime.makeUnsafe("2026-01-02T00:00:00.000Z"),
        strategy: "full_thread_summary",
        items: [
          importedItem({ role: "user", id: "request", text: "Write a plan", ordinal: 1 }),
          importedItem({ role: "assistant", id: "long-reply", text: replyText, ordinal: 2 }),
        ],
      });
      assert.deepEqual(
        handoff.history?.messages.map((message) => message.text),
        ["Write a plan", replyText],
      );
      assert.equal(handoff.history?.omittedItems, 0);
    }),
  );
});

it.effect("caps the side chat context block without summarizing discarded parent history", () =>
  Effect.gen(function* () {
    const parentThreadId = ThreadId.make("thread:side-chat-parent");
    let summarizedItems = 0;
    const parentItems = Array.from({ length: 60 }, (_, index) => ({
      ...importedItem({
        role: index % 2 === 0 ? "user" : "assistant",
        id: `parent-${index}`,
        text: `item-${index} ${"x".repeat(228)}😀${"x".repeat(70)}`,
        ordinal: index,
      }),
      get text() {
        summarizedItems++;
        return `item-${index} ${"x".repeat(228)}😀${"x".repeat(70)}`;
      },
    }));
    const service = yield* ContextHandoffService.ContextHandoffServiceV2;
    const handoff = yield* service.prepareSideChatContext({
      parentThreadId,
      threadId: ThreadId.make("thread:side-chat"),
      targetRunId: RunId.make("run:side-chat"),
      transferId: ContextTransferId.make("transfer:side-chat"),
      toProviderThreadId: ProviderThreadId.make("provider-thread:side-chat"),
      fromProviderInstanceId: ProviderInstanceId.make("codex"),
      toProviderInstanceId: ProviderInstanceId.make("codex"),
      coveredRunOrdinals: { from: 1, to: 1 },
      parentItems,
      createdAt: DateTime.makeUnsafe(0),
    });
    const context = handoff.summaryText;
    assert.isBelow(summarizedItems, parentItems.length);

    assert.include(context, "- Earlier items truncated.");
    assert.notInclude(context, "item-0 ");
    assert.include(context, "- Assistant: item-59 ");
    assert.isAtMost(context.length, 8_000);
    assert.isTrue(context.isWellFormed());
    assert.isTrue(
      context.endsWith(
        `Use oni_thread_read on thread ${parentThreadId} to see the main thread's live state.`,
      ),
    );

    const framingOnly = ContextHandoffService.makeSideChatContext({
      parentThreadId,
      digest: [],
    });
    assert.isTrue(framingOnly.startsWith("You are in a side chat beside the main thread."));
    assert.notInclude(framingOnly, "Main thread activity");

    const multiFilePatch = ContextHandoffService.makeSideChatContext({
      parentThreadId,
      digest: [
        {
          id: TurnItemId.make("turn-item:patch"),
          threadId: parentThreadId,
          runId: null,
          nodeId: null,
          providerThreadId: null,
          providerTurnId: null,
          nativeItemRef: null,
          parentItemId: null,
          ordinal: 60,
          status: "completed",
          title: null,
          startedAt: null,
          completedAt: null,
          updatedAt: DateTime.makeUnsafe(0),
          type: "file_change",
          fileName: "src/a.ts",
          changes: [
            { operation: "update", path: "src/a.ts" },
            { operation: "add", path: "src/b.ts" },
            { operation: "update", path: "src/new.ts", oldPath: "src/old.ts" },
          ],
        },
      ],
    });
    assert.include(multiFilePatch, "- File change: src/a.ts, src/b.ts, src/new.ts");
  }).pipe(Effect.provide(layerTest)),
);

it("covers the run range of a very long delta", () => {
  const runs = [
    { id: RunId.make("run:long-delta-early"), ordinal: 2 },
    { id: RunId.make("run:long-delta-late"), ordinal: 5 },
  ];
  const items = Array.from({ length: 150_000 }, (_, index) => ({
    runId: runs[index % 2]!.id,
  }));

  assert.deepEqual(
    ContextHandoffService.visibleDeltaRunOrdinals({ runs }, [...items, { runId: null }]),
    { from: 2, to: 5 },
  );
});

const forkTestNow = DateTime.makeUnsafe("2026-01-01T00:00:00.000Z");

function forkTestThread(
  id: ThreadId,
  forkedFrom: OrchestrationV2AppThread["forkedFrom"] = null,
): OrchestrationV2AppThread {
  return {
    createdBy: "user",
    creationSource: "web",
    id,
    projectId: ProjectId.make("project:fork-history"),
    title: "Fork history",
    providerInstanceId: ProviderInstanceId.make("codex"),
    modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "gpt-5.4" },
    runtimeMode: "full-access",
    interactionMode: "default",
    branch: null,
    worktreePath: null,
    activeProviderThreadId: null,
    lineage: { parentThreadId: null, relationshipToParent: null, rootThreadId: id },
    forkedFrom,
    createdAt: forkTestNow,
    updatedAt: forkTestNow,
    archivedAt: null,
    settledOverride: null,
    settledAt: null,
    lastVisitedAt: null,
    snoozedUntil: null,
    snoozedAt: null,
    deletedAt: null,
  };
}

/** One completed run of `threadId` whose user message is `text`. */
function forkTestRun(
  threadId: ThreadId,
  ordinal: number,
  text: string,
  fresh?: "fresh" | "scheduled-fresh",
) {
  const freshContext = fresh === undefined ? undefined : true;
  const runId = RunId.make(`${threadId}:run-${ordinal}`);
  const messageId = MessageId.make(`${runId}:message`);
  const run: OrchestrationV2Run = {
    id: runId,
    threadId,
    ordinal,
    providerInstanceId: ProviderInstanceId.make("codex"),
    modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "gpt-5.4" },
    providerThreadId: null,
    userMessageId: messageId,
    rootNodeId: null,
    activeAttemptId: null,
    status: "completed",
    requestedAt: forkTestNow,
    startedAt: forkTestNow,
    completedAt: forkTestNow,
    checkpointId: null,
    contextHandoffId: null,
    ...(freshContext ? { freshContext } : {}),
  };
  const item: OrchestrationV2TurnItem = {
    id: TurnItemId.make(`${runId}:item`),
    threadId,
    runId,
    nodeId: null,
    providerThreadId: null,
    providerTurnId: null,
    nativeItemRef: null,
    parentItemId: null,
    ordinal,
    status: "completed",
    title: null,
    startedAt: forkTestNow,
    completedAt: forkTestNow,
    updatedAt: forkTestNow,
    createdBy: "user",
    creationSource: "web",
    type: "user_message",
    inputIntent: "turn_start",
    messageId,
    text,
    attachments: [],
    ...(fresh === "scheduled-fresh"
      ? { scheduledTaskId: ScheduledTaskId.make("task:hourly") }
      : {}),
  };
  return { run, item };
}

it.effect("a portable fork carries no context its source discarded for a fresh session", () =>
  Effect.gen(function* () {
    const parentId = ThreadId.make("thread:fork-history-parent");
    const freshForkId = ThreadId.make("thread:fork-history-fresh-fork");
    const supersededForkId = ThreadId.make("thread:fork-history-superseded-fork");
    const parentRun = forkTestRun(parentId, 1, "PARENT_WORK");
    const scheduledRun = forkTestRun(parentId, 2, "SCHEDULED_SIDE_RUN", "scheduled-fresh");
    const forkBefore = forkTestRun(freshForkId, 1, "BEFORE_CLEAR");
    const forkFresh = forkTestRun(freshForkId, 2, "FRESH_START", "fresh");
    const forkLater = forkTestRun(freshForkId, 3, "FORK_FOLLOW_UP");
    // Its fresh first run was cancelled before starting, so only the transfer records the discard.
    const supersededRun = forkTestRun(supersededForkId, 1, "AFTER_DISCARD");
    const forkedFromParent = { type: "run", threadId: parentId, runId: parentRun.run.id } as const;
    const projection = (
      thread: OrchestrationV2AppThread,
      runs: ReadonlyArray<ReturnType<typeof forkTestRun>>,
      contextTransfers: ReadonlyArray<unknown> = [],
    ) => ({
      thread,
      runs: runs.map(({ run }) => run),
      attempts: [],
      providerThreads: [],
      turnItems: runs.map(({ item }) => item),
      contextTransfers,
    });
    const parent = projection(forkTestThread(parentId), [parentRun, scheduledRun]);
    const freshFork = projection(forkTestThread(freshForkId, forkedFromParent), [
      forkBefore,
      forkFresh,
      forkLater,
    ]);
    const supersededFork = projection(
      forkTestThread(supersededForkId, forkedFromParent),
      [supersededRun],
      [{ type: "fork", targetThreadId: supersededForkId, status: "superseded" }],
    );
    const projections = new Map([
      [parentId, parent],
      [freshForkId, freshFork],
      [supersededForkId, supersededFork],
    ]);
    // Runs whose every row was read, not only their prompts.
    const fullyRead = new Set<RunId | null>();
    const store: Parameters<typeof ContextHandoffService.readForkHistoryItems>[0] = {
      getForkSourceRunIds: (threadId) =>
        Effect.succeed(
          threadId === freshForkId || threadId === supersededForkId
            ? [parentRun.run.id]
            : [
                parentRun.run.id,
                scheduledRun.run.id,
                forkBefore.run.id,
                forkFresh.run.id,
                forkLater.run.id,
                supersededRun.run.id,
              ],
        ),
      getThreadRecords: (threadId, _fields, filter) => {
        const found = projections.get(threadId);
        if (found === undefined)
          return Effect.fail(new ProjectionStoreThreadNotFoundError({ threadId }));
        const runIds = filter?.turnItemRunIds;
        if (filter?.turnItemTypes === undefined) {
          for (const runId of runIds ?? []) fullyRead.add(runId);
        }
        return Effect.succeed({
          ...found,
          turnItems: found.turnItems.filter(
            (item) => runIds === undefined || runIds.includes(item.runId),
          ),
        } as never);
      },
    };
    const targetFork = forkTestThread(ThreadId.make("thread:fork-history-target"));
    const texts = (items: ReadonlyArray<OrchestrationV2TurnItem>) =>
      items.flatMap((item) => (item.type === "user_message" ? [item.text] : []));

    assert.deepEqual(
      texts(
        yield* ContextHandoffService.readForkHistoryItems(
          store,
          freshFork,
          forkLater.run,
          targetFork,
        ),
      ),
      ["FRESH_START", "FORK_FOLLOW_UP"],
    );
    assert.isFalse(fullyRead.has(forkBefore.run.id));
    assert.deepEqual(
      texts(
        yield* ContextHandoffService.readForkHistoryItems(
          store,
          supersededFork,
          supersededRun.run,
          targetFork,
        ),
      ),
      ["AFTER_DISCARD"],
    );
    assert.deepEqual(
      texts(
        yield* ContextHandoffService.readForkHistoryItems(
          store,
          parent,
          scheduledRun.run,
          targetFork,
        ),
      ),
      ["SCHEDULED_SIDE_RUN"],
    );
  }),
);

it.effect("inherited history stops at a missing fork ancestor", () =>
  Effect.gen(function* () {
    const thread = forkTestThread(ThreadId.make("thread:fork-of-missing"), {
      type: "run",
      threadId: ThreadId.make("thread:missing-ancestor"),
      runId: RunId.make("run:missing-ancestor"),
    });
    const { items } = yield* ContextHandoffService.readInheritedHandoffItems(
      {
        getForkSourceRunIds: () => Effect.succeed([]),
        getThreadRecords: (missing) =>
          Effect.fail(new ProjectionStoreThreadNotFoundError({ threadId: missing })),
      },
      thread,
    );
    assert.deepEqual(items, []);
  }),
);
