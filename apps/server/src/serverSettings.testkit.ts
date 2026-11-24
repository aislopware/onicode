import type { ServerSettings as ServerSettingsValue } from "@onicode/contracts";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as PubSub from "effect/PubSub";
import * as Stream from "effect/Stream";

import * as ServerSettings from "./serverSettings.ts";

/**
 * Test settings whose updates reach `subscribeChanges`, as the real service's do. With `announce`
 * off they never arrive, as a slow announcement would not have yet.
 */
export const layerLive = (initial: Partial<ServerSettingsValue>, { announce = true } = {}) =>
  Layer.effect(
    ServerSettings.ServerSettingsService,
    Effect.gen(function* () {
      const base = yield* ServerSettings.ServerSettingsService;
      const changes = yield* PubSub.unbounded<ServerSettingsValue>();
      return ServerSettings.ServerSettingsService.of({
        ...base,
        updateSettings: (patch) =>
          base
            .updateSettings(patch)
            .pipe(Effect.tap((next) => (announce ? PubSub.publish(changes, next) : Effect.void))),
        subscribeChanges: PubSub.subscribe(changes).pipe(Effect.map(Stream.fromSubscription)),
      });
    }),
  ).pipe(Layer.provide(ServerSettings.layerTest(initial)));
