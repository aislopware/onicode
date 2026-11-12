import {
  PushGatewayApi,
  PushGatewayAttestationRejectedError,
  PushGatewayCredentialInvalidError,
  type PushAttestDeviceRequest,
} from "@onicode/contracts/pushGateway";
import Constants from "expo-constants";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import * as HttpApiClient from "effect/http-api/HttpApiClient";
import type { HttpClient } from "effect/http";

import {
  PushGatewayRequestError,
  type PushGatewayClient,
  type PushGatewayError,
} from "./pushRegistration";

/** Fixed per app build by ONICODE_PUSH_GATEWAY_URL. Null for a build that names no gateway. */
export function pushGatewayUrl(): string | null {
  const configured: unknown = Constants.expoConfig?.extra?.pushGatewayUrl;
  // Environments list gateways by origin, which has no trailing slash.
  return typeof configured === "string" && configured.length > 0
    ? configured.replace(/\/+$/, "")
    : null;
}

const isAttestationRejected = Schema.is(PushGatewayAttestationRejectedError);
const isCredentialInvalid = Schema.is(PushGatewayCredentialInvalidError);

const toGatewayError = <A, E extends { readonly message: string }, R>(
  request: Effect.Effect<A, E, R>,
): Effect.Effect<A, PushGatewayError, R> =>
  Effect.mapError(request, (error): PushGatewayError =>
    isAttestationRejected(error) || isCredentialInvalid(error)
      ? error
      : new PushGatewayRequestError({ message: error.message }),
  );

export const makePushGatewayClient = (
  baseUrl: string,
): Effect.Effect<PushGatewayClient, never, HttpClient.HttpClient> =>
  HttpApiClient.make(PushGatewayApi, { baseUrl }).pipe(
    Effect.map((client) => ({
      challenge: toGatewayError(client.devices.challenge()),
      attest: (payload: PushAttestDeviceRequest) =>
        toGatewayError(client.devices.attest({ payload })),
      issueGrants: (payload) => toGatewayError(client.devices.issueGrants({ payload })),
    })),
  );
