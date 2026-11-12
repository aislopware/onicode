import { p256 } from "@noble/curves/nist.js";
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

import { fromBase64, toBase64Url, utf8 } from "./bytes.ts";
import type { IosConfig } from "./Config.ts";
import type { ApnsRequest } from "./payloads.ts";

export type DeliveryOutcome = "delivered" | "unregistered";

// Tokens last an hour, and APNs answers TooManyProviderTokenUpdates when one changes often.
const TOKEN_REUSE_SECONDS = 45 * 60;
const REQUEST_TIMEOUT = "10 seconds";

/** Reasons that mean this token will never work again for this app. */
const DEAD_TOKEN_REASONS = new Set([
  "BadDeviceToken",
  "DeviceTokenNotForTopic",
  "ExpiredToken",
  "Unregistered",
]);
const RETRYABLE_STATUSES = new Set([429, 500, 503]);

const decodeReason = Schema.decodeUnknownOption(
  Schema.fromJsonString(Schema.Struct({ reason: Schema.optional(Schema.String) })),
);

export function apnsOutcome(
  status: number,
  body: string,
):
  | { readonly outcome: DeliveryOutcome }
  | { readonly retryable: boolean; readonly reason: string } {
  if (status >= 200 && status < 300) return { outcome: "delivered" };
  const reason = Option.getOrUndefined(decodeReason(body))?.reason ?? `HTTP ${status}`;
  if (status === 410 || DEAD_TOKEN_REASONS.has(reason)) return { outcome: "unregistered" };
  return { retryable: RETRYABLE_STATUSES.has(status), reason: reason.slice(0, 120) };
}

async function signingScalar(pem: string): Promise<Uint8Array> {
  const der = fromBase64(pem.replaceAll("\\n", "\n").replace(/-----[^-]+-----|\s/g, ""));
  if (der === null) throw new Error("APNS_PRIVATE_KEY is not a PEM key.");
  const key = await crypto.subtle.importKey(
    "pkcs8",
    der,
    { name: "ECDSA", namedCurve: "P-256" },
    true,
    ["sign"],
  );
  const { d } = await crypto.subtle.exportKey("jwk", key);
  const scalar = d === undefined ? null : fromBase64(d);
  if (scalar === null) throw new Error("APNS_PRIVATE_KEY has no private scalar.");
  return scalar;
}

/**
 * Isolates share Cloudflare's connections to APNs, so each one signing its own token would look
 * like one provider changing tokens constantly. RFC 6979 signatures over a quantized `iat` make
 * every isolate produce the same token for the same 45 minutes.
 */
export function makeProviderTokens(ios: IosConfig) {
  let scalar: Promise<Uint8Array> | undefined;
  let cached: { readonly issuedAt: number; readonly token: string } | undefined;
  return async (nowSeconds: number): Promise<string> => {
    const issuedAt = Math.floor(nowSeconds / TOKEN_REUSE_SECONDS) * TOKEN_REUSE_SECONDS;
    if (cached?.issuedAt === issuedAt) return cached.token;
    scalar ??= signingScalar(Redacted.value(ios.apnsPrivateKey));
    const signingInput = `${toBase64Url(utf8(JSON.stringify({ alg: "ES256", kid: ios.apnsKeyId })))}.${toBase64Url(
      utf8(JSON.stringify({ iss: ios.teamId, iat: issuedAt })),
    )}`;
    const token = `${signingInput}.${toBase64Url(p256.sign(utf8(signingInput), await scalar))}`;
    cached = { issuedAt, token };
    return token;
  };
}

export class Apns extends Context.Service<
  Apns,
  {
    readonly send: (input: {
      readonly ios: IosConfig;
      readonly environment: "development" | "production";
      readonly bundleId: string;
      readonly token: string;
      readonly request: ApnsRequest;
    }) => Effect.Effect<DeliveryOutcome, PushGatewayProviderError>;
  }
>()("@onicode/push-gateway/Apns") {}

const providerError = (retryable: boolean, reason: string) =>
  new PushGatewayProviderError({ provider: "apns", retryable, reason });

export const layer = Layer.effect(
  Apns,
  Effect.gen(function* () {
    const client = yield* HttpClient.HttpClient;
    const tokens = new Map<string, ReturnType<typeof makeProviderTokens>>();

    return Apns.of({
      send: Effect.fn("Apns.send")(function* (input) {
        const nowSeconds = Math.floor((yield* Clock.currentTimeMillis) / 1000);
        const cacheKey = `${input.ios.teamId}/${input.ios.apnsKeyId}`;
        let providerToken = tokens.get(cacheKey);
        if (providerToken === undefined) {
          providerToken = makeProviderTokens(input.ios);
          tokens.set(cacheKey, providerToken);
        }
        const bearer = yield* Effect.tryPromise({
          try: () => providerToken(nowSeconds),
          catch: () => providerError(false, "provider token could not be signed"),
        });
        const host =
          input.environment === "production"
            ? "https://api.push.apple.com"
            : "https://api.sandbox.push.apple.com";
        const { request } = input;
        const response = yield* HttpClientRequest.post(`${host}/3/device/${input.token}`).pipe(
          HttpClientRequest.setHeaders({
            authorization: `bearer ${bearer}`,
            "apns-push-type": request.pushType,
            "apns-priority": request.priority,
            "apns-expiration": String(request.expiration),
            "apns-topic":
              request.pushType === "liveactivity"
                ? `${input.bundleId}.push-type.liveactivity`
                : input.bundleId,
            ...(request.collapseId === undefined ? {} : { "apns-collapse-id": request.collapseId }),
          }),
          HttpClientRequest.bodyJson(request.payload),
          Effect.flatMap(client.execute),
          Effect.flatMap((result) =>
            Effect.map(result.text, (body) => ({ status: result.status, body })),
          ),
          Effect.timeout(REQUEST_TIMEOUT),
          Effect.mapError(() => providerError(true, "APNs did not answer")),
        );
        const result = apnsOutcome(response.status, response.body);
        if ("outcome" in result) return result.outcome;
        if (response.status === 403) {
          yield* Effect.logError("APNs refused the provider token", { reason: result.reason });
        }
        return yield* providerError(result.retryable, result.reason);
      }),
    });
  }),
);
