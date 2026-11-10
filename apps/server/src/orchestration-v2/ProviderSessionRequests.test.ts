import {
  ProviderDriverKind,
  ProviderSessionId,
  ProviderThreadId,
  ThreadId,
  type OrchestrationV2ProviderThread,
  type ProviderSessionRequestError,
} from "@onicode/contracts";
import { assert, describe, it } from "@effect/vitest";
import * as ProviderAdapter from "@onicode/provider-core/server/ProviderAdapter";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";

import * as ProviderSessionManager from "./ProviderSessionManager.ts";
import * as ProviderSessionRequests from "./ProviderSessionRequests.ts";
import * as ThreadManagementService from "./ThreadManagementService.ts";

const threadId = ThreadId.make("thread-session-views");
const providerThreadId = ProviderThreadId.make("provider-thread-session-views");
const providerSessionId = ProviderSessionId.make("provider-session-views");
const driver = ProviderDriverKind.make("claudeAgent");

function makeLayer(input: {
  readonly session: Partial<ProviderAdapter.ProviderAdapterV2SessionRuntime> | null;
  readonly activeProviderThreadId?: ProviderThreadId | null;
}) {
  return ProviderSessionRequests.layer.pipe(
    Layer.provide(
      Layer.mergeAll(
        Layer.mock(ThreadManagementService.ThreadManagementService)({
          getThreadRecords: () =>
            Effect.succeed({
              thread: {
                activeProviderThreadId:
                  input.activeProviderThreadId === undefined
                    ? providerThreadId
                    : input.activeProviderThreadId,
              },
              providerThreads: [
                { id: providerThreadId, providerSessionId } as OrchestrationV2ProviderThread,
              ],
            } as never),
        }),
        Layer.mock(ProviderSessionManager.ProviderSessionManagerV2)({
          get: () =>
            Effect.succeed(
              input.session === null
                ? Option.none()
                : Option.some(input.session as ProviderAdapter.ProviderAdapterV2SessionRuntime),
            ),
        }),
      ),
    ),
  );
}

const reason = <A, R>(effect: Effect.Effect<A, ProviderSessionRequestError, R>) =>
  effect.pipe(
    Effect.flip,
    Effect.map((error) => error.reason),
  );

const contextUsage = Effect.flatMap(ProviderSessionRequests.ProviderSessionRequests, (requests) =>
  requests.contextUsage({ threadId }),
);

describe("ProviderSessionRequests", () => {
  it.effect("never answers without the thread's live provider session", () =>
    Effect.gen(function* () {
      assert.strictEqual(
        yield* reason(contextUsage).pipe(Effect.provide(makeLayer({ session: null }))),
        "no-session",
      );
      assert.strictEqual(
        yield* reason(contextUsage).pipe(
          Effect.provide(makeLayer({ session: {}, activeProviderThreadId: null })),
        ),
        "no-session",
      );
    }),
  );

  it.effect("names providers that cannot answer, and a session that stopped mid-request", () =>
    Effect.gen(function* () {
      assert.strictEqual(
        yield* reason(contextUsage).pipe(Effect.provide(makeLayer({ session: {} }))),
        "provider-unsupported",
      );
      assert.strictEqual(
        yield* reason(contextUsage).pipe(
          Effect.provide(
            makeLayer({
              session: {
                contextUsage: () =>
                  Effect.fail(new ProviderAdapter.ProviderAdapterSessionStoppedError({ driver })),
              },
            }),
          ),
        ),
        "no-session",
      );
      const failure = yield* Effect.flatMap(
        ProviderSessionRequests.ProviderSessionRequests,
        (requests) => requests.mcpServerAction({ threadId, server: "linear", action: "reconnect" }),
      ).pipe(
        Effect.flip,
        Effect.provide(
          makeLayer({
            session: {
              mcpServers: {
                list: () => Effect.succeed([]),
                act: () =>
                  Effect.fail(
                    new ProviderAdapter.ProviderAdapterProtocolError({
                      driver,
                      detail: "Server not found: linear",
                    }),
                  ),
                submitOAuthCallback: () => Effect.void,
              },
            },
          }),
        ),
      );
      assert.strictEqual(failure.reason, "request-failed");
      assert.strictEqual(failure.message, "Server not found: linear");
    }),
  );
});
