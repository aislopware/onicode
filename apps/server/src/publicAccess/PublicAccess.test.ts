// @effect-diagnostics nodeBuiltinImport:off - Holds a port with a Node HTTP server.
import * as NodeHttp from "node:http";

import { assert, it } from "@effect/vitest";
import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeHttpServer from "@effect/platform-node/NodeHttpServer";
import * as NodeServices from "@effect/platform-node/NodeServices";
import type {
  ManagedPolicyDocument,
  PublicAccessStatus,
  ServerSettings as ServerSettingsValue,
} from "@onicode/contracts";
import {
  TailscaleFunnel,
  TailscaleFunnelError,
  type TailscaleFunnelFailure,
  type TailscaleFunnelTarget,
} from "@onicode/tailscale";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Queue from "effect/Queue";
import * as Stream from "effect/Stream";
import * as TestClock from "effect/testing/TestClock";
import * as HttpServerRequest from "effect/http/HttpServerRequest";
import * as HttpServerResponse from "effect/http/HttpServerResponse";

import * as ServerSecretStore from "../auth/ServerSecretStore.ts";
import * as ServerConfig from "../config.ts";
import * as ManagedPolicy from "../policy/ManagedPolicy.ts";
import * as ServerSettings from "../serverSettings.ts";
import * as ServerSettingsTestkit from "../serverSettings.testkit.ts";
import * as PublicAccess from "./PublicAccess.ts";

const ORIGIN = "https://box.tailnet.ts.net:8443";

type FunnelCall = { readonly op: "enable" | "disable"; readonly target: TailscaleFunnelTarget };

const layerFunnel = (
  calls: Queue.Queue<FunnelCall>,
  options: {
    readonly failure?: TailscaleFunnelFailure;
    /** How many enables fail with `failure` before they succeed. */
    readonly failures?: number;
    /** How many disables fail before they succeed. */
    readonly disableFailures?: number;
    /** Holds each enable until it completes. */
    readonly enabling?: Deferred.Deferred<void>;
  } = {},
) => {
  let failures = options.failures ?? Number.POSITIVE_INFINITY;
  let disableFailures = options.disableFailures ?? 0;
  return Layer.succeed(TailscaleFunnel)({
    status: () => Effect.die("unused"),
    enable: (target) =>
      Queue.offer(calls, { op: "enable", target }).pipe(
        Effect.andThen(
          options.enabling === undefined ? Effect.void : Deferred.await(options.enabling),
        ),
        Effect.andThen(
          Effect.suspend(() =>
            options.failure !== undefined && failures-- > 0
              ? Effect.fail(new TailscaleFunnelError({ reason: options.failure }))
              : Effect.succeed(ORIGIN),
          ),
        ),
      ),
    disable: (target) =>
      Queue.offer(calls, { op: "disable", target }).pipe(
        Effect.andThen(
          Effect.suspend(() =>
            disableFailures-- > 0
              ? Effect.fail(new TailscaleFunnelError({ reason: "not-running" }))
              : Effect.void,
          ),
        ),
      ),
  });
};

const layerBase = (
  policy?: ManagedPolicyDocument,
  settings: Partial<ServerSettingsValue> = { publicWebhooksEnabled: true },
  settingsLayer = ServerSettingsTestkit.layerLive(settings),
) =>
  PublicAccess.layer.pipe(
    Layer.provideMerge(Layer.mergeAll(ManagedPolicy.layerTest(policy), ServerSecretStore.layer)),
    Layer.provideMerge(settingsLayer),
    Layer.provideMerge(ServerConfig.layerTest(process.cwd(), { prefix: "oni-public-access-" })),
    Layer.provideMerge(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer)),
  );

const statusIs = (access: PublicAccess.PublicAccess["Service"], tag: PublicAccessStatus["_tag"]) =>
  access.changes.pipe(
    Stream.filter((status) => status._tag === tag),
    Stream.runHead,
    Effect.forkScoped,
  );

/** Opens a request to the public listener and reads its first chunk; `HttpClient` hides the reader. */
const openStream = (target: TailscaleFunnelTarget, path = "/api/hooks/hook/token") =>
  Effect.promise(async () => {
    // @effect-diagnostics-next-line globalFetchInEffect:off globalFetch:off
    const response = await fetch(`http://${target.localHost}:${target.localPort}${path}`);
    const reader = response.body!.getReader();
    await reader.read();
    return reader;
  });

/** Runs public access with the setting on and returns the statuses up to the first settled one. */
const startPublicAccess = Effect.fn("PublicAccessTest.start")(function* (
  response = HttpServerResponse.text("ok"),
) {
  const access = yield* PublicAccess.PublicAccess;
  const settled = yield* access.changes.pipe(
    Stream.filter((status) => status._tag === "on" || status._tag === "failed"),
    Stream.runHead,
    Effect.forkScoped,
  );
  const runner = yield* PublicAccess.run(Effect.succeed(response)).pipe(Effect.forkScoped);
  const status = yield* Fiber.join(settled);
  return { status: status._tag === "Some" ? status.value : undefined, runner };
});

it.effect("publishes the public listener through Funnel and withdraws it on shutdown", () =>
  Effect.gen(function* () {
    const calls = yield* Queue.unbounded<FunnelCall>();
    const { status, runner } = yield* startPublicAccess().pipe(Effect.provide(layerFunnel(calls)));
    assert.deepStrictEqual(status, { _tag: "on", origin: ORIGIN } satisfies PublicAccessStatus);
    const enabled = yield* Queue.take(calls);
    assert.strictEqual(enabled.op, "enable");
    assert.strictEqual(enabled.target.localHost, "127.0.0.1");
    assert.strictEqual(enabled.target.funnelPort, 8443);

    yield* Fiber.interrupt(runner);
    assert.deepStrictEqual(yield* Queue.take(calls), { op: "disable", target: enabled.target });
  }).pipe(Effect.scoped, Effect.provide(layerBase())),
);

it.effect("reports why Funnel could not start", () =>
  Effect.gen(function* () {
    const calls = yield* Queue.unbounded<FunnelCall>();
    const { status } = yield* startPublicAccess().pipe(
      Effect.provide(layerFunnel(calls, { failure: "funnel-not-allowed" })),
    );
    assert.deepStrictEqual(status, { _tag: "failed", reason: "funnel-not-allowed" });
  }).pipe(Effect.scoped, Effect.provide(layerBase())),
);

it.effect("retries a failed start on either switch, and never restarts a running one", () =>
  Effect.gen(function* () {
    const access = yield* PublicAccess.PublicAccess;
    const settings = yield* ServerSettings.ServerSettingsService;
    const calls = yield* Queue.unbounded<FunnelCall>();
    const { status } = yield* startPublicAccess().pipe(
      Effect.provide(layerFunnel(calls, { failure: "funnel-not-allowed", failures: 1 })),
    );
    assert.strictEqual(status?._tag, "failed");

    // Webhooks stay on, so the pair changes while "publishes" does not.
    const on = yield* statusIs(access, "on");
    yield* settings.updateSettings({ publicAgentsEnabled: true });
    assert.deepStrictEqual(Option.getOrUndefined(yield* Fiber.join(on)), {
      _tag: "on",
      origin: ORIGIN,
    });

    const off = yield* statusIs(access, "off");
    yield* settings.updateSettings({ publicWebhooksEnabled: false });
    yield* settings.updateSettings({ publicAgentsEnabled: false });
    yield* Fiber.join(off);
    assert.deepStrictEqual(
      (yield* Queue.clear(calls)).map((call) => call.op),
      ["enable", "enable", "disable"],
    );
  }).pipe(Effect.scoped, Effect.provide(layerBase())),
);

it.effect("tries a failed start again after a while, without reporting it as new", () =>
  Effect.gen(function* () {
    const access = yield* PublicAccess.PublicAccess;
    const calls = yield* Queue.unbounded<FunnelCall>();
    const { status } = yield* startPublicAccess().pipe(
      Effect.provide(layerFunnel(calls, { failure: "not-running", failures: 2 })),
    );
    assert.strictEqual(status?._tag, "failed");
    const seen: Array<PublicAccessStatus> = [];
    const on = yield* access.changes.pipe(
      Stream.tap((next) => Effect.sync(() => seen.push(next))),
      Stream.takeUntil((next) => next._tag === "on"),
      Stream.runDrain,
      Effect.forkScoped,
    );
    yield* Queue.take(calls);
    yield* TestClock.adjust("30 seconds");
    yield* Queue.take(calls);
    yield* TestClock.adjust("30 seconds");
    yield* Fiber.join(on);
    assert.deepStrictEqual(
      seen.map((next) => next._tag),
      ["failed", "on"],
    );
  }).pipe(Effect.scoped, Effect.provide(layerBase())),
);

it.effect("switching while Funnel fails leaves one retry, timed from the last change", () =>
  Effect.gen(function* () {
    const access = yield* PublicAccess.PublicAccess;
    const settings = yield* ServerSettings.ServerSettingsService;
    const calls = yield* Queue.unbounded<FunnelCall>();
    yield* startPublicAccess().pipe(Effect.provide(layerFunnel(calls, { failure: "not-running" })));
    const change = Effect.fnUntraced(function* (patch: Partial<PublicAccess.PublicRouteSettings>) {
      yield* TestClock.adjust("10 seconds");
      const failed = yield* access.changes.pipe(
        Stream.drop(1),
        Stream.filter((status) => status._tag === "failed"),
        Stream.runHead,
        Effect.forkScoped({ startImmediately: true }),
      );
      yield* settings.updateSettings(patch);
      yield* Fiber.join(failed);
    });
    yield* change({ publicAgentsEnabled: true });
    const off = yield* statusIs(access, "off");
    yield* settings.updateSettings({ publicWebhooksEnabled: false, publicAgentsEnabled: false });
    yield* Fiber.join(off);
    yield* change({ publicWebhooksEnabled: true });
    yield* Queue.clear(calls);

    yield* TestClock.adjust("29 seconds");
    assert.deepStrictEqual(yield* Queue.clear(calls), []);
    yield* TestClock.adjust("1 second");
    assert.strictEqual((yield* Queue.take(calls)).op, "enable");
    yield* TestClock.adjust("29 seconds");
    assert.deepStrictEqual(yield* Queue.clear(calls), []);
  }).pipe(Effect.scoped, Effect.provide(layerBase())),
);

it.effect("keeps the port Funnel may still point at until that route is off", () =>
  Effect.gen(function* () {
    const access = yield* PublicAccess.PublicAccess;
    const secrets = yield* ServerSecretStore.ServerSecretStore;
    const calls = yield* Queue.unbounded<FunnelCall>();
    // Another process holds the port the previous run published.
    const taken = yield* NodeHttpServer.make(() => NodeHttp.createServer(), {
      host: "127.0.0.1",
      port: 0,
    });
    const port = "port" in taken.address ? taken.address.port : 0;
    yield* secrets.set("public-access-port", new TextEncoder().encode(String(port)));
    const { status } = yield* startPublicAccess().pipe(
      Effect.provide(layerFunnel(calls, { disableFailures: 1 })),
    );
    assert.deepStrictEqual(status, { _tag: "failed", reason: "not-running" });
    const first = yield* Queue.take(calls);
    assert.deepStrictEqual([first.op, first.target.localPort], ["disable", port]);

    const on = yield* statusIs(access, "on");
    yield* TestClock.adjust("30 seconds");
    yield* Fiber.join(on);
    const [disabled, enabled] = yield* Queue.clear(calls);
    assert.deepStrictEqual([disabled?.op, disabled?.target.localPort], ["disable", port]);
    assert.strictEqual(enabled?.op, "enable");
    assert.notStrictEqual(enabled?.target.localPort, port);
  }).pipe(Effect.scoped, Effect.provide(layerBase())),
);

it.effect("turns Funnel off when shutdown interrupts it turning on", () =>
  Effect.gen(function* () {
    const calls = yield* Queue.unbounded<FunnelCall>();
    const enabling = yield* Deferred.make<void>();
    const runner = yield* PublicAccess.run(Effect.succeed(HttpServerResponse.text("ok"))).pipe(
      Effect.provide(layerFunnel(calls, { enabling })),
      Effect.forkScoped,
    );
    const enabled = yield* Queue.take(calls);
    assert.strictEqual(enabled.op, "enable");

    const interrupting = yield* Effect.forkChild(Fiber.interrupt(runner), {
      startImmediately: true,
    });
    yield* Deferred.succeed(enabling, undefined);
    yield* Fiber.join(interrupting);
    assert.deepStrictEqual(yield* Queue.clear(calls), [{ op: "disable", target: enabled.target }]);
  }).pipe(Effect.scoped, Effect.provide(layerBase())),
);

it.effect("turns off at once while a response is still streaming", () =>
  Effect.gen(function* () {
    const calls = yield* Queue.unbounded<FunnelCall>();
    const streaming = Stream.make(new TextEncoder().encode("open")).pipe(
      Stream.concat(Stream.never),
    );
    const { runner } = yield* startPublicAccess(HttpServerResponse.stream(streaming)).pipe(
      Effect.provide(layerFunnel(calls)),
    );
    const { target } = yield* Queue.take(calls);
    const reader = yield* openStream(target);

    // The test clock never reaches the end of a grace period.
    yield* Fiber.interrupt(runner);
    const ended = yield* Effect.promise(() =>
      reader.read().then(
        (chunk) => chunk.done,
        () => true,
      ),
    );
    assert.isTrue(ended);
  }).pipe(Effect.scoped, Effect.provide(layerBase())),
);

it.effect("turning one switch off ends its open responses and no others", () =>
  Effect.gen(function* () {
    const settings = yield* ServerSettings.ServerSettingsService;
    const calls = yield* Queue.unbounded<FunnelCall>();
    const more = yield* Deferred.make<void>();
    const chunk = (text: string) => Stream.make(new TextEncoder().encode(text));
    yield* startPublicAccess(
      HttpServerResponse.stream(
        chunk("open").pipe(
          Stream.concat(Stream.unwrap(Effect.as(Deferred.await(more), chunk("more")))),
          Stream.concat(Stream.never),
        ),
      ),
    ).pipe(Effect.provide(layerFunnel(calls)));
    const { target } = yield* Queue.take(calls);
    const agent = yield* openStream(target, "/mcp");
    const hook = yield* openStream(target);

    yield* settings.updateSettings({ publicAgentsEnabled: false });
    assert.isTrue((yield* Effect.promise(() => agent.read())).done);
    yield* Deferred.succeed(more, undefined);
    assert.isFalse((yield* Effect.promise(() => hook.read())).done);
    yield* Effect.promise(() => hook.cancel());
  }).pipe(
    Effect.scoped,
    Effect.provide(
      layerBase(undefined, { publicWebhooksEnabled: true, publicAgentsEnabled: true }),
    ),
  ),
);

it.effect("keeps a response a setting just allowed, before the change reaches it", () =>
  Effect.gen(function* () {
    const settings = yield* ServerSettings.ServerSettingsService;
    const calls = yield* Queue.unbounded<FunnelCall>();
    const more = yield* Deferred.make<void>();
    const chunk = (text: string) => Stream.make(new TextEncoder().encode(text));
    yield* startPublicAccess(
      HttpServerResponse.stream(
        chunk("open").pipe(
          Stream.concat(Stream.unwrap(Effect.as(Deferred.await(more), chunk("more")))),
          Stream.concat(Stream.never),
        ),
      ),
    ).pipe(Effect.provide(layerFunnel(calls)));
    const { target } = yield* Queue.take(calls);
    yield* settings.updateSettings({ publicAgentsEnabled: true });
    const agent = yield* openStream(target, "/mcp");
    yield* Deferred.succeed(more, undefined);
    assert.isFalse((yield* Effect.promise(() => agent.read())).done);
    yield* Effect.promise(() => agent.cancel());
  }).pipe(
    Effect.scoped,
    Effect.provide(
      layerBase(
        undefined,
        undefined,
        ServerSettingsTestkit.layerLive({ publicWebhooksEnabled: true }, { announce: false }),
      ),
    ),
  ),
);

it.effect("gives webhooks no public origin while the policy turns webhook tasks off", () =>
  Effect.gen(function* () {
    const access = yield* PublicAccess.PublicAccess;
    const calls = yield* Queue.unbounded<FunnelCall>();
    const { status } = yield* startPublicAccess().pipe(Effect.provide(layerFunnel(calls)));
    assert.deepStrictEqual(status, { _tag: "on", origin: ORIGIN } satisfies PublicAccessStatus);
    assert.strictEqual(yield* access.webhookOrigin, null);
  }).pipe(
    Effect.scoped,
    Effect.provide(
      layerBase(
        { schemaVersion: 1, organization: "Example Corp", limits: { webhookTasks: false } },
        { publicWebhooksEnabled: true, publicAgentsEnabled: true },
      ),
    ),
  ),
);

it.effect("reports the webhook origin once per change of Funnel or the webhooks switch", () =>
  Effect.gen(function* () {
    const access = yield* PublicAccess.PublicAccess;
    const settings = yield* ServerSettings.ServerSettingsService;
    const origins = yield* Queue.unbounded<string | null>();
    yield* access.webhookOrigins.pipe(
      Stream.runForEach((origin) => Queue.offer(origins, origin)),
      Effect.forkScoped,
    );
    assert.strictEqual(yield* Queue.take(origins), null);

    const calls = yield* Queue.unbounded<FunnelCall>();
    yield* startPublicAccess().pipe(Effect.provide(layerFunnel(calls)));
    assert.strictEqual(yield* Queue.take(origins), ORIGIN);

    // Agents keep Funnel on: the agents switch leaves the origin alone, the webhooks one clears it.
    yield* settings.updateSettings({ publicAgentsEnabled: true });
    yield* settings.updateSettings({ publicWebhooksEnabled: false });
    assert.strictEqual(yield* Queue.take(origins), null);
    assert.deepStrictEqual(yield* Queue.clear(origins), []);
  }).pipe(Effect.scoped, Effect.provide(layerBase())),
);

it.effect("never turns Funnel on when the policy keeps clients on this machine", () =>
  Effect.gen(function* () {
    const calls = yield* Queue.unbounded<FunnelCall>();
    const { status } = yield* startPublicAccess().pipe(Effect.provide(layerFunnel(calls)));
    assert.deepStrictEqual(status, { _tag: "failed", reason: "policy-denied" });
    assert.strictEqual(yield* Queue.size(calls), 0);
  }).pipe(
    Effect.scoped,
    Effect.provide(
      layerBase({
        schemaVersion: 1,
        organization: "Example Corp",
        limits: { clients: ["desktop", "local"] },
      }),
    ),
  ),
);

it("serves each public route only while its setting is on", () => {
  const all = { webhooks: true, agents: true };
  const cases = [
    ["/api/hooks/task-1/token", { webhooks: true, agents: false }, true],
    ["/api/hooks/task-1/token", { webhooks: false, agents: true }, false],
    ["/mcp", { webhooks: false, agents: true }, true],
    ["/oauth/mcp/authorize", { webhooks: false, agents: true }, true],
    ["/.well-known/oauth-protected-resource/mcp", { webhooks: false, agents: true }, true],
    ["/mcp", { webhooks: true, agents: false }, false],
    ["/mcpx", { webhooks: true, agents: true }, false],
    ["/oauth/token", { webhooks: true, agents: true }, false],
    ["/api/auth/session", { webhooks: true, agents: true }, false],
    ["/ws", { webhooks: true, agents: true }, false],
    ["/api/hooks/task-1/token?ref=main", { webhooks: true, agents: false }, true],
    // The router sees the raw target, so only spellings it reads as the same path pass.
    ["/api/auth/../hooks/task-1/token", all, false],
    ["/api/hooks/../auth/session", all, false],
    ["/api/hooks/%2e%2e/%2e%2e/api/auth/session", all, false],
    ["/api/hooks/task-1/..%2f..%2fauth", all, false],
    ["/api/hooks\\..\\auth/session", all, false],
    ["//x/api/hooks/task-1/token", all, false],
    ["//mcp", all, false],
    ["//[", all, false],
    ["http://public/mcp", all, false],
    ["mcp", all, false],
    ["*", all, false],
  ] as const;
  for (const [path, { webhooks, agents }, allowed] of cases) {
    assert.equal(
      PublicAccess.publicRouteAllowed(path, {
        publicWebhooksEnabled: webhooks,
        publicAgentsEnabled: agents,
      }),
      allowed,
      `${path} webhooks=${webhooks} agents=${agents}`,
    );
  }
});

it("keeps webhooks private when the policy turns webhook tasks off", () => {
  const document: ManagedPolicyDocument = {
    schemaVersion: 1,
    organization: "Example Corp",
    limits: { webhookTasks: false },
  };
  assert.deepStrictEqual(
    PublicAccess.allowedRouteSettings(
      { publicWebhooksEnabled: true, publicAgentsEnabled: true },
      { document },
    ),
    { publicWebhooksEnabled: false, publicAgentsEnabled: true },
  );
});

it("hands the main router a Funnel request as https on the public host, without tailnet identity", () => {
  const request = PublicAccess.asPublicRequest(
    HttpServerRequest.fromWeb(
      new Request("http://127.0.0.1:4000/mcp", {
        headers: {
          host: "127.0.0.1:4000",
          "tailscale-user-login": "owner@example.com",
          "tailscale-user-name": "Owner",
          authorization: "Bearer token",
        },
      }),
    ),
    ORIGIN,
  );
  assert.equal(request.headers.host, new URL(ORIGIN).host);
  assert.equal(request.headers["x-forwarded-proto"], "https");
  assert.equal(request.headers["tailscale-user-login"], undefined);
  assert.equal(request.headers["tailscale-user-name"], undefined);
  assert.equal(request.headers.authorization, "Bearer token");
});
