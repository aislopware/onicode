// @effect-diagnostics-next-line nodeBuiltinImport:off -- The grant digest is one synchronous hash beside a synchronous signature.
import * as NodeCrypto from "node:crypto";

import {
  PushDeliverPayloadJson,
  PushGatewayApi,
  type PushDeliverResponse,
  type PushMessage,
} from "@onicode/contracts/pushGateway";
import * as Clock from "effect/Clock";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";
import * as FetchHttpClient from "effect/http/FetchHttpClient";
import * as HttpClient from "effect/http/HttpClient";
import * as HttpClientRequest from "effect/http/HttpClientRequest";
import * as HttpApiClient from "effect/http-api/HttpApiClient";

import packageJson from "../../package.json" with { type: "json" };
import * as PushSigningKey from "./PushSigningKey.ts";

const REQUEST_TIMEOUT = "15 seconds";

export class PushDeliveryError extends Schema.TaggedError<PushDeliveryError>()(
  "PushDeliveryError",
  {
    /** `grant` means this grant will never work again and should be dropped. */
    reason: Schema.Literals(["grant", "refused", "rate-limited", "provider", "unreachable"]),
    retryable: Schema.Boolean,
    detail: Schema.String,
  },
) {
  override get message(): string {
    return `Push delivery failed (${this.reason}): ${this.detail}`;
  }
}

const isDeliveryError = Schema.is(PushDeliveryError);
const encodePayload = Schema.encodeEffect(PushDeliverPayloadJson);

export class PushGatewayClient extends Context.Service<
  PushGatewayClient,
  {
    readonly deliver: (input: {
      readonly gatewayUrl: string;
      readonly grant: string;
      readonly message: PushMessage;
    }) => Effect.Effect<PushDeliverResponse["outcome"], PushDeliveryError>;
  }
>()("onicode/push/PushGatewayClient") {}

export const layer = Layer.effect(
  PushGatewayClient,
  Effect.gen(function* () {
    const signingKey = yield* PushSigningKey.PushSigningKey;
    // A redirect could carry a signed delivery to a host the allowlist never named.
    const httpClient = HttpClient.transform(yield* HttpClient.HttpClient, (response) =>
      Effect.provideService(response, FetchHttpClient.RequestInit, { redirect: "manual" }),
    ).pipe(
      // Cloudflare's bot checks turn away requests that do not say what they are.
      HttpClient.mapRequest(
        HttpClientRequest.setHeader("user-agent", `onicode-server/${packageJson.version}`),
      ),
    );

    return PushGatewayClient.of({
      deliver: Effect.fn("PushGatewayClient.deliver")(function* (input) {
        const client = yield* HttpApiClient.make(PushGatewayApi, {
          baseUrl: input.gatewayUrl,
        }).pipe(Effect.provideService(HttpClient.HttpClient, httpClient));
        const payload = yield* encodePayload({
          issuedAt: yield* Clock.currentTimeMillis,
          grantDigest: NodeCrypto.createHash("sha256").update(input.grant).digest("base64url"),
          message: input.message,
        }).pipe(
          Effect.mapError(
            (error) =>
              new PushDeliveryError({ reason: "refused", retryable: false, detail: error.message }),
          ),
        );
        return yield* client.deliveries
          .deliver({
            payload: { grant: input.grant, payload, signature: signingKey.sign(payload) },
          })
          .pipe(
            Effect.map((response) => response.outcome),
            Effect.timeout(REQUEST_TIMEOUT),
            Effect.catchTags({
              PushGatewayGrantInvalidError: (error) =>
                Effect.fail(
                  new PushDeliveryError({
                    reason: "grant",
                    retryable: false,
                    detail: error.reason,
                  }),
                ),
              PushGatewayInvalidRequestError: (error) =>
                Effect.fail(
                  new PushDeliveryError({
                    reason: "refused",
                    // A stale payload means the clocks disagree, which a retry cannot fix either.
                    retryable: false,
                    detail: error.reason,
                  }),
                ),
              PushGatewayRateLimitedError: (error) =>
                Effect.fail(
                  new PushDeliveryError({
                    reason: "rate-limited",
                    retryable: true,
                    detail: `retry in ${error.retryAfterSeconds}s`,
                  }),
                ),
              PushGatewayProviderError: (error) =>
                Effect.fail(
                  new PushDeliveryError({
                    reason: "provider",
                    retryable: error.retryable,
                    detail: `${error.provider}: ${error.reason}`,
                  }),
                ),
              PushGatewayUnavailableError: (error) =>
                Effect.fail(
                  new PushDeliveryError({
                    reason: "unreachable",
                    retryable: error.reason === "dependency-unavailable",
                    detail: error.reason,
                  }),
                ),
            }),
            Effect.mapError((error) =>
              isDeliveryError(error)
                ? error
                : new PushDeliveryError({
                    reason: "unreachable",
                    retryable: true,
                    detail: error._tag,
                  }),
            ),
          );
      }),
    });
  }),
);
