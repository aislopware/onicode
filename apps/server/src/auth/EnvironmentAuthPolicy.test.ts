import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import { expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";

import * as ServerConfig from "../config.ts";
import * as EnvironmentAuthPolicy from "./EnvironmentAuthPolicy.ts";

const layerEnvironmentAuthPolicy = (overrides?: Partial<ServerConfig.ServerConfig["Service"]>) =>
  EnvironmentAuthPolicy.layer.pipe(
    Layer.provide(
      Layer.effect(
        ServerConfig.ServerConfig,
        Effect.gen(function* () {
          const config = yield* ServerConfig.ServerConfig;
          return {
            ...config,
            ...overrides,
          } satisfies ServerConfig.ServerConfig["Service"];
        }),
      ).pipe(
        Layer.provide(ServerConfig.layerTest(process.cwd(), { prefix: "oni-auth-policy-test-" })),
      ),
    ),
  );

it.layer(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))(
  "EnvironmentAuthPolicy.layer",
  (it) => {
    it.effect("uses remote-reachable policy for desktop mode when bound beyond loopback", () =>
      Effect.gen(function* () {
        const policy = yield* EnvironmentAuthPolicy.EnvironmentAuthPolicy;
        const descriptor = yield* policy.getDescriptor();

        expect(descriptor.policy).toBe("remote-reachable");
        expect(descriptor.bootstrapMethods).toEqual(["desktop-bootstrap", "one-time-token"]);
      }).pipe(
        Effect.provide(
          layerEnvironmentAuthPolicy({
            mode: "desktop",
            host: "0.0.0.0",
          }),
        ),
      ),
    );

    it.effect("uses the loopback policy for loopback web hosts", () =>
      Effect.gen(function* () {
        const policy = yield* EnvironmentAuthPolicy.EnvironmentAuthPolicy;
        const descriptor = yield* policy.getDescriptor();

        expect(descriptor.policy).toBe("loopback");
        expect(descriptor.bootstrapMethods).toEqual(["one-time-token"]);
        expect(descriptor.serverUpdateScope).toBe("environment:maintain");
      }).pipe(
        Effect.provide(
          layerEnvironmentAuthPolicy({
            mode: "standalone",
            host: "127.0.0.1",
            port: 3773,
          }),
        ),
      ),
    );
  },
);
