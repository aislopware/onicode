// @effect-diagnostics nodeBuiltinImport:off -- Pure synchronous hashing keeps page matching bounded without a runtime dependency.
import * as NodeCrypto from "node:crypto";
import type { ProviderApprovalPushHooks, ProviderInstanceId, ThreadId } from "@onicode/contracts";
import type { HtmlRenderReference } from "@onicode/shared/htmlRender";
import { resolveOniMcpToolId } from "@onicode/shared/oniMcpToolPresentation";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Predicate from "effect/Predicate";
import * as Ref from "effect/Ref";

import type { McpProviderSessionConfig } from "./mcpSession.ts";
import { randomUuidV4 } from "./randomUuid.ts";

// Other providers may never consume these references.
const MAX_KEPT_HTML_RENDERS = 64;
const MAX_KEPT_TOOL_RESULTS = 64;
// Screenshots make kept results large; a thread keeps at most this much until its turn ends.
const MAX_KEPT_TOOL_RESULT_BYTES = 32 * 1024 * 1024;
// A request whose call never comes back for its answer stays until the session ends.
const MAX_KEPT_APPROVAL_REQUESTS = 8;

// Antigravity truncates long HTML arguments to a marked tail of at most 8,000 characters.
const BOUNDED_TEXT_MARKER = "[Earlier output truncated]\n\n";
const KEPT_HTML_END_LENGTH = 8_000;

interface KeptHtmlRender {
  readonly path: string | undefined;
  readonly title: string;
  readonly height: number;
  readonly htmlLength: number;
  readonly htmlHash: string;
  readonly htmlEnd: string;
  readonly reference: HtmlRenderReference;
}

interface KeptToolResult {
  readonly toolName: string;
  readonly inputHash: string;
  readonly outputJson: string;
  readonly output: Record<string, unknown>;
}

/** What one of Onicode's own MCP tools is asking the person behind a thread to approve. */
export interface OniApprovalRequest {
  /** The whole question, as the provider's client hands it on. */
  readonly message: string;
  readonly pushHooks?: ProviderApprovalPushHooks | undefined;
  /** What an answer is given for; an answer to another state does not count for this request. */
  readonly state?: string | undefined;
}

/** A request Onicode is waiting on, and what the person answered once an adapter has shown it. */
export interface KeptApprovalRequest extends OniApprovalRequest {
  readonly id: string;
  readonly approved?: boolean | undefined;
}

interface ToolResults {
  readonly epoch: number;
  readonly kept: ReadonlyArray<KeptToolResult>;
}

interface ProviderSession {
  readonly config: McpProviderSessionConfig;
  /** Set once the adapter asks for capture; most adapters never take results back. */
  readonly toolResults: ToolResults | undefined;
}

// Key order must not change a hash: adapters hash the call's input as the agent echoed it.
const stableStringify = (value: unknown): string => {
  if (Array.isArray(value)) return `[${value.map(stableStringify).join(",")}]`;
  if (Predicate.isObject(value)) {
    return `{${Object.entries(value)
      .filter(([, entry]) => entry !== undefined)
      .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0))
      .map(([key, entry]) => `${JSON.stringify(key)}:${stableStringify(entry)}`)
      .join(",")}}`;
  }
  return JSON.stringify(value) ?? "null";
};

const hashText = (text: string) => NodeCrypto.createHash("sha256").update(text).digest("hex");

export const hashToolInput = (input: unknown): string => {
  const normalized = Predicate.isObject(input) && !Array.isArray(input) ? input : {};
  return hashText(stableStringify(normalized));
};

export class McpProviderSessions extends Context.Service<
  McpProviderSessions,
  {
    readonly set: (config: McpProviderSessionConfig) => Effect.Effect<void>;
    readonly read: (threadId: ThreadId) => Effect.Effect<McpProviderSessionConfig | undefined>;
    readonly clear: (threadId: ThreadId) => Effect.Effect<void>;
    readonly recordHtmlRender: (
      threadId: ThreadId,
      input: {
        readonly html: string;
        readonly path?: string | undefined;
        readonly title: string;
        readonly height: number;
      },
      reference: HtmlRenderReference,
    ) => Effect.Effect<void>;
    // A reported attachment consumes that exact page; otherwise the oldest page for a path, or a
    // unique inline match.
    readonly takeHtmlRender: (
      threadId: ThreadId,
      input: PageInput,
      reported?: HtmlRenderReference,
    ) => Effect.Effect<HtmlRenderReference | undefined>;
    /** Called by adapters that take results back, before the provider gets the MCP session. */
    readonly captureToolResults: (threadId: ThreadId) => Effect.Effect<void>;
    /** Undefined while the thread's session does not capture. */
    readonly toolResultEpoch: (threadId: ThreadId) => Effect.Effect<number | undefined>;
    readonly recordToolResult: (
      threadId: ThreadId,
      toolName: string,
      input: unknown,
      output: Record<string, unknown>,
      options?: {
        readonly providerSessionId?: string | undefined;
        readonly epoch?: number | undefined;
      },
    ) => Effect.Effect<void>;
    // A reported output consumes only an exact (tool, input, output) match; input-only recovery
    // consumes a match only when all matching candidates carry the same output.
    readonly takeToolResult: (
      threadId: ThreadId,
      toolName: string,
      input: unknown,
      reported?: Record<string, unknown>,
    ) => Effect.Effect<Record<string, unknown> | undefined>;
    readonly clearToolResults: (threadId: ThreadId) => Effect.Effect<void>;
    // The provider's client hands an adapter a server name and the message. This is how the
    // adapter learns a request is Onicode's own, and what it names, from Onicode: kept from when
    // Onicode asks until it has the answer. Recording returns the request's own id, and only that
    // id forgets it: two calls may ask the same thing at once, and one may outlive its session.
    readonly recordApprovalRequest: (
      threadId: ThreadId,
      request: OniApprovalRequest,
    ) => Effect.Effect<string>;
    readonly findApprovalRequest: (
      threadId: ThreadId,
      message: string,
    ) => Effect.Effect<KeptApprovalRequest | undefined>;
    readonly forgetApprovalRequest: (threadId: ThreadId, id: string) => Effect.Effect<void>;
    // An adapter that shows Onicode's own requests says so for its provider instance and passes
    // on each answer the person gave in the app. It is handed the message and no id, so the answer
    // counts for every request of the thread asking `message` that has none yet. For those
    // instances that answer, not the client's reply, is what Onicode goes by.
    readonly showsApprovalRequests: (providerInstanceId: ProviderInstanceId) => Effect.Effect<void>;
    // An adapter that recovers Onicode tool results says so for its provider instance, so screenshot
    // results are kept only where something will take them back out.
    readonly recoversToolResultImages: (
      providerInstanceId: ProviderInstanceId,
    ) => Effect.Effect<void>;
    readonly areToolResultImagesRecovered: (
      providerInstanceId: ProviderInstanceId,
    ) => Effect.Effect<boolean>;
    readonly areApprovalRequestsShown: (
      providerInstanceId: ProviderInstanceId,
    ) => Effect.Effect<boolean>;
    readonly answerApprovalRequests: (
      threadId: ThreadId,
      message: string,
      approved: boolean,
    ) => Effect.Effect<void>;
    // Ends the request and returns it as it was, with the person's answer if one was given.
    readonly takeApprovalRequest: (
      threadId: ThreadId,
      id: string,
    ) => Effect.Effect<KeptApprovalRequest | undefined>;
  }
>()("@onicode/provider-core/server/McpProviderSessions") {}

interface PageInput {
  readonly html?: unknown;
  readonly path?: unknown;
  readonly title?: unknown;
  readonly height?: unknown;
}

const matchingPageIndex = (kept: ReadonlyArray<KeptHtmlRender>, input: PageInput): number => {
  const height =
    typeof input.height === "string" && input.height.trim() !== ""
      ? Number(input.height)
      : input.height;
  const sameFrame = (page: KeptHtmlRender) => page.title === input.title && page.height === height;
  const path = input.path;
  // The handler refuses a call with both sources, so it published nothing to recover. Its decoder
  // reads null as an omitted source.
  if (path !== undefined && path !== null && input.html !== undefined && input.html !== null) {
    return -1;
  }
  if (typeof path === "string" && path.length > 0) {
    // An edited file is published again under the same path; completions arrive in publish order.
    return kept.findIndex((page) => page.path === path && sameFrame(page));
  }
  const html = input.html;
  if (typeof html !== "string" || html.length === 0) {
    return -1;
  }
  const htmlHash = hashText(html);
  const tail = html.startsWith(BOUNDED_TEXT_MARKER)
    ? html.slice(BOUNDED_TEXT_MARKER.length)
    : undefined;
  const matches = kept.flatMap((page, index) =>
    page.path === undefined &&
    sameFrame(page) &&
    (page.htmlHash === htmlHash ||
      (tail !== undefined &&
        tail.length > 0 &&
        tail.length < page.htmlLength &&
        page.htmlEnd.endsWith(tail)))
      ? [index]
      : [],
  );
  // Identical pages are interchangeable, so repeats are taken in publish order.
  if (matches.length > 1 && matches.every((index) => kept[index]!.htmlHash === htmlHash)) {
    return matches[0]!;
  }
  return matches.length === 1 ? matches[0]! : -1;
};

const matchingToolResultIndex = (
  kept: ReadonlyArray<KeptToolResult>,
  toolName: string,
  input: unknown,
  reported?: Record<string, unknown>,
): number => {
  const inputHash = hashToolInput(input);
  if (reported !== undefined) {
    const reportedJson = stableStringify(reported);
    return kept.findIndex(
      (entry) =>
        entry.toolName === toolName &&
        entry.inputHash === inputHash &&
        entry.outputJson === reportedJson,
    );
  }
  const matches = kept.flatMap((entry, index) =>
    entry.toolName === toolName && entry.inputHash === inputHash ? [index] : [],
  );
  if (matches.length === 0) return -1;
  if (matches.length === 1) return matches[0]!;
  // Identical outputs (e.g. repeated send_to_user calls) are interchangeable; differing outputs
  // (e.g. parallel identical-input delegate_task calls) stay unclaimed to avoid misattribution.
  const firstOutput = kept[matches[0]!]!.outputJson;
  return matches.every((index) => kept[index]!.outputJson === firstOutput) ? matches[0]! : -1;
};

/**
 * Drops the oldest results until the thread fits. Every result kept for the same call input goes
 * with it, so an input-only lookup can never hand the dropped call's sibling output to another call.
 */
const withinByteLimit = (kept: ReadonlyArray<KeptToolResult>): ReadonlyArray<KeptToolResult> => {
  let rest = kept;
  let bytes = rest.reduce((total, entry) => total + entry.outputJson.length, 0);
  while (bytes > MAX_KEPT_TOOL_RESULT_BYTES && rest.length > 0) {
    const oldest = rest[0]!;
    const dropped = rest.filter(
      (entry) => entry.toolName === oldest.toolName && entry.inputHash === oldest.inputHash,
    );
    rest = rest.filter((entry) => !dropped.includes(entry));
    bytes -= dropped.reduce((total, entry) => total + entry.outputJson.length, 0);
  }
  return rest;
};

const updateToolResults = (
  sessions: ReadonlyMap<ThreadId, ProviderSession>,
  threadId: ThreadId,
  update: (current: ToolResults) => ToolResults,
): ReadonlyMap<ThreadId, ProviderSession> => {
  const session = sessions.get(threadId);
  return session?.toolResults === undefined
    ? sessions
    : new Map(sessions).set(threadId, {
        ...session,
        toolResults: update(session.toolResults),
      });
};

const make = Effect.gen(function* () {
  // One Ref, so a late result cannot land between a session's removal and its results'.
  const sessions = yield* Ref.make<ReadonlyMap<ThreadId, ProviderSession>>(new Map());
  const htmlRenders = yield* Ref.make(new Map<ThreadId, ReadonlyArray<KeptHtmlRender>>());
  const approvalRequests = yield* Ref.make(new Map<ThreadId, ReadonlyArray<KeptApprovalRequest>>());
  const approvalRequestsShownBy = yield* Ref.make<ReadonlySet<ProviderInstanceId>>(new Set());
  const toolResultImagesRecoveredBy = yield* Ref.make<ReadonlySet<ProviderInstanceId>>(new Set());
  const withoutThread = <A>(kept: ReadonlyMap<ThreadId, A>, threadId: ThreadId) => {
    const next = new Map(kept);
    next.delete(threadId);
    return next;
  };
  const takeApprovalRequest = (threadId: ThreadId, id: string) =>
    Ref.modify(approvalRequests, (current) => {
      const kept = current.get(threadId);
      const taken = kept?.find((candidate) => candidate.id === id);
      if (kept === undefined || taken === undefined) return [undefined, current];
      const rest = kept.filter((candidate) => candidate !== taken);
      return [
        taken,
        rest.length === 0 ? withoutThread(current, threadId) : new Map(current).set(threadId, rest),
      ];
    });
  return McpProviderSessions.of({
    set: Effect.fn("McpProviderSessions.set")(function* (config) {
      const current = yield* Ref.get(sessions);
      if (current.get(config.threadId)?.config.providerSessionId !== config.providerSessionId) {
        yield* Ref.update(htmlRenders, (kept) => {
          const next = new Map(kept);
          next.delete(config.threadId);
          return next;
        });
        yield* Ref.update(approvalRequests, (kept) => withoutThread(kept, config.threadId));
      }
      yield* Ref.update(sessions, (current) => {
        const existing = current.get(config.threadId);
        return new Map(current).set(config.threadId, {
          config,
          toolResults:
            existing?.config.providerSessionId === config.providerSessionId
              ? existing.toolResults
              : undefined,
        });
      });
    }),
    read: Effect.fn("McpProviderSessions.read")(function (threadId) {
      return Ref.get(sessions).pipe(Effect.map((current) => current.get(threadId)?.config));
    }),
    clear: Effect.fn("McpProviderSessions.clear")(function (threadId) {
      return Ref.update(sessions, (current) => {
        const next = new Map(current);
        next.delete(threadId);
        return next;
      }).pipe(
        Effect.andThen(
          Ref.update(htmlRenders, (current) => {
            const next = new Map(current);
            next.delete(threadId);
            return next;
          }),
        ),
        Effect.andThen(Ref.update(approvalRequests, (kept) => withoutThread(kept, threadId))),
      );
    }),
    recordHtmlRender: Effect.fn("McpProviderSessions.recordHtmlRender")(
      function (threadId, input, reference) {
        return Ref.update(htmlRenders, (current) =>
          new Map(current).set(
            threadId,
            [
              ...(current.get(threadId) ?? []),
              {
                path: input.path,
                title: input.title,
                height: input.height,
                htmlLength: input.html.length,
                htmlHash: hashText(input.html),
                // Copy the tail so its backing storage cannot retain the entire HTML string.
                htmlEnd: Buffer.from(input.html.slice(-KEPT_HTML_END_LENGTH), "utf16le").toString(
                  "utf16le",
                ),
                reference,
              },
            ].slice(-MAX_KEPT_HTML_RENDERS),
          ),
        );
      },
    ),
    takeHtmlRender: Effect.fn("McpProviderSessions.takeHtmlRender")(
      function (threadId, input, reported) {
        return Ref.modify(htmlRenders, (current) => {
          const kept = current.get(threadId) ?? [];
          const index =
            reported === undefined
              ? matchingPageIndex(kept, input)
              : kept.findIndex((page) => page.reference.attachmentId === reported.attachmentId);
          if (index === -1) {
            return [undefined, current];
          }
          const rest = kept.filter((_, keptIndex) => keptIndex !== index);
          const next = new Map(current);
          if (rest.length === 0) {
            next.delete(threadId);
          } else {
            next.set(threadId, rest);
          }
          return [kept[index]!.reference, next];
        });
      },
    ),
    captureToolResults: Effect.fn("McpProviderSessions.captureToolResults")(function (threadId) {
      return Ref.update(sessions, (current) => {
        const session = current.get(threadId);
        return session === undefined || session.toolResults !== undefined
          ? current
          : new Map(current).set(threadId, { ...session, toolResults: { epoch: 0, kept: [] } });
      });
    }),
    toolResultEpoch: Effect.fn("McpProviderSessions.toolResultEpoch")(function (threadId) {
      return Ref.get(sessions).pipe(
        Effect.map((current) => current.get(threadId)?.toolResults?.epoch),
      );
    }),
    recordToolResult: Effect.fn("McpProviderSessions.recordToolResult")(
      function (threadId, toolName, input, output, options) {
        return Ref.update(sessions, (current) => {
          const session = current.get(threadId);
          if (
            session?.toolResults === undefined ||
            // A handler finishing after session replacement must not populate the new session.
            (options?.providerSessionId !== undefined &&
              options.providerSessionId !== session.config.providerSessionId) ||
            // A handler finishing after its turn closed must not populate the next turn.
            (options?.epoch !== undefined && options.epoch !== session.toolResults.epoch)
          ) {
            return current;
          }
          const entry: KeptToolResult = {
            toolName: resolveOniMcpToolId(toolName) ?? toolName,
            inputHash: hashToolInput(input),
            outputJson: stableStringify(output),
            output,
          };
          return updateToolResults(current, threadId, ({ epoch, kept }) => ({
            epoch,
            kept: withinByteLimit([...kept, entry].slice(-MAX_KEPT_TOOL_RESULTS)),
          }));
        });
      },
    ),
    takeToolResult: Effect.fn("McpProviderSessions.takeToolResult")(
      function (threadId, toolName, input, reported) {
        const canonicalTool = resolveOniMcpToolId(toolName) ?? toolName;
        return Ref.modify(sessions, (current) => {
          const kept = current.get(threadId)?.toolResults?.kept ?? [];
          const index = matchingToolResultIndex(kept, canonicalTool, input, reported);
          if (index === -1) {
            return [undefined, current];
          }
          return [
            kept[index]!.output,
            updateToolResults(current, threadId, ({ epoch }) => ({
              epoch,
              kept: kept.filter((_, keptIndex) => keptIndex !== index),
            })),
          ];
        });
      },
    ),
    clearToolResults: Effect.fn("McpProviderSessions.clearToolResults")(function (threadId) {
      return Ref.update(sessions, (current) =>
        updateToolResults(current, threadId, ({ epoch }) => ({ epoch: epoch + 1, kept: [] })),
      );
    }),
    recordApprovalRequest: Effect.fn("McpProviderSessions.recordApprovalRequest")(
      function (threadId, request) {
        return randomUuidV4.pipe(
          Effect.tap((id) =>
            Ref.update(approvalRequests, (current) =>
              new Map(current).set(
                threadId,
                [...(current.get(threadId) ?? []), { ...request, id }].slice(
                  -MAX_KEPT_APPROVAL_REQUESTS,
                ),
              ),
            ),
          ),
        );
      },
    ),
    findApprovalRequest: Effect.fn("McpProviderSessions.findApprovalRequest")(
      function (threadId, message) {
        return Ref.get(approvalRequests).pipe(
          Effect.map((current) => current.get(threadId)?.find((kept) => kept.message === message)),
        );
      },
    ),
    forgetApprovalRequest: Effect.fn("McpProviderSessions.forgetApprovalRequest")(
      function (threadId, id) {
        return Effect.asVoid(takeApprovalRequest(threadId, id));
      },
    ),
    recoversToolResultImages: Effect.fn("McpProviderSessions.recoversToolResultImages")(
      function (providerInstanceId) {
        return Ref.update(toolResultImagesRecoveredBy, (current) =>
          new Set(current).add(providerInstanceId),
        );
      },
    ),
    areToolResultImagesRecovered: Effect.fn("McpProviderSessions.areToolResultImagesRecovered")(
      function (providerInstanceId) {
        return Ref.get(toolResultImagesRecoveredBy).pipe(
          Effect.map((current) => current.has(providerInstanceId)),
        );
      },
    ),
    showsApprovalRequests: Effect.fn("McpProviderSessions.showsApprovalRequests")(
      function (providerInstanceId) {
        return Ref.update(approvalRequestsShownBy, (current) =>
          new Set(current).add(providerInstanceId),
        );
      },
    ),
    areApprovalRequestsShown: Effect.fn("McpProviderSessions.areApprovalRequestsShown")(
      function (providerInstanceId) {
        return Ref.get(approvalRequestsShownBy).pipe(
          Effect.map((current) => current.has(providerInstanceId)),
        );
      },
    ),
    answerApprovalRequests: Effect.fn("McpProviderSessions.answerApprovalRequests")(
      function (threadId, message, approved) {
        return Ref.update(approvalRequests, (current) => {
          const kept = current.get(threadId);
          // A request's first answer stands.
          const waits = (request: KeptApprovalRequest) =>
            request.message === message && request.approved === undefined;
          if (kept === undefined || !kept.some(waits)) return current;
          return new Map(current).set(
            threadId,
            kept.map((request) => (waits(request) ? { ...request, approved } : request)),
          );
        });
      },
    ),
    takeApprovalRequest: Effect.fn("McpProviderSessions.takeApprovalRequest")(takeApprovalRequest),
  });
});

export const layer = Layer.effect(McpProviderSessions, make);
