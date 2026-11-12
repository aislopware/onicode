import { type RemoteDesktopHostRequest, WS_METHODS } from "@onicode/contracts";
import * as Effect from "effect/Effect";
import * as Stream from "effect/Stream";
import type { Atom } from "effect/reactivity";

import type { EnvironmentRegistry } from "../connection/registry.ts";
import { request } from "../rpc/client.ts";
import {
  createEnvironmentRpcCommand,
  createEnvironmentRpcSubscriptionAtomFamily,
} from "./runtime.ts";

export function createRemoteDesktopEnvironmentAtoms<R, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, E>,
  /** Answers one viewer's offer on the host; a rejection's message is what the viewer is told. */
  accept: (request: RemoteDesktopHostRequest) => Promise<string>,
) {
  return {
    state: createEnvironmentRpcSubscriptionAtomFamily(runtime, {
      label: "environment-data:remote-desktop:state",
      tag: WS_METHODS.subscribeRemoteDesktop,
    }),
    connect: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:remote-desktop:connect",
      tag: WS_METHODS.remoteDesktopConnect,
    }),
    /** This client is the environment's host for as long as the atom stays mounted. */
    host: createEnvironmentRpcSubscriptionAtomFamily(runtime, {
      label: "environment-data:remote-desktop:host",
      tag: WS_METHODS.remoteDesktopHost,
      transform: (requests) =>
        requests.pipe(
          Stream.mapEffect(
            (hostRequest) =>
              Effect.tryPromise({
                try: () => accept(hostRequest),
                catch: (cause) =>
                  cause instanceof Error ? cause.message : "The host could not share its screen.",
              }).pipe(
                Effect.match({
                  onSuccess: (answer) => ({ answer }),
                  onFailure: (refused) => ({ refused }),
                }),
                Effect.flatMap((result) =>
                  request(WS_METHODS.remoteDesktopAnswer, {
                    requestId: hostRequest.requestId,
                    result,
                  }),
                ),
                // The viewer times out by itself when an answer cannot be delivered.
                Effect.ignore,
              ),
            // A capture waiting on an OS prompt must not hold up the next viewer.
            { concurrency: "unbounded" },
          ),
        ),
    }),
  };
}
