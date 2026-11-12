// @effect-diagnostics-next-line nodeBuiltinImport:off -- Verifies the signature the way a gateway would, outside the code under test.
import * as NodeCrypto from "node:crypto";

import { assert, describe, it } from "@effect/vitest";
import { PushDeliverPayloadJson } from "@onicode/contracts/pushGateway";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as FetchHttpClient from "effect/http/FetchHttpClient";

import * as ServerSecretStore from "../auth/ServerSecretStore.ts";
import * as PushGatewayClient from "./PushGatewayClient.ts";
import * as PushSigningKey from "./PushSigningKey.ts";

const decodeRequest = Schema.decodeUnknownEffect(
  Schema.fromJsonString(
    Schema.Struct({
      grant: Schema.String,
      payload: PushDeliverPayloadJson,
      signature: Schema.String,
    }),
  ),
);
const decodeSignedText = Schema.decodeUnknownEffect(
  Schema.fromJsonString(Schema.Struct({ payload: Schema.String })),
);

const MESSAGE = {
  deviceId: "device-phone-1",
  environmentId: "env-1",
  activity: { state: null },
} as const;

function setup(respond: () => Response) {
  const secrets = new Map<string, Uint8Array>();
  const requests: Array<{ readonly url: string; readonly body: string; readonly agent: string }> =
    [];
  const fetch: typeof globalThis.fetch = Object.assign(
    async (input: Parameters<typeof globalThis.fetch>[0], init?: RequestInit) => {
      requests.push({
        url: String(input),
        body: new TextDecoder().decode(init?.body as Uint8Array),
        agent: new Headers(init?.headers).get("user-agent") ?? "",
      });
      return respond();
    },
    { preconnect: () => {} },
  );
  const layer = PushGatewayClient.layer.pipe(
    Layer.provideMerge(PushSigningKey.layer),
    Layer.provide(
      Layer.mock(ServerSecretStore.ServerSecretStore)({
        get: (name) => Effect.sync(() => Option.fromUndefinedOr(secrets.get(name))),
        create: (name, value) => Effect.sync(() => void secrets.set(name, value)),
      }),
    ),
    Layer.provide(FetchHttpClient.layer),
    Layer.provide(Layer.succeed(FetchHttpClient.Fetch, fetch)),
  );
  return { layer, requests };
}

const deliver = Effect.gen(function* () {
  const client = yield* PushGatewayClient.PushGatewayClient;
  return yield* client.deliver({
    gatewayUrl: "https://push.example.com",
    grant: "sealed-grant",
    message: MESSAGE,
  });
});

describe("PushGatewayClient", () => {
  it.effect("signs each delivery for its grant with the key devices bound their grants to", () => {
    const { layer, requests } = setup(() => Response.json({ outcome: "delivered" }));
    return Effect.gen(function* () {
      const { publicKey } = yield* PushSigningKey.PushSigningKey;
      assert.strictEqual(yield* deliver, "delivered");

      const [request] = requests;
      assert.strictEqual(request?.url, "https://push.example.com/v1/deliveries");
      assert.match(request?.agent ?? "", /^onicode-server\//);
      const body = yield* decodeRequest(request?.body);
      const signed = yield* decodeSignedText(request?.body);
      const verified = NodeCrypto.verify(
        null,
        Buffer.from(signed.payload, "utf8"),
        NodeCrypto.createPublicKey({
          key: { kty: "OKP", crv: "Ed25519", x: publicKey },
          format: "jwk",
        }),
        Buffer.from(body.signature, "base64url"),
      );
      assert.isTrue(verified);
      assert.deepStrictEqual(body.payload, {
        issuedAt: 0,
        grantDigest: NodeCrypto.createHash("sha256").update("sealed-grant").digest("base64url"),
        message: MESSAGE,
      });
    }).pipe(Effect.provide(layer));
  });

  it.effect.each([
    [401, { _tag: "PushGatewayGrantInvalidError", reason: "expired" }, "grant", false],
    [400, { _tag: "PushGatewayInvalidRequestError", reason: "payload-stale" }, "refused", false],
    [429, { _tag: "PushGatewayRateLimitedError", retryAfterSeconds: 60 }, "rate-limited", true],
    [
      502,
      { _tag: "PushGatewayProviderError", provider: "apns", retryable: false, reason: "BadTopic" },
      "provider",
      false,
    ],
    [503, { _tag: "PushGatewayUnavailableError", reason: "disabled" }, "unreachable", false],
    [500, { error: "worker crashed" }, "unreachable", true],
  ] as const)("reads a %s answer of %o as %s", ([status, body, reason, retryable]) => {
    const { layer } = setup(() => Response.json(body, { status }));
    return Effect.gen(function* () {
      const error = yield* Effect.flip(deliver);
      assert.include(error, { reason, retryable });
    }).pipe(Effect.provide(layer));
  });
});
