import {
  AuthEnvironmentMaintainScope,
  type ServerAuthDescriptor,
  type ServerAuthPolicy,
} from "@onicode/contracts";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";

import * as ServerConfig from "../config.ts";
import { isRemoteReachableHost } from "./utils.ts";

export class EnvironmentAuthPolicy extends Context.Service<
  EnvironmentAuthPolicy,
  {
    readonly getDescriptor: () => Effect.Effect<ServerAuthDescriptor>;
  }
>()("onicode/auth/EnvironmentAuthPolicy") {}

/** The auth policy a server started with `config` runs under. */
export function resolveServerAuthPolicy(
  config: Pick<ServerConfig.ServerConfig["Service"], "mode" | "host">,
): ServerAuthPolicy {
  if (isRemoteReachableHost(config.host)) return "remote-reachable";
  return config.mode === "desktop" ? "desktop-managed-local" : "loopback";
}

/** @public */
export const make = Effect.gen(function* () {
  const config = yield* ServerConfig.ServerConfig;
  const policy = resolveServerAuthPolicy(config);

  const bootstrapMethods: ServerAuthDescriptor["bootstrapMethods"] =
    policy === "desktop-managed-local"
      ? ["desktop-bootstrap"]
      : config.mode === "desktop" && policy === "remote-reachable"
        ? ["desktop-bootstrap", "one-time-token"]
        : ["one-time-token"];

  const descriptor: ServerAuthDescriptor = {
    policy,
    bootstrapMethods,
    serverUpdateScope: AuthEnvironmentMaintainScope,
  };

  return EnvironmentAuthPolicy.of({
    getDescriptor: Effect.fn("EnvironmentAuthPolicy.getDescriptor")(function () {
      return Effect.succeed(descriptor);
    }),
  });
});

export const layer = Layer.effect(EnvironmentAuthPolicy, make);
