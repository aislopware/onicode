import { PushGatewayApi } from "@onicode/contracts/pushGateway";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as HttpEffect from "effect/http/HttpEffect";
import type * as HttpServerRequest from "effect/http/HttpServerRequest";
import * as HttpServerResponse from "effect/http/HttpServerResponse";
import * as HttpApiBuilder from "effect/http-api/HttpApiBuilder";

import * as Gateway from "./Gateway.ts";

// Every body here is a credential or says whether one worked.
const noStore = HttpEffect.appendPreResponseHandler((_request, response) =>
  Effect.succeed(HttpServerResponse.setHeader(response, "cache-control", "no-store")),
);

/** Cloudflare sets this header itself and overwrites one a client sends. */
const clientAddress = (request: HttpServerRequest.HttpServerRequest) =>
  request.headers["cf-connecting-ip"] ?? "unknown";

const layerHealth = HttpApiBuilder.group(PushGatewayApi, "health", (handlers) =>
  handlers.handle("check", () => Effect.succeed({ ok: true as const })),
);

const layerDevices = HttpApiBuilder.group(
  PushGatewayApi,
  "devices",
  Effect.fnUntraced(function* (handlers) {
    const gateway = yield* Gateway.Gateway;
    return handlers
      .handle("challenge", ({ request }) =>
        gateway.challenge(clientAddress(request)).pipe(Effect.tap(() => noStore)),
      )
      .handle("attest", ({ payload, request }) =>
        gateway.attest(payload, clientAddress(request)).pipe(Effect.tap(() => noStore)),
      )
      .handle("issueGrants", ({ payload }) =>
        gateway.issueGrants(payload).pipe(Effect.tap(() => noStore)),
      );
  }),
);

const layerDeliveries = HttpApiBuilder.group(
  PushGatewayApi,
  "deliveries",
  Effect.fnUntraced(function* (handlers) {
    const gateway = yield* Gateway.Gateway;
    return handlers.handle("deliver", ({ payload }) =>
      gateway.deliver(payload).pipe(Effect.tap(() => noStore)),
    );
  }),
);

export const layer = HttpApiBuilder.layer(PushGatewayApi).pipe(
  Layer.provide([layerHealth, layerDevices, layerDeliveries]),
);
