import { isCompactionCommandMessage } from "@onicode/contracts";
import { LIVE_ACTIVITY_ROW_ID, type ThreadFeedEntry } from "../../lib/threadActivity";
import { scopedThreadKey } from "../../lib/scopedEntities";
import { threadHasCaughtUpTo } from "@onicode/client-runtime/state/threads";
import { resolveUserMessageGrouping } from "@onicode/client-runtime/user-message";
import type {
  AcknowledgedThreadMessage,
  QueuedThreadMessage,
} from "../../state/thread-outbox-model";

export type PendingThreadFeedEntry = ThreadFeedEntry & {
  readonly pendingMessage?: QueuedThreadMessage;
  readonly acknowledged?: boolean;
};

// An acknowledged row stops being pending once the thread catches up to it,
// even when the feed hides that message (a rolled-back or cancelled run).
export function selectPendingAcknowledgedMessages(
  acknowledged: ReadonlyArray<AcknowledgedThreadMessage>,
  thread: {
    readonly threadKey: string | null;
    readonly queuedMessages: ReadonlyArray<QueuedThreadMessage>;
    readonly sequence: number;
    readonly projectedMessages: ReadonlyArray<{ readonly id: string }> | undefined;
  },
): ReadonlyArray<AcknowledgedThreadMessage> {
  return acknowledged.filter(
    (message) =>
      scopedThreadKey(message.environmentId, message.threadId) === thread.threadKey &&
      !thread.queuedMessages.some((queued) => queued.messageId === message.messageId) &&
      !threadHasCaughtUpTo(message, {
        sequence: thread.sequence,
        messages: thread.projectedMessages,
      }),
  );
}

/**
 * Holds the turn's fold slot while a sent message waits for its run, so the live "Working for"
 * row replaces it in place instead of being inserted.
 */
export interface PendingFoldFeedEntry {
  readonly type: "pending-fold";
  readonly id: string;
  readonly createdAt: string;
  readonly messageId: string;
}

/** A row of the thread feed's list. */
export type ThreadFeedRow = PendingThreadFeedEntry | PendingFoldFeedEntry;

export type GroupedThreadFeedRow = ThreadFeedRow & {
  readonly userMessageGrouping: ReturnType<typeof resolveUserMessageGrouping>;
};

export function createThreadFeedGrouping() {
  const rows = new WeakMap<ThreadFeedRow, GroupedThreadFeedRow>();
  const ungrouped = resolveUserMessageGrouping(undefined, undefined, undefined);
  return (
    feed: ReadonlyArray<ThreadFeedRow>,
    options: { readonly workflowOpeningMessageId?: string; readonly setupAnchorIndex?: number },
  ): ReadonlyArray<GroupedThreadFeedRow> => {
    const groupingMessage = (index: number) => {
      const entry = feed[index];
      return entry?.type === "message" &&
        entry.message.role === "user" &&
        !isCompactionCommandFeedEntry(entry) &&
        entry.message.id !== options.workflowOpeningMessageId
        ? entry.message
        : undefined;
    };
    return feed.map((entry, index) => {
      const message = groupingMessage(index);
      const grouping =
        message === undefined
          ? ungrouped
          : resolveUserMessageGrouping(
              message,
              index - 1 === options.setupAnchorIndex ? undefined : groupingMessage(index - 1),
              index === options.setupAnchorIndex ? undefined : groupingMessage(index + 1),
            );
      const previous = rows.get(entry);
      if (
        previous?.userMessageGrouping.groupedWithPrevious === grouping.groupedWithPrevious &&
        previous.userMessageGrouping.groupedWithNext === grouping.groupedWithNext &&
        previous.userMessageGrouping.continuesSteer === grouping.continuesSteer
      )
        return previous;
      const row = { ...entry, userMessageGrouping: grouping };
      rows.set(entry, row);
      return row;
    });
  };
}

export function appendPendingThreadMessages(
  presentedFeed: ReadonlyArray<ThreadFeedEntry>,
  feed: ReadonlyArray<ThreadFeedEntry>,
  queuedMessages: ReadonlyArray<QueuedThreadMessage>,
): ReadonlyArray<PendingThreadFeedEntry> {
  if (queuedMessages.length === 0) return presentedFeed;
  const deliveredIds = new Set(
    feed.flatMap((entry) => (entry.type === "message" ? [entry.message.id] : [])),
  );
  return [
    ...presentedFeed.map((entry): PendingThreadFeedEntry => {
      const pendingMessage = queuedMessages.find(
        (message) => message.creation !== undefined && message.messageId === entry.id,
      );
      return pendingMessage ? { ...entry, pendingMessage, acknowledged: false } : entry;
    }),
    ...queuedMessages
      .filter((message) => !deliveredIds.has(message.messageId))
      .map((pendingMessage): PendingThreadFeedEntry => ({
        type: "message",
        id: pendingMessage.messageId,
        createdAt: pendingMessage.createdAt,
        pendingMessage,
        message: {
          id: pendingMessage.messageId,
          role: "user",
          text: pendingMessage.text,
          attachments: [],
          context: pendingMessage.context,
          createdAt: pendingMessage.createdAt,
          updatedAt: pendingMessage.createdAt,
          runId: null,
          streaming: false,
          visibility: "local",
          sourceThreadId: pendingMessage.threadId,
        },
      })),
  ];
}

/**
 * Messages the server accepted but has not projected yet. A new thread's first message stays
 * anchored until the server's copy loads, and keeps its local attachments so its media does
 * not drop out of the feed in between.
 */
export function appendAcceptedThreadMessages(
  feed: ReadonlyArray<ThreadFeedEntry>,
  acknowledged: ReadonlyArray<QueuedThreadMessage>,
  creation: QueuedThreadMessage | null,
): ReadonlyArray<PendingThreadFeedEntry> {
  const accepted = creation === null ? acknowledged : [...acknowledged, creation];
  if (accepted.length === 0) return feed;
  return appendPendingThreadMessages(feed, feed, accepted).map((entry) =>
    entry.pendingMessage ? { ...entry, acknowledged: true } : entry,
  );
}

/**
 * Follows the sent message no run has picked up with the rows its run will fill: the fold slot
 * and the live activity row, under the activity row's own key. Messages queued behind it stay
 * after. The run's rows then replace them in place instead of being inserted.
 */
export function createPendingTurnAppender() {
  let fold: PendingFoldFeedEntry | null = null;
  let activity: Extract<ThreadFeedEntry, { readonly type: "thinking" }> | null = null;
  return (
    feed: ReadonlyArray<PendingThreadFeedEntry>,
    options: { readonly zen: boolean; readonly dispatchingMessageId: string | null },
  ): ReadonlyArray<PendingThreadFeedEntry | PendingFoldFeedEntry> => {
    // Queued behind a lost connection or missing access, a message is waiting, not sending.
    // The oldest sent message runs first, so its rows go under it.
    let anchorIndex = -1;
    for (let index = feed.length - 1; index >= 0; index -= 1) {
      const entry = feed[index];
      if (entry?.type !== "message" || entry.pendingMessage === undefined) break;
      if (entry.acknowledged || entry.pendingMessage.messageId === options.dispatchingMessageId) {
        anchorIndex = index;
      }
    }
    const anchor = feed[anchorIndex];
    if (
      anchor?.type !== "message" ||
      anchor.message.role !== "user" ||
      anchor.pendingMessage === undefined ||
      anchor.pendingMessage.creation !== undefined ||
      isCompactionCommandMessage(anchor.message) ||
      feed.some(
        (entry) =>
          (entry.type === "run-fold" && entry.workingSince !== undefined) ||
          entry.type === "thinking",
      )
    ) {
      return feed;
    }
    const label = anchor.acknowledged ? "Starting…" : "Sending…";
    if (activity?.createdAt !== anchor.createdAt || activity.label !== label) {
      activity = {
        type: "thinking",
        id: LIVE_ACTIVITY_ROW_ID,
        createdAt: anchor.createdAt,
        runId: null,
        label,
      };
    }
    let rows: ReadonlyArray<PendingFoldFeedEntry | typeof activity> = [activity];
    if (!options.zen) {
      if (fold?.messageId !== anchor.id) {
        fold = {
          type: "pending-fold",
          id: `pending-fold:${anchor.id}`,
          createdAt: anchor.createdAt,
          messageId: anchor.id,
        };
      }
      rows = [fold, activity];
    }
    return [...feed.slice(0, anchorIndex + 1), ...rows, ...feed.slice(anchorIndex + 1)];
  };
}

export function isCompactionCommandFeedEntry(entry: PendingThreadFeedEntry): boolean {
  return (
    entry.type === "message" &&
    entry.pendingMessage === undefined &&
    isCompactionCommandMessage(entry.message)
  );
}
