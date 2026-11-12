import { env } from "cloudflare:workers";
import { PUSH_GATEWAY_MAX_BODY_BYTES } from "@onicode/contracts/pushGateway";
import * as Layer from "effect/Layer";
import * as FetchHttpClient from "effect/http/FetchHttpClient";
import * as HttpRouter from "effect/http/HttpRouter";
import * as HttpServer from "effect/http/HttpServer";

import * as Api from "./Api.ts";
import * as Apns from "./Apns.ts";
import * as AndroidRevocations from "./attestation/AndroidRevocations.ts";
import * as Config from "./Config.ts";
import * as Fcm from "./Fcm.ts";
import * as Gateway from "./Gateway.ts";
import * as RateLimiter from "./RateLimiter.ts";

const layerGateway = Gateway.layer.pipe(
  Layer.provide([Apns.layer, Fcm.layer, AndroidRevocations.layer]),
  Layer.provide(FetchHttpClient.layer),
  Layer.provide(
    RateLimiter.layer({
      challenge: env.RL_CHALLENGE,
      attest: env.RL_ATTEST,
      grants: env.RL_GRANTS,
      deliverDevice: env.RL_DELIVER_DEVICE,
      deliverEnvironment: env.RL_DELIVER_ENVIRONMENT,
    }),
  ),
  Layer.provide(Config.layer(env)),
);

const { handler } = HttpRouter.toWebHandler(
  Api.layer.pipe(Layer.provide(layerGateway), Layer.provide(HttpServer.layerServices)),
  // Request lines would put client addresses in the logs; failures are logged where they happen.
  { disableLogger: true },
);

const refuse = (status: number, error: string) =>
  Response.json({ error }, { status, headers: { "cache-control": "no-store" } });

export default {
  async fetch(request: Request): Promise<Response> {
    if (request.method === "POST") {
      // Refuse oversized and unsized bodies before anything reads them.
      const length = Number(request.headers.get("content-length") ?? Number.NaN);
      if (!Number.isFinite(length)) return refuse(411, "length_required");
      if (length > PUSH_GATEWAY_MAX_BODY_BYTES) return refuse(413, "payload_too_large");
    }
    try {
      return await handler(request);
    } catch (error) {
      // Layer construction failed, which means the Worker's settings are wrong.
      // @effect-diagnostics-next-line globalConsole:off -- No Effect runtime exists to log through when its layers fail to build.
      console.error("push gateway could not start", error);
      return refuse(503, "misconfigured");
    }
  },
} satisfies ExportedHandler;
