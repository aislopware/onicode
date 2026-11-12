import {
  AuthAccessReadScope,
  AuthAccessWriteScope,
  AuthOrchestrationReadScope,
  EnvironmentHttpApi,
  type EnvironmentSessionPrincipalShape,
} from "@onicode/contracts";
import * as Effect from "effect/Effect";
import * as HttpApiBuilder from "effect/http-api/HttpApiBuilder";

import {
  annotateEnvironmentRequest,
  failEnvironmentInternal,
  failEnvironmentInvalidRequest,
  failEnvironmentNotFound,
  failEnvironmentScopeRequired,
  requireEnvironmentScope,
} from "../auth/http.ts";
import type { PushDevicesError } from "./PushDevices.ts";
import * as PushNotifications from "./PushNotifications.ts";

type PushError =
  | PushDevicesError
  | PushNotifications.PushGatewayNotAllowedError
  | PushNotifications.PushDeviceNotFoundError
  | PushNotifications.PushDeviceNotOwnedError;

const caller = (session: EnvironmentSessionPrincipalShape): PushNotifications.PushDeviceCaller => ({
  sessionId: session.sessionId,
  seesAllDevices: session.scopes.has(AuthAccessReadScope),
  // Removing or replacing another session's device is removing part of its access.
  managesAllDevices: session.scopes.has(AuthAccessWriteScope),
});

export const layer = HttpApiBuilder.group(
  EnvironmentHttpApi,
  "push",
  Effect.fnUntraced(function* (handlers) {
    const push = yield* PushNotifications.PushNotifications;
    // A registered device learns thread and project titles, which is what reading threads grants.
    const guard = Effect.fnUntraced(function* (endpoint: string) {
      yield* annotateEnvironmentRequest(endpoint);
      return yield* requireEnvironmentScope(AuthOrchestrationReadScope);
    });
    const mapErrors = <A, R>(effect: Effect.Effect<A, PushError, R>) =>
      effect.pipe(
        Effect.catchTags({
          PersistenceSqlError: (error) => failEnvironmentInternal("internal_error", error),
          PersistenceDecodeError: (error) => failEnvironmentInternal("internal_error", error),
          PushGatewayNotAllowedError: () =>
            failEnvironmentInvalidRequest("push_gateway_not_allowed"),
          PushDeviceNotFoundError: () => failEnvironmentNotFound("push_device_not_found"),
          PushDeviceNotOwnedError: () => failEnvironmentScopeRequired(AuthAccessWriteScope),
        }),
      );
    return handlers
      .handle(
        "registration",
        Effect.fn("environment.push.registration")(function* (args) {
          yield* guard(args.endpoint.name);
          return push.registrationInfo;
        }),
      )
      .handle(
        "devices",
        Effect.fn("environment.push.devices")(function* (args) {
          const session = yield* guard(args.endpoint.name);
          return { devices: yield* push.listDevices(caller(session)).pipe(mapErrors) };
        }),
      )
      .handle(
        "registerDevice",
        Effect.fn("environment.push.registerDevice")(function* (args) {
          const session = yield* guard(args.endpoint.name);
          yield* push
            .registerDevice({
              caller: caller(session),
              deviceId: args.params.deviceId,
              request: args.payload,
            })
            .pipe(mapErrors);
        }),
      )
      .handle(
        "registerLiveActivity",
        Effect.fn("environment.push.registerLiveActivity")(function* (args) {
          const session = yield* guard(args.endpoint.name);
          yield* push
            .registerLiveActivity({
              caller: caller(session),
              deviceId: args.params.deviceId,
              grant: args.payload,
            })
            .pipe(mapErrors);
        }),
      )
      .handle(
        "unregisterDevice",
        Effect.fn("environment.push.unregisterDevice")(function* (args) {
          const session = yield* guard(args.endpoint.name);
          yield* push
            .unregisterDevice({ deviceId: args.params.deviceId, caller: caller(session) })
            .pipe(mapErrors);
        }),
      )
      .handle(
        "sendTest",
        Effect.fn("environment.push.sendTest")(function* (args) {
          const session = yield* guard(args.endpoint.name);
          return yield* push
            .sendTest({ deviceId: args.params.deviceId, caller: caller(session) })
            .pipe(mapErrors);
        }),
      );
  }),
);
