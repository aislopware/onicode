// @effect-diagnostics nodeBuiltinImport:off - The public listener is a Node HTTP server.
// Funnel forwards to a second, loopback-only listener serving only public routes. The main
// listener is never published: Funnel's requests arrive from loopback, which it trusts.
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
import * as Queue from "effect/Queue";
import * as Schema from "effect/Schema";
import * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import * as Tracer from "effect/Tracer";
import { Headers, HttpBody, HttpServerRequest, HttpServerResponse } from "effect/http";

import * as ServerSecretStore from "../auth/ServerSecretStore.ts";
import { guardHttpResponseWriteErrors, keepIdleConnections } from "../httpResponseErrorGuard.ts";
import * as ManagedLimits from "../policy/managedLimits.ts";
import * as ManagedPolicy from "../policy/ManagedPolicy.ts";
import * as ServerSettings from "../serverSettings.ts";
import { IsPublicRequest } from "./publicRequest.ts";

export class PublicAccess extends Context.Service<
  PublicAccess,
  {
    /** `https://<node>.<tailnet>.ts.net[:port]` while Funnel is on, else null. */
    readonly origin: Effect.Effect<string | null>;
    /** The Funnel origin while public webhooks are on and allowed, else null. */
    readonly webhookOrigin: Effect.Effect<string | null>;
    /** The webhook origin now, then each time it changes. */
    readonly webhookOrigins: Stream.Stream<string | null>;
    readonly changes: Stream.Stream<PublicAccessStatus>;
    readonly set: (status: PublicAccessStatus) => Effect.Effect<void>;
  }
>()("onicode/publicAccess/PublicAccess") {}

export interface PublicRouteSettings {
  readonly publicWebhooksEnabled: boolean;
  readonly publicAgentsEnabled: boolean;
}

const AGENT_PATHS = ["/mcp", "/oauth/mcp/", "/.well-known/oauth-"];

/**
 * The path of a request target, only when the router will see the same path the gate checks:
 * origin-form, with no dot segment, encoded separator or other spelling a URL parser rewrites.
 */
const canonicalPath = (target: string): string | undefined => {
  const end = target.search(/[?#]/);
  const path = end === -1 ? target : target.slice(0, end);
  if (!path.startsWith("/") || path.startsWith("//") || /%(2f|2e|5c)/i.test(path)) {
    return undefined;
  }
  try {
    return new URL(path, "http://public").pathname === path ? path : undefined;
  } catch {
    return undefined;
  }
};

/** Whether the public listener serves the request target `target` under these settings. */
export const publicRouteAllowed = (target: string, settings: PublicRouteSettings): boolean => {
  const path = canonicalPath(target);
  if (path === undefined) return false;
  if (path.startsWith("/api/hooks/")) return settings.publicWebhooksEnabled;
  return (
    settings.publicAgentsEnabled &&
    AGENT_PATHS.some((prefix) => (prefix === "/mcp" ? path === prefix : path.startsWith(prefix)))
  );
};

const publishes = (settings: PublicRouteSettings) =>
  settings.publicWebhooksEnabled || settings.publicAgentsEnabled;

const allOff: PublicRouteSettings = { publicWebhooksEnabled: false, publicAgentsEnabled: false };

const sameRouteSettings = (left: PublicRouteSettings, right: PublicRouteSettings) =>
  left.publicWebhooksEnabled === right.publicWebhooksEnabled &&
  left.publicAgentsEnabled === right.publicAgentsEnabled;

/** The settings as the managed policy lets them apply: no webhook tasks means no public webhooks. */
export const allowedRouteSettings = (
  settings: PublicRouteSettings,
  policy: Pick<ManagedPolicy.ManagedPolicy["Service"], "document">,
): PublicRouteSettings => ({
  publicWebhooksEnabled:
    settings.publicWebhooksEnabled && ManagedPolicy.deniedBy(policy, "webhookTasks") === undefined,
  publicAgentsEnabled: settings.publicAgentsEnabled,
});

export const layer = Layer.effect(
  PublicAccess,
  Effect.gen(function* () {
    const settings = yield* ServerSettings.ServerSettingsService;
    const policy = yield* ManagedPolicy.ManagedPolicy;
    const ref = yield* SubscriptionRef.make<PublicAccessStatus>({ _tag: "off" });
    const status = SubscriptionRef.get(ref);
    const origin = Effect.map(status, (current) => (current._tag === "on" ? current.origin : null));
    const webhookOrigin = Effect.all([origin, settings.getSettings]).pipe(
      Effect.map(([current, values]) =>
        allowedRouteSettings(values, policy).publicWebhooksEnabled ? current : null,
      ),
      Effect.orElseSucceed(() => null),
    );
    return PublicAccess.of({
      origin,
      webhookOrigin,
      webhookOrigins: Stream.unwrap(
        Effect.gen(function* () {
          const settingsChanges = yield* settings.subscribeChanges;
          return Stream.merge(SubscriptionRef.changes(ref), settingsChanges).pipe(
            Stream.mapEffect(() => webhookOrigin),
            Stream.changes,
          );
        }),
      ),
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

// Tailscale may start after this server, or its policy or ports may change under it.
const RETRY_AFTER = "30 seconds";

const LOCAL_HOST = "127.0.0.1";
// The same local port across restarts keeps Funnel's config pointed at us, so a restart never
// leaves a stale Funnel route on another process's port.
const PORT_SECRET = "public-access-port";

const decodePort = (bytes: Uint8Array) => {
  const port = Number(new TextDecoder().decode(bytes));
  return Number.isInteger(port) && port > 0 && port < 65536 ? Option.some(port) : Option.none();
};

type PublicApp<E, R> = Effect.Effect<HttpServerResponse.HttpServerResponse, E, R>;

// Each request starts its own trace rather than joining the span that opened the listener.
// ParentSpan is optional to every reader, so removing it leaves R satisfied.
const withoutParentSpan = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
  Effect.updateContext(
    effect,
    (context: Context.Context<R>) => Context.omit(Tracer.ParentSpan)(context) as Context.Context<R>,
  );

// No grace period: an agent's open event stream would hold off turning it off for 20 s.
const listen = (port: number) =>
  NodeHttpServer.make(
    () => keepIdleConnections(guardHttpResponseWriteErrors(NodeHttp.createServer())),
    { host: LOCAL_HOST, port, gracefulShutdownTimeout: 0 },
  );

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
    // The port is kept until then: another process may hold it, and a new one hides that route.
    if (Option.isSome(stored)) {
      yield* funnel
        .disable(target(stored.value))
        .pipe(
          Effect.mapError(
            (error) => new PublicAccessUnavailable({ reason: error.reason, cause: error }),
          ),
        );
    }
    yield* secrets
      .set(PORT_SECRET, new TextEncoder().encode(String(localPort)))
      .pipe(
        Effect.catch((cause) =>
          Effect.logWarning("Could not keep the public access port", { cause }),
        ),
      );
  }
  yield* withoutParentSpan(server.serve(app));
  // Released after the listener closes in reverse order, so Funnel stops first. Acquisition is
  // uninterruptible: an interrupted start still turns off the Funnel it turned on.
  const origin = yield* Effect.acquireRelease(
    funnel
      .enable(target(localPort))
      .pipe(
        Effect.mapError(
          (error) => new PublicAccessUnavailable({ reason: error.reason, cause: error }),
        ),
      ),
    () =>
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

const notFound = HttpServerResponse.empty({ status: 404 });

/**
 * What the main router sees of a Funnel request: always https on the public host, and without
 * the headers Tailscale Serve uses to name a tailnet user, which a stranger could send.
 */
export const asPublicRequest = (request: HttpServerRequest.HttpServerRequest, origin: string) => {
  const headers: Record<string, string> = {};
  for (const [name, value] of Object.entries(request.headers)) {
    if (!name.startsWith("tailscale-")) headers[name] = value;
  }
  headers.host = new URL(origin).host;
  headers["x-forwarded-proto"] = "https";
  return request.modify({ headers: Headers.fromInput(headers), protocol: "https" });
};

/**
 * Follows the public webhooks and public agents settings for the life of the server. Each request
 * is checked against the current settings, so turning one off ends its routes and streams at once.
 * A failed start tries again when either changes, and every 30 seconds unless policy denied it.
 */
export const run = Effect.fn("PublicAccess.run")(function* <E, R>(app: PublicApp<E, R>) {
  const access = yield* PublicAccess;
  const settings = yield* ServerSettings.ServerSettingsService;
  const policy = yield* ManagedPolicy.ManagedPolicy;
  const allowedSettings = settings.getSettings.pipe(
    Effect.map((values) => allowedRouteSettings(values, policy)),
  );
  // Says when to look again; the settings themselves are read, as a change can reach them first.
  const followed = yield* SubscriptionRef.make(allOff);
  const gated = Effect.gen(function* () {
    const request = yield* HttpServerRequest.HttpServerRequest;
    const values = yield* allowedSettings.pipe(Effect.orElseSucceed(() => undefined));
    const origin = yield* access.origin;
    if (values === undefined || origin === null || !publicRouteAllowed(request.url, values)) {
      return notFound;
    }
    const response = yield* app.pipe(
      Effect.provideService(HttpServerRequest.HttpServerRequest, asPublicRequest(request, origin)),
      Effect.provideService(IsPublicRequest, true),
    );
    const body = response.body;
    if (body._tag !== "Stream") return response;
    const turnedOff = SubscriptionRef.changes(followed).pipe(
      Stream.mapEffect(() => allowedSettings.pipe(Effect.orElseSucceed(() => allOff))),
      Stream.filter((now) => !publicRouteAllowed(request.url, now)),
      Stream.runHead,
    );
    return HttpServerResponse.setBody(
      response,
      HttpBody.stream(
        Stream.interruptWhen(body.stream, turnedOff),
        body.contentType,
        body.contentLength,
      ),
    );
  });
  let current: Scope.Closeable | undefined;
  let failed: PublicAccessFailure | undefined;
  // Counts starts and settings changes, so a retry set before the last one does nothing.
  let attempt = 0;
  const retries = yield* Queue.sliding<number>(1);

  const stop = Effect.gen(function* () {
    const scope = current;
    current = undefined;
    if (scope !== undefined) yield* Scope.close(scope, Exit.void);
  });

  // A retry keeps showing the failure rather than flashing "starting".
  const start = Effect.fnUntraced(function* (retry: boolean) {
    if (!retry) yield* access.set({ _tag: "starting" });
    const scope = yield* Scope.make("sequential");
    current = scope;
    const exit = yield* expose(gated).pipe(Scope.provide(scope), Effect.exit);
    if (Exit.isSuccess(exit)) {
      failed = undefined;
      yield* access.set({ _tag: "on", origin: exit.value });
      return;
    }
    yield* stop;
    const error = Exit.findErrorOption(exit);
    const reason = Option.isSome(error) ? failureOf(error.value.reason) : "unknown";
    // The managed policy is read once, so trying again would end the same way.
    if (reason !== "policy-denied") {
      yield* Queue.offer(retries, attempt).pipe(Effect.delay(RETRY_AFTER), Effect.forkScoped);
    }
    if (retry && reason === failed) return;
    failed = reason;
    yield* Effect.logWarning("Public access could not start", { reason, cause: exit.cause });
    yield* access.set({ _tag: "failed", reason });
  });

  yield* Effect.gen(function* () {
    // Subscribed before the first read and before a slow start, so no change goes missing.
    const changes = yield* settings.subscribeChanges;
    yield* Stream.concat(Stream.fromEffect(settings.getSettings), changes).pipe(
      Stream.map((values) => allowedRouteSettings(values, policy)),
      Stream.catch(() => Stream.make(allOff)),
      Stream.changesWith(sameRouteSettings),
      Stream.tap((values) => SubscriptionRef.set(followed, values)),
      Stream.map((): number | undefined => undefined),
      Stream.merge(Stream.fromQueue(retries), { haltStrategy: "left" }),
      // While published, the gate follows switching routes; only a stopped or failed listener
      // starts again.
      Stream.runForEach(
        Effect.fnUntraced(function* (retryOf) {
          const retry = retryOf !== undefined;
          if (retry && retryOf !== attempt) return;
          attempt += 1;
          if (!publishes(yield* SubscriptionRef.get(followed))) {
            if (retry) return;
            yield* stop;
            yield* access.set({ _tag: "off" });
          } else if (current === undefined) {
            yield* start(retry);
          }
        }),
      ),
    );
  }).pipe(Effect.scoped, Effect.ensuring(stop));
});
