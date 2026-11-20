import {
  latestProviderTurnForAttempt,
  MessageId,
  type OrchestrationV2ConversationMessage,
  type OrchestrationV2ThreadProjection,
  RuntimeRequestId,
  type ThreadId,
} from "@onicode/contracts";

const QUESTION_WAKE_PREFIX = "message:delegated-task-question:";

/** The parent's wake for one question of a delegated task. */
export const delegatedTaskQuestionMessageId = (
  parentThreadId: ThreadId,
  requestId: RuntimeRequestId,
): MessageId => MessageId.make(`${QUESTION_WAKE_PREFIX}${parentThreadId}:${requestId}`);

/** The question a wake asks about; undefined for any other message. */
export const delegatedTaskQuestionRequestId = (
  parentThreadId: ThreadId,
  messageId: MessageId,
): RuntimeRequestId | undefined => {
  const prefix = `${QUESTION_WAKE_PREFIX}${parentThreadId}:`;
  return messageId.startsWith(prefix) && messageId.length > prefix.length
    ? RuntimeRequestId.make(messageId.slice(prefix.length))
    : undefined;
};

export function isBufferedProviderNotification(
  message: Pick<
    OrchestrationV2ConversationMessage,
    "createdBy" | "creationSource" | "notification" | "delegatedCompletion"
  >,
): boolean {
  return (
    message.createdBy === "agent" &&
    message.creationSource === "provider" &&
    message.notification !== undefined &&
    message.delegatedCompletion === undefined
  );
}

/** Acceptance and receipts cannot commit atomically; reuse message ids for at-least-once delivery. */
export function isUndeliveredMailboxSteer(
  projection: Pick<OrchestrationV2ThreadProjection, "messages" | "runs" | "providerTurns">,
  messageId: MessageId,
): boolean {
  const message = projection.messages.find((candidate) => candidate.id === messageId);
  if (message?.delegatedCompletion === undefined) return false;
  const run = projection.runs.find((candidate) => candidate.id === message.runId);
  return (
    run !== undefined &&
    run.userMessageId !== message.id &&
    (["completed", "failed", "interrupted", "cancelled", "rolled_back"].includes(run.status) ||
      latestProviderTurnForAttempt(projection.providerTurns, run.activeAttemptId)?.status ===
        "completed")
  );
}
