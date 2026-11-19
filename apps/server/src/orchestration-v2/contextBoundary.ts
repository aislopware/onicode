import type { MessageId, OrchestrationV2ThreadProjection } from "@onicode/contracts";

import { cancelledBeforeStart } from "./RestartBackgroundNote.ts";

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
  const messageIds = scheduledMessageIds(projection.turnItems);
  return new Set(
    projection.runs.flatMap((run) =>
      run.freshContext === true && messageIds.has(run.userMessageId) ? [run.ordinal] : [],
    ),
  );
}

/** Ordinal of the latest started fresh-context run. Runs before it never reach a provider again. */
export function contextBoundaryOrdinal(
  projection: Pick<OrchestrationV2ThreadProjection, "runs" | "turnItems">,
): number {
  let scheduled: ReadonlySet<MessageId> | undefined;
  let boundary = 0;
  for (const run of projection.runs) {
    if (
      run.freshContext !== true ||
      run.status === "queued" ||
      cancelledBeforeStart(run) ||
      run.ordinal <= boundary
    ) {
      continue;
    }
    scheduled ??= scheduledMessageIds(projection.turnItems);
    if (!scheduled.has(run.userMessageId)) {
      boundary = run.ordinal;
    }
  }
  return boundary;
}
