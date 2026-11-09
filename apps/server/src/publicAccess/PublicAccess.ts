// @effect-diagnostics nodeBuiltinImport:off - The public listener is a Node HTTP server.
// Funnel forwards to a second, loopback-only listener serving only the webhook route. The main listener (app, API, /mcp) is never published: Funnel's
// requests arrive from loopback, which the main listener trusts.
import * as NodeHttp from "node:http";

import * as NodeHttpServer from "@effect/platform-node/NodeHttpServer";
import type { PublicAccessFailure, PublicAccessStatus } from "@onicode/contracts";
import {
  DEFAULT_TAILSCALE_FUNNEL_PORT,
  TailscaleFunnel,
  type TailscaleFunnelTarget,
} from "@onicode/tailscale";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import type * as HttpServerResponse from "effect/http/HttpServerResponse";

import * as ServerSecretStore from "../auth/ServerSecretStore.ts";
import * as ManagedLimits from "../policy/managedLimits.ts";
import * as ManagedPolicy from "../policy/ManagedPolicy.ts";
import * as ServerSettings from "../serverSettings.ts";

export class PublicAccess extends Context.Service<
  PublicAccess,
  {
    /** `https://<node>.<tailnet>.ts.net[:port]` while public access is on, else null. */
    readonly origin: Effect.Effect<string | null>;
    readonly changes: Stream.Stream<PublicAccessStatus>;
    readonly set: (status: PublicAccessStatus) => Effect.Effect<void>;
  }
>()("onicode/publicAccess/PublicAccess") {}

export const layer = Layer.effect(
  PublicAccess,
  Effect.gen(function* () {
    const ref = yield* SubscriptionRef.make<PublicAccessStatus>({ _tag: "off" });
    const status = SubscriptionRef.get(ref);
    return PublicAccess.of({
      origin: Effect.map(status, (current) => (current._tag === "on" ? current.origin : null)),
      changes: SubscriptionRef.changes(ref),
      set: Effect.fn("PublicAccess.set")(function* (next: PublicAccessStatus) {
        yield* SubscriptionRef.set(ref, next);
      }),
    });
  }),
);

class PublicAccessUnavailable extends Schema.TaggedError<PublicAccessUnavailable>()(
  "PublicAccessUnavailable",
  { reason: Schema.String, cause: Schema.optional(Schema.Defect()) },
) {}

const LOCAL_HOST = "127.0.0.1";
// The same local port across restarts keeps Funnel's config pointed at us, so a restart never
// leaves a stale Funnel route on another process's port.
const PORT_SECRET = "public-access-port";

const decodePort = (bytes: Uint8Array) => {
  const port = Number(new TextDecoder().decode(bytes));
  return Number.isInteger(port) && port > 0 && port < 65536 ? Option.some(port) : Option.none();
};

type PublicApp<E, R> = Effect.Effect<HttpServerResponse.HttpServerResponse, E, R>;

const listen = (port: number) =>
  NodeHttpServer.make(() => NodeHttp.createServer(), { host: LOCAL_HOST, port });

/** Opens the listener and Funnel in the current scope; closing it turns both off. */
const expose = Effect.fn("PublicAccess.expose")(function* <E, R>(app: PublicApp<E, R>) {
  const policy = yield* ManagedPolicy.ManagedPolicy;
  if (!ManagedLimits.allowsRemoteClients(policy)) {
    return yield* new PublicAccessUnavailable({ reason: "policy-denied" });
  }
  const funnel = yield* TailscaleFunnel;
  const secrets = yield* ServerSecretStore.ServerSecretStore;
  const stored = yield* secrets.get(PORT_SECRET).pipe(
    Effect.map(Option.flatMap(decodePort)),
    Effect.orElseSucceed(() => Option.none<number>()),
  );
  const server = yield* listen(Option.getOrElse(stored, () => 0)).pipe(
    Effect.catch((cause) =>
      Option.isSome(stored)
        ? Effect.logInfo("Public access port is taken; using another", { cause }).pipe(
            Effect.andThen(listen(0)),
          )
        : Effect.fail(cause),
    ),
    Effect.mapError((cause) => new PublicAccessUnavailable({ reason: "unknown", cause })),
  );
  const address = server.address;
  if (!("port" in address)) {
    return yield* new PublicAccessUnavailable({ reason: "unknown" });
  }
  const localPort = address.port;
  const target = (port: number): TailscaleFunnelTarget => ({
    localHost: LOCAL_HOST,
    localPort: port,
    funnelPort: DEFAULT_TAILSCALE_FUNNEL_PORT,
  });
  if (Option.isNone(stored) || stored.value !== localPort) {
    // Funnel may still point at the port a previous run used; only a route that is ours goes.
    if (Option.isSome(stored)) yield* funnel.disable(target(stored.value)).pipe(Effect.ignore);
    yield* secrets
      .set(PORT_SECRET, new TextEncoder().encode(String(localPort)))
      .pipe(
        Effect.catch((cause) =>
          Effect.logWarning("Could not keep the public access port", { cause }),
        ),
      );
  }
  yield* server.serve(app);
  const origin = yield* funnel
    .enable(target(localPort))
    .pipe(
      Effect.mapError(
        (error) => new PublicAccessUnavailable({ reason: error.reason, cause: error }),
      ),
    );
  // Added after the listener, so it runs first: Funnel stops before the listener closes.
  yield* Effect.addFinalizer(() =>
    funnel.disable(target(localPort)).pipe(
      Effect.tap(() => Effect.logInfo("Tailscale Funnel disabled")),
      Effect.catch((cause) => Effect.logWarning("Failed to disable Tailscale Funnel", { cause })),
    ),
  );
  yield* Effect.logInfo("Tailscale Funnel enabled", { origin, localPort });
  return origin;
});

const failureOf = (reason: string): PublicAccessFailure =>
  (
    [
      "not-installed",
      "not-running",
      "not-logged-in",
      "https-disabled",
      "funnel-not-allowed",
      "port-in-use",
      "permission-denied",
      "policy-denied",
    ] as const
  ).find((known) => known === reason) ?? "unknown";

/**
 * Follows the `publicAccessEnabled` server setting for the life of the server: on opens the
 * public listener and Funnel, off closes both. A failure stays reported until the setting
 * changes again.
 */
export const run = Effect.fn("PublicAccess.run")(function* <E, R>(app: PublicApp<E, R>) {
  const access = yield* PublicAccess;
  const settings = yield* ServerSettings.ServerSettingsService;
  let current: Scope.Closeable | undefined;

  const stop = Effect.gen(function* () {
    const scope = current;
    current = undefined;
    if (scope !== undefined) yield* Scope.close(scope, Exit.void);
  });

  const start = Effect.gen(function* () {
    yield* access.set({ _tag: "starting" });
    const scope = yield* Scope.make("sequential");
    current = scope;
    const exit = yield* expose(app).pipe(Scope.provide(scope), Effect.exit);
    if (Exit.isSuccess(exit)) {
      yield* access.set({ _tag: "on", origin: exit.value });
      return;
    }
    yield* stop;
    const error = Exit.findErrorOption(exit);
    const reason = Option.isSome(error) ? failureOf(error.value.reason) : "unknown";
    yield* Effect.logWarning("Public access could not start", { reason, cause: exit.cause });
    yield* access.set({ _tag: "failed", reason });
  });

  yield* Stream.concat(Stream.fromEffect(settings.getSettings), settings.streamChanges).pipe(
    Stream.map((current) => current.publicAccessEnabled),
    Stream.catch(() => Stream.make(false)),
    Stream.changes,
    Stream.runForEach((enabled) =>
      stop.pipe(Effect.andThen(enabled ? start : access.set({ _tag: "off" }))),
    ),
    Effect.ensuring(stop),
  );
});
