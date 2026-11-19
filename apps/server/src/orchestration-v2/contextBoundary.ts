import type {
  MessageId,
  OrchestrationV2ContextTransfer,
  OrchestrationV2Run,
  OrchestrationV2ThreadProjection,
  OrchestrationV2TurnItem,
  RunId,
  ThreadId,
} from "@onicode/contracts";
import * as Effect from "effect/Effect";

import { cancelledBeforeStart, neverStarted } from "./RestartBackgroundNote.ts";

/** A fresh run that never reached a provider discarded nothing, also once a rollback rewound it. */
function discardedContext(run: OrchestrationV2Run): boolean {
  return (
    run.freshContext === true &&
    run.status !== "queued" &&
    !cancelledBeforeStart(run) &&
    !(run.status === "rolled_back" && neverStarted(run))
  );
}

// Collected in one pass: an hourly fresh-session schedule leaves thousands of fresh runs to check.
// Turn items, unlike messages, are not narrowed to the runs since the latest compaction.
function scheduledMessageIds(
  turnItems: OrchestrationV2ThreadProjection["turnItems"],
): ReadonlySet<MessageId> {
  return new Set(
    turnItems.flatMap((item) =>
      item.type === "user_message" && item.scheduledTaskId !== undefined ? [item.messageId] : [],
    ),
  );
}

/** Ordinals of fresh-session schedule runs: side conversations the thread's own one continues past. */
export function scheduledFreshRunOrdinals(
  projection: Pick<OrchestrationV2ThreadProjection, "runs" | "turnItems">,
): ReadonlySet<number> {
  const freshRuns = projection.runs.filter((run) => run.freshContext === true);
  if (freshRuns.length === 0) return new Set();
  const messageIds = scheduledMessageIds(projection.turnItems);
  return new Set(
    freshRuns.flatMap((run) => (messageIds.has(run.userMessageId) ? [run.ordinal] : [])),
  );
}

/** Ordinal of the latest started fresh-context run. Runs before it never reach a provider again. */
export function contextBoundaryOrdinal(
  projection: Pick<OrchestrationV2ThreadProjection, "runs" | "turnItems">,
): number {
  let scheduled: ReadonlySet<MessageId> | undefined;
  let boundary = 0;
  for (const run of projection.runs) {
    if (!discardedContext(run) || run.ordinal <= boundary) continue;
    scheduled ??= scheduledMessageIds(projection.turnItems);
    if (!scheduled.has(run.userMessageId)) {
      boundary = run.ordinal;
    }
  }
  return boundary;
}

/**
 * Where a fork at `forkRun` starts reading its source, given the source's runs through it. A
 * started fresh fork run, even a scheduled side conversation, saw nothing before itself.
 */
export function forkContextBoundaryOrdinal(
  projection: Pick<OrchestrationV2ThreadProjection, "runs" | "turnItems">,
  forkRun: OrchestrationV2Run | undefined,
): number {
  return forkRun !== undefined && discardedContext(forkRun)
    ? forkRun.ordinal
    : contextBoundaryOrdinal(projection);
}

/** The user messages of fresh runs: all the boundary and schedule checks read. */
export const readFreshRunUserMessages = <E, R>(
  store: {
    readonly getThreadRecords: (
      threadId: ThreadId,
      fields: ReadonlyArray<"turnItems">,
      filter: {
        readonly turnItemRunIds: ReadonlyArray<RunId>;
        readonly turnItemTypes: ReadonlyArray<OrchestrationV2TurnItem["type"]>;
      },
    ) => Effect.Effect<Pick<OrchestrationV2ThreadProjection, "turnItems">, E, R>;
  },
  threadId: ThreadId,
  runs: ReadonlyArray<OrchestrationV2Run>,
): Effect.Effect<ReadonlyArray<OrchestrationV2TurnItem>, E, R> => {
  const freshRunIds = runs.flatMap((run) => (run.freshContext === true ? [run.id] : []));
  return freshRunIds.length === 0
    ? Effect.succeed([])
    : store
        .getThreadRecords(threadId, ["turnItems"], {
          turnItemRunIds: freshRunIds,
          turnItemTypes: ["user_message"],
        })
        .pipe(Effect.map(({ turnItems }) => turnItems));
};

/** A fresh send before a fork's first run supersedes its transfer, discarding the parent's context. */
export const forkContextSuperseded = (
  threadId: ThreadId,
  contextTransfers: ReadonlyArray<OrchestrationV2ContextTransfer>,
) =>
  contextTransfers.some(
    (transfer) =>
      transfer.type === "fork" &&
      transfer.targetThreadId === threadId &&
      transfer.status === "superseded",
  );
