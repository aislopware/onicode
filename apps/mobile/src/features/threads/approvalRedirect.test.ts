import type {
  ApprovalResponder,
  ThreadPendingApproval,
} from "@onicode/client-runtime/state/thread-requests";
import { RuntimeRequestId } from "@onicode/contracts";
import { AsyncResult } from "effect/reactivity";
import { expect, it, vi } from "vite-plus/test";

import { declineApprovalsBeforeRedirect } from "./approvalRedirect";

const approval: ThreadPendingApproval = {
  requestId: RuntimeRequestId.make("approval"),
  requestKind: "command",
  createdAt: "2026-11-21T00:00:00Z",
  responseCapability: "live",
};

it.each([
  { during: "nothing", redirects: true },
  { during: "draft edit", redirects: false },
  { during: "new approval", redirects: false },
] as const)("redirects after a decline only if $during changed", async ({ during, redirects }) => {
  let draftEdited = false;
  let pending: ReadonlyArray<ThreadPendingApproval> = [approval];
  const respond = vi.fn<ApprovalResponder>(async () => {
    if (during === "draft edit") draftEdited = true;
    pending =
      during === "new approval"
        ? [{ ...approval, requestId: RuntimeRequestId.make("arrived") }]
        : [];
    return AsyncResult.success(undefined);
  });

  const decline = declineApprovalsBeforeRedirect({
    approvals: [approval],
    respond,
    responding: false,
    aborted: () => false,
    stillSendable: () => !draftEdited,
    pendingApprovals: () => pending,
    refuseFeedbackWithAttachments: vi.fn(),
  });

  expect(await decline("Do this instead", false)).toBe(redirects);
  expect(respond).toHaveBeenCalledExactlyOnceWith(approval.requestId, "decline");
});
