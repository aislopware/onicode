import type {
  MessageId,
  OrchestrationV2Run,
  OrchestrationV2ThreadProjection,
} from "@onicode/contracts";

import { cancelledBeforeStart } from "./RestartBackgroundNote.ts";

/** A fresh-session schedule run starts a side conversation; the thread's own one continues past it. */
export function isScheduledFreshRun(
  run: Pick<OrchestrationV2Run, "freshContext" | "userMessageId">,
  turnItems: OrchestrationV2ThreadProjection["turnItems"],
): boolean {
  // Turn items, unlike messages, are not narrowed to the runs since the latest compaction.
  return (
    run.freshContext === true &&
    turnItems.some(
      (item) =>
        item.type === "user_message" &&
        item.messageId === run.userMessageId &&
        item.scheduledTaskId !== undefined,
    )
  );
}

/** Ordinal of the latest started fresh-context run. Runs before it never reach a provider again. */
export function contextBoundaryOrdinal(
  projection: Pick<OrchestrationV2ThreadProjection, "runs" | "turnItems">,
): number {
  // Built once, not searched per run: an hourly fresh-session schedule leaves thousands of fresh runs.
  let scheduledMessageIds: ReadonlySet<MessageId> | undefined;
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
    scheduledMessageIds ??= new Set(
      projection.turnItems.flatMap((item) =>
        item.type === "user_message" && item.scheduledTaskId !== undefined ? [item.messageId] : [],
      ),
    );
    if (!scheduledMessageIds.has(run.userMessageId)) {
      boundary = run.ordinal;
    }
  }
  return boundary;
}
