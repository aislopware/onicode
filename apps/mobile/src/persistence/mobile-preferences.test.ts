import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import { vi } from "vite-plus/test";

vi.mock("expo-secure-store", () => ({
  getItemAsync: vi.fn(),
  setItemAsync: vi.fn(),
  deleteItemAsync: vi.fn(),
}));

import * as MobileDatabase from "./mobile-database";
import * as MobilePreferences from "./mobile-preferences";
import * as MobileSecureStorage from "./mobile-secure-storage";

function loadWith(options: {
  readonly stored: Option.Option<string> | "unavailable";
  readonly secure?: Readonly<Record<string, string>> | undefined;
  readonly secureReadFails?: boolean | undefined;
  readonly save?: boolean | undefined;
}) {
  let stored = options.stored;
  const unavailable = Effect.fail(
    new MobileDatabase.MobileDatabaseError({ operation: "open", cause: "unavailable" }),
  );
  const database = {
    loadPreferencesJson:
      stored === "unavailable"
        ? unavailable
        : Effect.sync(() =>
            Option.map(stored as Option.Option<string>, (payload) => ({ payload, updatedAt: 1 })),
          ),
    savePreferencesJson: (payload: string) =>
      stored === "unavailable"
        ? unavailable
        : Effect.sync(() => {
            stored = Option.some(payload);
          }),
  } as unknown as MobileDatabase.MobileDatabase["Service"];
  const secure = new Map(Object.entries(options.secure ?? {}));
  const secureStorage = MobileSecureStorage.MobileSecureStorage.of({
    getItem: (key) =>
      options.secureReadFails
        ? Effect.fail(
            new MobileSecureStorage.MobileSecureStorageError({ operation: "read", key, cause: "" }),
          )
        : Effect.sync(() => secure.get(key) ?? null),
    setItem: (key, value) => Effect.sync(() => void secure.set(key, value)),
    removeItem: (key) => Effect.sync(() => void secure.delete(key)),
  });
  return Effect.gen(function* () {
    const store = yield* MobilePreferences.MobilePreferencesStore;
    const loaded = yield* store.load;
    const saved = options.save === false ? undefined : yield* store.savePatch({ baseFontSize: 18 });
    return { loaded, saved, stored };
  }).pipe(
    Effect.provide(
      MobilePreferences.layer.pipe(
        Layer.provide(Layer.succeed(MobileDatabase.MobileDatabase, database)),
        Layer.provide(Layer.succeed(MobileSecureStorage.MobileSecureStorage, secureStorage)),
      ),
    ),
  );
}

describe("MobilePreferencesStore", () => {
  it.effect("keeps error reporting off when stored preferences cannot be read", () =>
    Effect.gen(function* () {
      const fresh = yield* loadWith({ stored: Option.none() });
      expect(fresh.loaded.errorReportingEnabled).toBeUndefined();

      const damaged: ReadonlyArray<Parameters<typeof loadWith>[0]> = [
        { stored: Option.some("{not-json") },
        { stored: Option.some(`{ "errorReportingEnabled": "no" }`) },
        { stored: Option.none(), secure: { "onicode.preferences": "{not-json" } },
        { stored: "unavailable" },
        { stored: Option.some("{}"), secure: { "onicode.preferences.fallback": "{not-json" } },
        { stored: Option.some("{}"), secureReadFails: true },
      ];
      for (const options of damaged) {
        const result = yield* loadWith(options);
        expect(result.loaded.errorReportingEnabled).toBe(false);
        expect(result.saved?.errorReportingEnabled).toBe(false);
      }

      // Loading alone must not overwrite an opt-in while the fallback is briefly unreadable.
      const optedIn = `{ "errorReportingEnabled": true }`;
      const unreadable = yield* loadWith({
        stored: Option.some(optedIn),
        secureReadFails: true,
        save: false,
      });
      expect(unreadable.loaded.errorReportingEnabled).toBe(false);
      expect(unreadable.stored).toEqual(Option.some(optedIn));
    }),
  );
});
