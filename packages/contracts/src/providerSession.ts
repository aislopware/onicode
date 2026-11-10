import * as Schema from "effect/Schema";

import { ThreadId, TrimmedNonEmptyString } from "./baseSchemas.ts";

// Session requests read the thread's live provider process; they never start one.
export const ProviderSessionThreadInput = Schema.Struct({
  threadId: ThreadId,
});
export type ProviderSessionThreadInput = typeof ProviderSessionThreadInput.Type;

export const ProviderContextUsageCategoryKind = Schema.Literals([
  "used",
  "free",
  "buffer",
  "deferred",
]);
export type ProviderContextUsageCategoryKind = typeof ProviderContextUsageCategoryKind.Type;

export const ProviderContextUsageCategory = Schema.Struct({
  /** Stable when the provider names the category (`mcp_tools`, `memory_files`, `skills`…). */
  id: Schema.optional(Schema.String),
  name: Schema.String,
  tokens: Schema.Number,
  kind: ProviderContextUsageCategoryKind,
});
export type ProviderContextUsageCategory = typeof ProviderContextUsageCategory.Type;

export const ProviderContextUsage = Schema.Struct({
  model: Schema.String,
  totalTokens: Schema.Number,
  maxTokens: Schema.Number,
  percentage: Schema.Number,
  autoCompactThreshold: Schema.optional(Schema.Number),
  isAutoCompactEnabled: Schema.Boolean,
  categories: Schema.Array(ProviderContextUsageCategory),
  mcpTools: Schema.Array(
    Schema.Struct({ server: Schema.String, name: Schema.String, tokens: Schema.Number }),
  ),
  memoryFiles: Schema.Array(
    Schema.Struct({ path: Schema.String, type: Schema.String, tokens: Schema.Number }),
  ),
  agents: Schema.Array(
    Schema.Struct({
      name: Schema.String,
      source: Schema.optional(Schema.String),
      tokens: Schema.Number,
    }),
  ),
  skills: Schema.Array(
    Schema.Struct({
      name: Schema.String,
      source: Schema.optional(Schema.String),
      tokens: Schema.Number,
    }),
  ),
});
export type ProviderContextUsage = typeof ProviderContextUsage.Type;

export const ProviderMcpServerStatus = Schema.Literals([
  "connected",
  "pending",
  "needs-auth",
  "failed",
  "disabled",
]);
export type ProviderMcpServerStatus = typeof ProviderMcpServerStatus.Type;

export const ProviderMcpServer = Schema.Struct({
  name: Schema.String,
  status: ProviderMcpServerStatus,
  /** Where the server is configured, e.g. `user`, `project`, `local`, `plugin`. */
  scope: Schema.optional(Schema.String),
  /** `stdio`, `http`, `sse`…; only remote transports can sign in. */
  transport: Schema.optional(Schema.String),
  error: Schema.optional(Schema.String),
  tools: Schema.Array(
    Schema.Struct({ name: Schema.String, description: Schema.optional(Schema.String) }),
  ),
});
export type ProviderMcpServer = typeof ProviderMcpServer.Type;

export const ProviderMcpServerList = Schema.Struct({
  servers: Schema.Array(ProviderMcpServer),
});
export type ProviderMcpServerList = typeof ProviderMcpServerList.Type;

export const ProviderMcpServerAction = Schema.Literals([
  "reconnect",
  "enable",
  "disable",
  "authenticate",
  "clear-auth",
]);
export type ProviderMcpServerAction = typeof ProviderMcpServerAction.Type;

export const ProviderMcpServerActionInput = Schema.Struct({
  threadId: ThreadId,
  server: TrimmedNonEmptyString,
  action: ProviderMcpServerAction,
});
export type ProviderMcpServerActionInput = typeof ProviderMcpServerActionInput.Type;

export const ProviderMcpServerActionResult = Schema.Struct({
  /** Set by `authenticate` when the user must finish signing in in a browser. */
  authUrl: Schema.optional(Schema.String),
});
export type ProviderMcpServerActionResult = typeof ProviderMcpServerActionResult.Type;

/** The browser's final redirect URL, for clients that cannot reach the provider's local callback. */
export const ProviderMcpOAuthCallbackInput = Schema.Struct({
  threadId: ThreadId,
  server: TrimmedNonEmptyString,
  callbackUrl: TrimmedNonEmptyString.check(Schema.isMaxLength(8_192)),
});
export type ProviderMcpOAuthCallbackInput = typeof ProviderMcpOAuthCallbackInput.Type;

export const ProviderSessionRequestErrorReason = Schema.Literals([
  "no-session",
  "provider-unsupported",
  "request-failed",
]);
export type ProviderSessionRequestErrorReason = typeof ProviderSessionRequestErrorReason.Type;

export class ProviderSessionRequestError extends Schema.TaggedError<ProviderSessionRequestError>()(
  "ProviderSessionRequestError",
  {
    threadId: ThreadId,
    reason: ProviderSessionRequestErrorReason,
    detail: Schema.optional(Schema.String),
    cause: Schema.optional(Schema.Defect()),
  },
) {
  override get message(): string {
    if (this.detail !== undefined) return this.detail;
    switch (this.reason) {
      case "no-session":
        return "The agent is not running in this thread. Send a message to start it.";
      case "provider-unsupported":
        return "This provider does not support this request.";
      case "request-failed":
        return "The provider request failed.";
    }
  }
}
