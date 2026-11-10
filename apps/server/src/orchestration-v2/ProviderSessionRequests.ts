import {
  ProviderSessionRequestError,
  type ProviderContextUsage,
  type ProviderMcpOAuthCallbackInput,
  type ProviderMcpServerActionInput,
  type ProviderMcpServerActionResult,
  type ProviderMcpServerList,
  type ProviderMemoryFile,
  type ProviderMemoryFileInput,
  type ProviderSessionInfo,
  type ProviderSessionInfoInput,
  type ProviderSessionThreadInput,
  type ThreadId,
} from "@onicode/contracts";
import type * as ProviderAdapter from "@onicode/provider-core/server/ProviderAdapter";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";

import * as ProviderSessionManager from "./ProviderSessionManager.ts";
import * as ThreadManagementService from "./ThreadManagementService.ts";

/** Native views over a thread's live provider process. Never starts one: views must not spend turns. */
export class ProviderSessionRequests extends Context.Service<
  ProviderSessionRequests,
  {
    readonly contextUsage: (
      input: ProviderSessionThreadInput,
    ) => Effect.Effect<ProviderContextUsage, ProviderSessionRequestError>;
    readonly mcpServers: (
      input: ProviderSessionThreadInput,
    ) => Effect.Effect<ProviderMcpServerList, ProviderSessionRequestError>;
    readonly mcpServerAction: (
      input: ProviderMcpServerActionInput,
    ) => Effect.Effect<ProviderMcpServerActionResult, ProviderSessionRequestError>;
    readonly mcpOAuthCallback: (
      input: ProviderMcpOAuthCallbackInput,
    ) => Effect.Effect<void, ProviderSessionRequestError>;
    readonly info: (
      input: ProviderSessionInfoInput,
    ) => Effect.Effect<ProviderSessionInfo, ProviderSessionRequestError>;
    readonly memoryFile: (
      input: ProviderMemoryFileInput,
    ) => Effect.Effect<ProviderMemoryFile, ProviderSessionRequestError>;
  }
>()("onicode/orchestration-v2/ProviderSessionRequests") {}

const make = Effect.gen(function* () {
  const threadManagement = yield* ThreadManagementService.ThreadManagementService;
  const providerSessions = yield* ProviderSessionManager.ProviderSessionManagerV2;

  const fail = (
    threadId: ThreadId,
    reason: ProviderSessionRequestError["reason"],
    cause?: unknown,
  ) =>
    new ProviderSessionRequestError({
      threadId,
      reason,
      ...(cause === undefined ? {} : { cause }),
    });

  const adapterFailure = (threadId: ThreadId) => (cause: ProviderAdapter.ProviderAdapterV2Error) =>
    cause._tag === "ProviderAdapterSessionStoppedError"
      ? fail(threadId, "no-session", cause)
      : new ProviderSessionRequestError({
          threadId,
          reason: "request-failed",
          ...("detail" in cause && typeof cause.detail === "string"
            ? { detail: cause.detail }
            : {}),
          cause,
        });

  const resolve = Effect.fn("ProviderSessionRequests.resolve")(function* (threadId: ThreadId) {
    const projection = yield* threadManagement
      .getThreadRecords(threadId, ["providerThreads"])
      .pipe(Effect.mapError((cause) => fail(threadId, "request-failed", cause)));
    const providerThread = projection.providerThreads.find(
      (candidate) => candidate.id === projection.thread.activeProviderThreadId,
    );
    if (providerThread?.providerSessionId == null) return yield* fail(threadId, "no-session");
    const runtime = Option.getOrUndefined(
      yield* providerSessions
        .get(providerThread.providerSessionId)
        .pipe(Effect.mapError((cause) => fail(threadId, "request-failed", cause))),
    );
    if (runtime === undefined) return yield* fail(threadId, "no-session");
    return { providerThread, runtime };
  });

  const resolveMcp = Effect.fn("ProviderSessionRequests.resolveMcp")(function* (
    threadId: ThreadId,
  ) {
    const { providerThread, runtime } = yield* resolve(threadId);
    if (runtime.mcpServers === undefined) return yield* fail(threadId, "provider-unsupported");
    return { providerThread, mcpServers: runtime.mcpServers };
  });

  const resolveSettings = Effect.fn("ProviderSessionRequests.resolveSettings")(function* (
    threadId: ThreadId,
  ) {
    const { providerThread, runtime } = yield* resolve(threadId);
    if (runtime.sessionSettings === undefined) {
      return yield* fail(threadId, "provider-unsupported");
    }
    return { providerThread, settings: runtime.sessionSettings };
  });

  const contextUsage = Effect.fn("ProviderSessionRequests.contextUsage")(function* (
    input: ProviderSessionThreadInput,
  ) {
    const { providerThread, runtime } = yield* resolve(input.threadId);
    if (runtime.contextUsage === undefined) {
      return yield* fail(input.threadId, "provider-unsupported");
    }
    return yield* runtime
      .contextUsage({ providerThread })
      .pipe(Effect.mapError(adapterFailure(input.threadId)));
  });

  const mcpServers = Effect.fn("ProviderSessionRequests.mcpServers")(function* (
    input: ProviderSessionThreadInput,
  ) {
    const { providerThread, mcpServers } = yield* resolveMcp(input.threadId);
    const servers = yield* mcpServers
      .list({ providerThread })
      .pipe(Effect.mapError(adapterFailure(input.threadId)));
    return { servers };
  });

  const mcpServerAction = Effect.fn("ProviderSessionRequests.mcpServerAction")(function* (
    input: ProviderMcpServerActionInput,
  ) {
    const { providerThread, mcpServers } = yield* resolveMcp(input.threadId);
    return yield* mcpServers
      .act({ providerThread, server: input.server, action: input.action })
      .pipe(Effect.mapError(adapterFailure(input.threadId)));
  });

  const mcpOAuthCallback = Effect.fn("ProviderSessionRequests.mcpOAuthCallback")(function* (
    input: ProviderMcpOAuthCallbackInput,
  ) {
    const { providerThread, mcpServers } = yield* resolveMcp(input.threadId);
    yield* mcpServers
      .submitOAuthCallback({
        providerThread,
        server: input.server,
        callbackUrl: input.callbackUrl,
      })
      .pipe(Effect.mapError(adapterFailure(input.threadId)));
  });

  const info = Effect.fn("ProviderSessionRequests.info")(function* (
    input: ProviderSessionInfoInput,
  ) {
    const { providerThread, settings } = yield* resolveSettings(input.threadId);
    return yield* settings
      .info({ providerThread, view: input.view })
      .pipe(Effect.mapError(adapterFailure(input.threadId)));
  });

  const memoryFile = Effect.fn("ProviderSessionRequests.memoryFile")(function* (
    input: ProviderMemoryFileInput,
  ) {
    const { providerThread, settings } = yield* resolveSettings(input.threadId);
    return yield* settings
      .memoryFile({ providerThread, path: input.path })
      .pipe(Effect.mapError(adapterFailure(input.threadId)));
  });

  return ProviderSessionRequests.of({
    contextUsage,
    mcpServers,
    mcpServerAction,
    mcpOAuthCallback,
    info,
    memoryFile,
  });
});

export const layer = Layer.effect(ProviderSessionRequests, make);
