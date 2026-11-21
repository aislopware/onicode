import {
  declinePendingApprovals,
  sendApprovalFeedback,
  type ApprovalResponder,
  type ThreadPendingApproval,
} from "@onicode/client-runtime/state/thread-requests";

/**
 * Answers the approvals a sent message redirects: feedback to the first when it takes feedback,
 * otherwise a decline of each. True only when the draft can still go out as it was sent.
 */
export function declineApprovalsBeforeRedirect(input: {
  readonly approvals: ReadonlyArray<ThreadPendingApproval>;
  readonly respond: ApprovalResponder;
  readonly responding: boolean;
  readonly aborted: () => boolean;
  readonly stillSendable?: () => boolean;
  readonly pendingApprovals: () => ReadonlyArray<ThreadPendingApproval>;
  readonly refuseFeedbackWithAttachments: () => void;
}) {
  return async (text: string, hasAttachmentsOrContext: boolean) => {
    if (input.aborted() || input.stillSendable?.() === false || input.responding) return false;
    const feedbackTarget = input.approvals[0];
    if (feedbackTarget?.supportsFeedback) {
      if (hasAttachmentsOrContext) {
        input.refuseFeedbackWithAttachments();
        return false;
      }
      return (await sendApprovalFeedback(feedbackTarget, text, input.respond))
        ? ("responded" as const)
        : false;
    }
    return (
      (await declinePendingApprovals(input.approvals, input.respond)) &&
      !input.aborted() &&
      // A draft edited or pasted into during the decline isn't what the user sent.
      input.stillSendable?.() !== false &&
      // An approval that arrived during the decline would block the redirect; the draft stays so
      // the next send declines it too.
      !input
        .pendingApprovals()
        .some(
          (approval) =>
            !input.approvals.some((declined) => declined.requestId === approval.requestId),
        )
    );
  };
}
