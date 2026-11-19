import type {
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
export function startedFreshRun(run: OrchestrationV2Run): boolean {
  return (
    run.freshContext === true &&
    run.status !== "queued" &&
    !cancelledBeforeStart(run) &&
    !(run.status === "rolled_back" && neverStarted(run))
  );
}

// Collected in one pass: an hourly fresh-session schedule leaves thousands of fresh runs to check.
// A compacted thread's command read loads the prompts of every fresh-context run.
// A run's first message decides: a steering restart points the run at the steer instead.
function scheduledRunIds(
  turnItems: OrchestrationV2ThreadProjection["turnItems"],
): ReadonlySet<RunId> {
  const first = new Map<RunId, OrchestrationV2TurnItem>();
  for (const item of turnItems) {
    if (item.type !== "user_message" || item.runId === null) continue;
    const held = first.get(item.runId);
    if (held === undefined || item.ordinal < held.ordinal) first.set(item.runId, item);
  }
  return new Set(
    Array.from(first).flatMap(([runId, item]) =>
      item.type === "user_message" && item.scheduledTaskId !== undefined ? [runId] : [],
    ),
  );
}

/** A fresh-session schedule run starts a side conversation; the thread's own one continues past it. */
export function isScheduledFreshRun(
  run: Pick<OrchestrationV2Run, "freshContext" | "id">,
  turnItems: OrchestrationV2ThreadProjection["turnItems"],
): boolean {
  return run.freshContext === true && scheduledRunIds(turnItems).has(run.id);
}

/** Ordinals of fresh-session schedule runs: side conversations the thread's own one continues past. */
export function scheduledFreshRunOrdinals(
  projection: Pick<OrchestrationV2ThreadProjection, "runs" | "turnItems">,
): ReadonlySet<number> {
  const freshRuns = projection.runs.filter((run) => run.freshContext === true);
  if (freshRuns.length === 0) return new Set();
  const runIds = scheduledRunIds(projection.turnItems);
  return new Set(freshRuns.flatMap((run) => (runIds.has(run.id) ? [run.ordinal] : [])));
}

/** Ordinal of the latest started fresh-context run. Runs before it never reach a provider again. */
export function contextBoundaryOrdinal(
  projection: Pick<OrchestrationV2ThreadProjection, "runs" | "turnItems">,
): number {
  let scheduled: ReadonlySet<RunId> | undefined;
  let boundary = 0;
  for (const run of projection.runs) {
    if (!startedFreshRun(run) || run.ordinal <= boundary) continue;
    scheduled ??= scheduledRunIds(projection.turnItems);
    if (!scheduled.has(run.id)) {
      boundary = run.ordinal;
    }
  }
  return boundary;
}

type ConversationSource = Pick<
  OrchestrationV2ThreadProjection,
  "runs" | "turnItems" | "providerThreads"
>;

/**
 * The first run of the scheduled fresh session `run` belongs to, or undefined for the main
 * conversation. Every provider thread of a session records that run as its first.
 */
export function scheduledConversationStart(
  projection: ConversationSource,
  run: Pick<OrchestrationV2Run, "providerThreadId">,
): number | undefined {
  if (run.providerThreadId === null) return undefined;
  const first = projection.providerThreads.find(
    (candidate) => candidate.id === run.providerThreadId,
  )?.firstRunOrdinal;
  return first != null && scheduledFreshRunOrdinals(projection).has(first) ? first : undefined;
}

/** For every run of a scheduled fresh session, that session's first run ordinal. */
export function scheduledConversationStarts(
  projection: ConversationSource,
): ReadonlyMap<RunId, number> {
  const scheduled = scheduledFreshRunOrdinals(projection);
  if (scheduled.size === 0) return new Map();
  const sessionStarts = new Map<string, number>();
  for (const providerThread of projection.providerThreads) {
    const first = providerThread.firstRunOrdinal;
    if (first !== null && scheduled.has(first)) sessionStarts.set(providerThread.id, first);
  }
  const starts = new Map<RunId, number>();
  for (const run of projection.runs) {
    const start =
      run.providerThreadId === null ? undefined : sessionStarts.get(run.providerThreadId);
    if (start !== undefined) starts.set(run.id, start);
  }
  return starts;
}

/**
 * The runs through `run` of the conversation it continues, and where that conversation starts: a
 * scheduled fresh session's own runs, a started fresh run alone, or the main conversation since
 * its latest fresh start. The main conversation leaves scheduled fresh sessions out, as its native
 * session never saw them, unless `withScheduledRuns` asks for them (side chats rank them lower).
 */
export function conversationRuns(
  projection: ConversationSource,
  run: OrchestrationV2Run | undefined,
  options?: { readonly withScheduledRuns?: boolean },
): { readonly boundary: number; readonly runIds: ReadonlySet<RunId> } {
  const through = run?.ordinal ?? Number.POSITIVE_INFINITY;
  const start = run === undefined ? undefined : scheduledConversationStart(projection, run);
  if (start !== undefined) {
    const starts = scheduledConversationStarts(projection);
    return {
      boundary: start,
      runIds: new Set(
        projection.runs.flatMap((candidate) =>
          candidate.ordinal <= through && starts.get(candidate.id) === start ? [candidate.id] : [],
        ),
      ),
    };
  }
  const boundary =
    run !== undefined && startedFreshRun(run) ? run.ordinal : contextBoundaryOrdinal(projection);
  const scheduled =
    options?.withScheduledRuns === true ? undefined : scheduledConversationStarts(projection);
  return {
    boundary,
    runIds: new Set(
      projection.runs.flatMap((candidate) =>
        candidate.ordinal >= boundary &&
        candidate.ordinal <= through &&
        scheduled?.has(candidate.id) !== true
          ? [candidate.id]
          : [],
      ),
    ),
  };
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
