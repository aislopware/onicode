import { assert, it } from "@effect/vitest";
import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import type { ManagedPolicyDocument, PublicAccessStatus } from "@onicode/contracts";
import {
  TailscaleFunnel,
  TailscaleFunnelError,
  type TailscaleFunnelFailure,
  type TailscaleFunnelTarget,
} from "@onicode/tailscale";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Queue from "effect/Queue";
import * as Stream from "effect/Stream";
import * as HttpServerResponse from "effect/http/HttpServerResponse";

import * as ServerSecretStore from "../auth/ServerSecretStore.ts";
import * as ServerConfig from "../config.ts";
import * as ManagedPolicy from "../policy/ManagedPolicy.ts";
import * as ServerSettings from "../serverSettings.ts";
import * as PublicAccess from "./PublicAccess.ts";

const ORIGIN = "https://box.tailnet.ts.net:8443";

type FunnelCall = { readonly op: "enable" | "disable"; readonly target: TailscaleFunnelTarget };

const layerFunnel = (calls: Queue.Queue<FunnelCall>, failure?: TailscaleFunnelFailure) =>
  Layer.succeed(TailscaleFunnel)({
    status: () => Effect.die("unused"),
    enable: (target) =>
      Queue.offer(calls, { op: "enable", target }).pipe(
        Effect.andThen(
          failure === undefined
            ? Effect.succeed(ORIGIN)
            : Effect.fail(new TailscaleFunnelError({ reason: failure })),
        ),
      ),
    disable: (target) => Queue.offer(calls, { op: "disable", target }).pipe(Effect.asVoid),
  });

const layerBase = (policy?: ManagedPolicyDocument) =>
  Layer.mergeAll(
    PublicAccess.layer,
    ServerSettings.layerTest({ publicAccessEnabled: true }),
    ManagedPolicy.layerTest(policy),
    ServerSecretStore.layer,
  ).pipe(
    Layer.provideMerge(ServerConfig.layerTest(process.cwd(), { prefix: "oni-public-access-" })),
    Layer.provideMerge(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer)),
  );

/** Runs public access with the setting on and returns the statuses up to the first settled one. */
const startPublicAccess = Effect.fn("PublicAccessTest.start")(function* () {
  const access = yield* PublicAccess.PublicAccess;
  const settled = yield* access.changes.pipe(
    Stream.filter((status) => status._tag === "on" || status._tag === "failed"),
    Stream.runHead,
    Effect.forkScoped,
  );
  const runner = yield* PublicAccess.run(Effect.succeed(HttpServerResponse.text("ok"))).pipe(
    Effect.forkScoped,
  );
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
      Effect.provide(layerFunnel(calls, "funnel-not-allowed")),
    );
    assert.deepStrictEqual(status, { _tag: "failed", reason: "funnel-not-allowed" });
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
        limits: { clients: ["desktop", "local-browser"] },
      }),
    ),
  ),
);
