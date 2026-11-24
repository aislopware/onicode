import * as Schema from "effect/Schema";

import { AuthMcpClientAccess } from "./auth.ts";
import { TrimmedNonEmptyString } from "./baseSchemas.ts";

/**
 * An outside agent's OAuth sign-in waiting on this environment's consent page. Any signed-in
 * client that may manage access can approve it; the page and the app show the same match code.
 */
export const AgentSignInRequest = Schema.Struct({
  id: TrimmedNonEmptyString,
  clientName: Schema.String,
  /** Where the agent's sign-in returns to, such as `localhost` or `claude.ai`. */
  redirectHost: Schema.String,
  /** The redirect stays on this machine (localhost), as a local agent's does. */
  loopbackRedirect: Schema.Boolean,
  /** Also shown on the consent page, so the user approves the sign-in in front of them. */
  matchCode: TrimmedNonEmptyString,
  expiresAt: Schema.DateTimeUtc,
});
export type AgentSignInRequest = typeof AgentSignInRequest.Type;

export const AgentSignInAnswer = Schema.Union([
  Schema.TaggedStruct("approve", { access: AuthMcpClientAccess }),
  Schema.TaggedStruct("deny", {}),
]);
export type AgentSignInAnswer = typeof AgentSignInAnswer.Type;

export const AgentSignInAnswerInput = Schema.Struct({
  id: TrimmedNonEmptyString,
  answer: AgentSignInAnswer,
});
export type AgentSignInAnswerInput = typeof AgentSignInAnswerInput.Type;

export const AgentTokenIssueInput = Schema.Struct({
  label: TrimmedNonEmptyString,
  access: AuthMcpClientAccess,
});
export type AgentTokenIssueInput = typeof AgentTokenIssueInput.Type;

export const AgentTokenIssueResult = Schema.Struct({
  token: Schema.RedactedFromValue(TrimmedNonEmptyString),
  expiresAt: Schema.DateTimeUtc,
});
export type AgentTokenIssueResult = typeof AgentTokenIssueResult.Type;

const AGENT_SIGN_IN_FAILURE_MESSAGES = {
  not_found: "This sign-in already finished or expired. Start it again from the agent.",
  policy_denied: "Your organization does not allow outside agents on this environment.",
  scope_required:
    "This device cannot grant that access. Choose a lower level, or approve from a device with more permissions.",
  issue_failed: "Could not create the agent's access. Try again.",
} as const;

export const AgentSignInFailureReason = Schema.Literals(
  Object.keys(AGENT_SIGN_IN_FAILURE_MESSAGES) as Array<keyof typeof AGENT_SIGN_IN_FAILURE_MESSAGES>,
);
export type AgentSignInFailureReason = typeof AgentSignInFailureReason.Type;

export class AgentSignInError extends Schema.TaggedError<AgentSignInError>()("AgentSignInError", {
  reason: AgentSignInFailureReason,
  cause: Schema.optionalKey(Schema.Defect()),
}) {
  override get message(): string {
    return AGENT_SIGN_IN_FAILURE_MESSAGES[this.reason];
  }
}
