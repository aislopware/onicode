import { PushGatewayProviderError } from "@onicode/contracts/pushGateway";
import * as Clock from "effect/Clock";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Redacted from "effect/Redacted";
import * as Schema from "effect/Schema";
import * as HttpClient from "effect/http/HttpClient";
import * as HttpClientRequest from "effect/http/HttpClientRequest";

import type { DeliveryOutcome } from "./Apns.ts";
import { fromBase64, toBase64Url, utf8 } from "./bytes.ts";
import type { AndroidConfig } from "./Config.ts";
import type { FcmRequest } from "./payloads.ts";

const REQUEST_TIMEOUT = "10 seconds";
// Google's access tokens last an hour.
const ACCESS_TOKEN_REUSE_MS = 50 * 60_000;

const decodeAccessToken = Schema.decodeUnknownOption(
  Schema.Struct({ access_token: Schema.NonEmptyString }),
);
const decodeFcmError = Schema.decodeUnknownOption(
  Schema.Struct({
    error: Schema.Struct({
      status: Schema.optional(Schema.String),
      details: Schema.optional(
        Schema.Array(
          Schema.Struct({
            "@type": Schema.optional(Schema.String),
            errorCode: Schema.optional(Schema.String),
          }),
        ),
      ),
    }),
  }),
);

export function fcmOutcome(
  status: number,
  body: unknown,
):
  | { readonly outcome: DeliveryOutcome }
  | { readonly retryable: boolean; readonly reason: string } {
  if (status >= 200 && status < 300) return { outcome: "delivered" };
  const error = Option.getOrUndefined(decodeFcmError(body))?.error;
  const code =
    error?.details?.find(
      (detail) => detail["@type"] === "type.googleapis.com/google.firebase.fcm.v1.FcmError",
    )?.errorCode ?? error?.status;
  if (code === "UNREGISTERED") return { outcome: "unregistered" };
  return {
    retryable: status === 401 || status === 403 || status === 429 || status >= 500,
    reason: (code ?? `HTTP ${status}`).slice(0, 120),
  };
}

async function signAssertion(fcm: AndroidConfig["fcm"], nowSeconds: number): Promise<string> {
  const der = fromBase64(
    Redacted.value(fcm.privateKey)
      .replaceAll("\\n", "\n")
      .replace(/-----[^-]+-----|\s/g, ""),
  );
  if (der === null) throw new Error("The FCM service account key is not a PEM key.");
  const key = await crypto.subtle.importKey(
    "pkcs8",
    der,
    { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const signingInput = `${toBase64Url(utf8(JSON.stringify({ alg: "RS256", typ: "JWT" })))}.${toBase64Url(
    utf8(
      JSON.stringify({
        iss: fcm.clientEmail,
        scope: "https://www.googleapis.com/auth/firebase.messaging",
        aud: "https://oauth2.googleapis.com/token",
        iat: nowSeconds,
        exp: nowSeconds + 3600,
      }),
    ),
  )}`;
  const signature = await crypto.subtle.sign("RSASSA-PKCS1-v1_5", key, utf8(signingInput));
  return `${signingInput}.${toBase64Url(new Uint8Array(signature))}`;
}

export class Fcm extends Context.Service<
  Fcm,
  {
    readonly send: (input: {
      readonly android: AndroidConfig;
      readonly packageName: string;
      readonly token: string;
      readonly request: FcmRequest;
    }) => Effect.Effect<DeliveryOutcome, PushGatewayProviderError>;
  }
>()("@onicode/push-gateway/Fcm") {}

const providerError = (retryable: boolean, reason: string) =>
  new PushGatewayProviderError({ provider: "fcm", retryable, reason });

export const layer = Layer.effect(
  Fcm,
  Effect.gen(function* () {
    const client = yield* HttpClient.HttpClient;
    const accessTokens = new Map<string, { readonly token: string; readonly expiresAt: number }>();

    const accessToken = Effect.fn("Fcm.accessToken")(function* (fcm: AndroidConfig["fcm"]) {
      const now = yield* Clock.currentTimeMillis;
      const cached = accessTokens.get(fcm.clientEmail);
      if (cached !== undefined && cached.expiresAt > now) return cached.token;
      const assertion = yield* Effect.tryPromise({
        try: () => signAssertion(fcm, Math.floor(now / 1000)),
        catch: () => providerError(false, "service account assertion could not be signed"),
      });
      const response = yield* HttpClientRequest.post("https://oauth2.googleapis.com/token").pipe(
        HttpClientRequest.bodyUrlParams({
          grant_type: "urn:ietf:params:oauth:grant-type:jwt-bearer",
          assertion,
        }),
        client.execute,
        Effect.flatMap((result) =>
          Effect.map(result.json, (body) => ({ status: result.status, body })),
        ),
        Effect.timeout(REQUEST_TIMEOUT),
        Effect.mapError(() => providerError(true, "Google did not issue an access token")),
      );
      const token = Option.getOrUndefined(decodeAccessToken(response.body))?.access_token;
      if (response.status !== 200 || token === undefined) {
        yield* Effect.logError("Google refused the FCM service account", {
          status: response.status,
        });
        return yield* providerError(true, "Google refused the service account");
      }
      accessTokens.set(fcm.clientEmail, { token, expiresAt: now + ACCESS_TOKEN_REUSE_MS });
      return token;
    });

    return Fcm.of({
      send: Effect.fn("Fcm.send")(function* (input) {
        const { fcm } = input.android;
        const bearer = yield* accessToken(fcm);
        const response = yield* HttpClientRequest.post(
          `https://fcm.googleapis.com/v1/projects/${encodeURIComponent(fcm.projectId)}/messages:send`,
        ).pipe(
          HttpClientRequest.bearerToken(bearer),
          HttpClientRequest.bodyJson({
            message: {
              token: input.token,
              data: input.request.data,
              android: {
                priority: "HIGH",
                ttl: `${input.request.ttlSeconds}s`,
                restricted_package_name: input.packageName,
                ...(input.request.collapseKey === undefined
                  ? {}
                  : { collapse_key: input.request.collapseKey }),
              },
            },
          }),
          Effect.flatMap(client.execute),
          Effect.flatMap((result) =>
            Effect.map(result.json.pipe(Effect.orElseSucceed((): unknown => null)), (body) => ({
              status: result.status,
              body,
            })),
          ),
          Effect.timeout(REQUEST_TIMEOUT),
          Effect.mapError(() => providerError(true, "FCM did not answer")),
        );
        const result = fcmOutcome(response.status, response.body);
        if ("outcome" in result) return result.outcome;
        if (response.status === 401 || response.status === 403) {
          accessTokens.delete(fcm.clientEmail);
        }
        return yield* providerError(result.retryable, result.reason);
      }),
    });
  }),
);
