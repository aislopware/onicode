import type { PreparedConnection } from "@onicode/client-runtime/connection";
import { executeAuthenticatedEnvironmentHttpRequest } from "@onicode/client-runtime/state/environmentHttpAuth";
import type { PushDeviceId } from "@onicode/contracts";
import * as Effect from "effect/Effect";
import type { HttpClient } from "effect/http";

import { PushEnvironmentRequestError, type PushEnvironmentClient } from "./pushRegistration";

type PushUrls = Parameters<
  Parameters<
    typeof executeAuthenticatedEnvironmentHttpRequest<"push", unknown, unknown, never>
  >[0]["url"]
>[0];
type PushApi = Parameters<
  Parameters<
    typeof executeAuthenticatedEnvironmentHttpRequest<"push", unknown, unknown, never>
  >[0]["request"]
>[0];

const TIMEOUT_MS = 15_000;

/** Plain HTTP so the app can register from the background, when it holds no socket. */
export const makeEnvironmentPushClient = (
  prepared: PreparedConnection,
): Effect.Effect<PushEnvironmentClient, never, HttpClient.HttpClient> =>
  Effect.map(Effect.context<HttpClient.HttpClient>(), (context) => {
    const request = <A, E>(
      method: "GET" | "PUT" | "POST" | "DELETE",
      url: (urls: PushUrls) => string,
      run: (api: PushApi) => Effect.Effect<A, E>,
    ) =>
      executeAuthenticatedEnvironmentHttpRequest({
        group: "push",
        prepared,
        method,
        url,
        timeoutMs: TIMEOUT_MS,
        requestInit: { redirect: "error" },
        request: run,
      }).pipe(
        Effect.mapError((error) => new PushEnvironmentRequestError({ message: error.message })),
        Effect.provideContext(context),
      );
    const params = (deviceId: string) => ({ deviceId: deviceId as PushDeviceId });
    return {
      registration: request(
        "GET",
        (urls) => urls.registration(),
        ({ client, headers }) => client.registration({ headers }),
      ),
      register: (deviceId, payload) =>
        request(
          "PUT",
          (urls) => urls.registerDevice({ params: params(deviceId) }),
          ({ client, headers }) =>
            client.registerDevice({ headers, params: params(deviceId), payload }),
        ).pipe(Effect.asVoid),
      registerLiveActivity: (deviceId, payload) =>
        request(
          "PUT",
          (urls) => urls.registerLiveActivity({ params: params(deviceId) }),
          ({ client, headers }) =>
            client.registerLiveActivity({ headers, params: params(deviceId), payload }),
        ).pipe(Effect.asVoid),
      unregister: (deviceId) =>
        request(
          "DELETE",
          (urls) => urls.unregisterDevice({ params: params(deviceId) }),
          ({ client, headers }) => client.unregisterDevice({ headers, params: params(deviceId) }),
        ).pipe(Effect.asVoid),
      sendTest: (deviceId) =>
        request(
          "POST",
          (urls) => urls.sendTest({ params: params(deviceId) }),
          ({ client, headers }) => client.sendTest({ headers, params: params(deviceId) }),
        ),
    } satisfies PushEnvironmentClient;
  });
