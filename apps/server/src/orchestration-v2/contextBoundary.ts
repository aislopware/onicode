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

/**
 * The first run of the scheduled fresh session `run` belongs to: the run itself, or the schedule's
 * run when `run` is a wake on that session's provider thread. Undefined for the main conversation.
 */
export function scheduledConversationStart(
  projection: Pick<OrchestrationV2ThreadProjection, "runs" | "turnItems">,
  run: Pick<OrchestrationV2Run, "ordinal" | "providerThreadId">,
): number | undefined {
  if (run.providerThreadId === null) return undefined;
  let first: OrchestrationV2Run | undefined;
  for (const candidate of projection.runs) {
    if (
      candidate.providerThreadId === run.providerThreadId &&
      candidate.ordinal <= run.ordinal &&
      (first === undefined || candidate.ordinal < first.ordinal)
    ) {
      first = candidate;
    }
  }
  return first !== undefined &&
    startedFreshRun(first) &&
    scheduledRunIds(projection.turnItems).has(first.id)
    ? first.ordinal
    : undefined;
}

/** For every run of a scheduled fresh session's conversation, that session's first run ordinal. */
export function scheduledConversationStarts(
  projection: Pick<OrchestrationV2ThreadProjection, "runs" | "turnItems">,
): ReadonlyMap<RunId, number> {
  const first = new Map<string, OrchestrationV2Run>();
  for (const run of projection.runs) {
    if (run.providerThreadId === null) continue;
    const held = first.get(run.providerThreadId);
    if (held === undefined || run.ordinal < held.ordinal) first.set(run.providerThreadId, run);
  }
  const scheduled = scheduledRunIds(projection.turnItems);
  const starts = new Map<RunId, number>();
  for (const run of projection.runs) {
    const start = run.providerThreadId === null ? undefined : first.get(run.providerThreadId);
    if (start !== undefined && startedFreshRun(start) && scheduled.has(start.id)) {
      starts.set(run.id, start.ordinal);
    }
  }
  return starts;
}

/**
 * Where the conversation `run` continues starts. A started fresh run saw nothing before itself, and
 * a scheduled fresh session's conversation, its wakes included, nothing before the schedule's run.
 */
export function runContextBoundaryOrdinal(
  projection: Pick<OrchestrationV2ThreadProjection, "runs" | "turnItems">,
  run: OrchestrationV2Run | undefined,
): number {
  if (run === undefined) return contextBoundaryOrdinal(projection);
  if (startedFreshRun(run)) return run.ordinal;
  return scheduledConversationStart(projection, run) ?? contextBoundaryOrdinal(projection);
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
