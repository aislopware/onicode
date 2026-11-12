import { assert, describe, it } from "@effect/vitest";
import * as NodeServices from "@effect/platform-node/NodeServices";
import {
  AuthAccessWriteScope,
  type AuthEnvironmentScope,
  AuthOrchestrationReadScope,
  AuthSessionId,
  EnvironmentAuthenticatedAuth,
  EnvironmentAuthenticatedPrincipal,
  EnvironmentPushHttpApi,
} from "@onicode/contracts";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as HttpRouter from "effect/http/HttpRouter";
import * as HttpServer from "effect/http/HttpServer";
import * as HttpApi from "effect/http-api/HttpApi";
import * as HttpApiBuilder from "effect/http-api/HttpApiBuilder";

import * as PushHttp from "./http.ts";
import * as PushNotifications from "./PushNotifications.ts";

const DEVICE = "device-phone-1";

const serve = Effect.fn(function* (scopes: ReadonlyArray<AuthEnvironmentScope>) {
  const callers: Array<PushNotifications.PushDeviceCaller> = [];
  const { handler, dispose } = HttpRouter.toWebHandler(
    HttpApiBuilder.layer(HttpApi.make("environment").add(EnvironmentPushHttpApi)).pipe(
      Layer.provide(PushHttp.layer),
      Layer.provide(
        Layer.succeed(EnvironmentAuthenticatedAuth, (effect) =>
          effect.pipe(
            Effect.provideService(EnvironmentAuthenticatedPrincipal, {
              sessionId: AuthSessionId.make("session-desktop"),
              subject: "test",
              scopes: new Set(scopes),
            }),
          ),
        ),
      ),
      Layer.provide(
        Layer.mock(PushNotifications.PushNotifications)({
          registrationInfo: { environmentKey: "A".repeat(43), gatewayUrls: [] },
          registerDevice: ({ request }) =>
            Effect.fail(
              new PushNotifications.PushGatewayNotAllowedError({ gatewayUrl: request.gatewayUrl }),
            ),
          registerLiveActivity: ({ deviceId }) =>
            Effect.fail(new PushNotifications.PushDeviceNotFoundError({ deviceId })),
          unregisterDevice: ({ deviceId, caller }) => {
            callers.push(caller);
            return caller.managesAllDevices
              ? Effect.void
              : Effect.fail(new PushNotifications.PushDeviceNotOwnedError({ deviceId }));
          },
        }),
      ),
      Layer.provide(HttpServer.layerServices),
      Layer.provide(NodeServices.layer),
    ),
    { disableLogger: true },
  );
  yield* Effect.addFinalizer(() => Effect.promise(dispose));
  const request = (method: string, path: string, body?: unknown) =>
    Effect.promise(async () => {
      const response = await handler(
        new Request(`http://environment.test${path}`, {
          method,
          ...(body === undefined
            ? {}
            : { headers: { "content-type": "application/json" }, body: JSON.stringify(body) }),
        }),
      );
      const text = await response.text();
      return { status: response.status, reason: /"reason":"([a-z_]+)"/.exec(text)?.[1] };
    });
  return { request, callers };
});

const grant = { grant: "sealed", expiresAt: 1 };
const registration = {
  platform: "ios",
  label: "iPhone",
  gatewayUrl: "https://push.example.com",
  notification: grant,
  liveActivityStart: null,
  preferences: {
    notifyOnApproval: true,
    notifyOnInput: true,
    notifyOnCompletion: true,
    notifyOnFailure: true,
    activity: true,
  },
};

describe("push http", () => {
  it.effect("lets only sessions that can read threads register for pushes about them", () =>
    Effect.gen(function* () {
      const { request } = yield* serve([AuthAccessWriteScope]);
      assert.strictEqual((yield* request("GET", "/api/push/registration")).status, 403);
      assert.strictEqual(
        (yield* request("PUT", `/api/push/devices/${DEVICE}`, registration)).status,
        403,
      );
    }),
  );

  it.effect("answers the service's refusals with a status a client can act on", () =>
    Effect.gen(function* () {
      const { request, callers } = yield* serve([AuthOrchestrationReadScope]);

      assert.deepStrictEqual(yield* request("PUT", `/api/push/devices/${DEVICE}`, registration), {
        status: 400,
        reason: "push_gateway_not_allowed",
      });
      assert.deepStrictEqual(
        yield* request("PUT", `/api/push/devices/${DEVICE}/live-activity`, grant),
        { status: 404, reason: "push_device_not_found" },
      );
      assert.strictEqual((yield* request("PUT", "/api/push/devices/x", registration)).status, 400);
      assert.strictEqual((yield* request("DELETE", `/api/push/devices/${DEVICE}`)).status, 403);
      assert.deepStrictEqual(
        callers.map((caller) => caller.managesAllDevices),
        [false],
      );
    }),
  );

  it.effect("lets a session that manages access remove another session's device", () =>
    Effect.gen(function* () {
      const { request, callers } = yield* serve([AuthOrchestrationReadScope, AuthAccessWriteScope]);
      assert.strictEqual((yield* request("DELETE", `/api/push/devices/${DEVICE}`)).status, 204);
      assert.deepStrictEqual(
        callers.map((caller) => caller.managesAllDevices),
        [true],
      );
    }),
  );
});
