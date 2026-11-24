import type { AgentSignInFailureReason } from "@onicode/contracts";
import { mFallback, msg } from "gt-i18n";
import type { MFunctionType } from "gt-i18n/types";

const FAILURE_MESSAGES: Record<AgentSignInFailureReason, string> = {
  not_found: msg("This sign-in already finished or expired."),
  policy_denied: msg("Your organization does not allow outside agents on this environment."),
  scope_required: msg("This device cannot grant that access. Choose a lower level."),
  issue_failed: msg("Could not create the agent's access. Try again."),
};

/** Why answering a sign-in or issuing an agent's access failed, from the command's failure. */
export function agentSignInFailureMessage(
  error: unknown,
  resolve: MFunctionType = mFallback,
): string {
  const tagged =
    typeof error === "object" && error !== null && "_tag" in error && "reason" in error
      ? (error as { readonly _tag: unknown; readonly reason: unknown })
      : null;
  const reason =
    tagged?._tag === "AgentSignInError" && typeof tagged.reason === "string"
      ? (tagged.reason as AgentSignInFailureReason)
      : "issue_failed";
  return resolve(FAILURE_MESSAGES[reason] ?? FAILURE_MESSAGES.issue_failed);
}
