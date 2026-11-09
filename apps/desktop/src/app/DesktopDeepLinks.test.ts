import * as NodePathLayer from "@effect/platform-node/NodePath";
import * as HostProcess from "@onicode/shared/HostProcess";
import { assert, describe, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import { beforeEach, vi } from "vite-plus/test";

const { requestSingleInstanceLockMock } = vi.hoisted(() => ({
  requestSingleInstanceLockMock: vi.fn(() => true),
}));
import * as Exit from "effect/Exit";
import * as FileSystem from "effect/FileSystem";
import * as ElectronApp from "../electron/ElectronApp.ts";
import * as ElectronWindow from "../electron/ElectronWindow.ts";
import * as DesktopDeepLinks from "./DesktopDeepLinks.ts";
import * as DesktopWebLinks from "./DesktopWebLinks.ts";
import * as DesktopEnvironment from "./DesktopEnvironment.ts";
import * as DesktopPreReadyFileSystem from "./DesktopPreReadyFileSystem.ts";

const ignoreWebLinks = DesktopWebLinks.DesktopWebLinks.of({
  receive: () => Effect.void,
  setRendererReady: () => Effect.void,
});

const layerDesktopDeepLinks = (
  isDevelopment = true,
  events: string[] = [],
  platform: NodeJS.Platform = "darwin",
  fileSystemLayer: Layer.Layer<FileSystem.FileSystem> = FileSystem.layerNoop({
    exists: () => Effect.succeed(false),
  }),
  listeners = new Map<string, (...args: unknown[]) => void>(),
) => {
  const environment = DesktopEnvironment.DesktopEnvironment.of({
    stateDir: "/tmp/state",
    isDevelopment,
    appDataDirectory: "/tmp/app-data",
    platform,
  } as unknown as DesktopEnvironment.DesktopEnvironment["Service"]);

  const electronApp = {
    setPath: (name: string, value: string) =>
      Effect.sync(() => {
        events.push(`setPath:${name}:${value}`);
      }),
    requestSingleInstanceLock: Effect.sync(() => {
      events.push("requestSingleInstanceLock");
      return requestSingleInstanceLockMock();
    }),
    quit: Effect.sync(() => {
      events.push("quit");
    }),
    on: (name: string, listener: (...args: unknown[]) => void) =>
      Effect.acquireRelease(
        Effect.sync(() => {
          events.push(`on:${name}`);
          listeners.set(name, listener);
        }),
        () =>
          Effect.sync(() => {
            listeners.delete(name);
          }),
      ).pipe(Effect.asVoid),
  } as unknown as ElectronApp.ElectronApp["Service"];

  return DesktopDeepLinks.layer.pipe(
    Layer.provide(
      Layer.mergeAll(
        NodePathLayer.layerPosix,
        Layer.succeed(DesktopEnvironment.DesktopEnvironment, environment),
        Layer.succeed(ElectronApp.ElectronApp, electronApp),
        fileSystemLayer,
      ),
    ),
  );
};

describe("DesktopDeepLinks", () => {
  beforeEach(() => {
    requestSingleInstanceLockMock.mockReset().mockReturnValue(true);
  });

  it.each([
    {
      name: "packaged macOS",
      isDevelopment: false,
      platform: "darwin" as const,
      userData: "/tmp/app-data/onicode-v2",
    },
    {
      name: "packaged Linux",
      isDevelopment: false,
      platform: "linux" as const,
      userData: "/tmp/app-data/onicode-v2",
    },
    {
      name: "packaged Windows",
      isDevelopment: false,
      platform: "win32" as const,
      userData: "/tmp/app-data/onicode-v2",
    },
    {
      name: "development",
      isDevelopment: true,
      platform: "win32" as const,
      userData: "/tmp/app-data/onicode-dev",
    },
  ])(
    "registers launch listeners before startup can yield to the event loop ($name)",
    ({ isDevelopment, platform, userData }) => {
      const events: string[] = [];
      // oxlint-disable-next-line onicode/no-manual-effect-runtime-in-tests -- The assertion IS that the layer builds synchronously; it.effect would mask a regression to async.
      Effect.runSync(
        Effect.scoped(
          Layer.build(
            layerDesktopDeepLinks(isDevelopment, events, platform, DesktopPreReadyFileSystem.layer),
          ),
        ),
      );

      assert.deepEqual(events, [
        `setPath:userData:${userData}`,
        "requestSingleInstanceLock",
        "on:second-instance",
        "on:open-url",
      ]);
    },
  );

  it.effect.each(["before configure", "after configure"] as const)(
    "reveals the running window when a second instance launches %s",
    (entry) => {
      const quit = vi.fn();
      const listeners = new Map<string, (...args: unknown[]) => void>();
      const revealed = Promise.withResolvers<unknown>();
      const window = { id: 1 };
      const electronApp = {
        quit: Effect.sync(quit),
        on: (eventName: string, listener: (...args: unknown[]) => void) =>
          Effect.sync(() => {
            listeners.set(eventName, listener);
          }),
      } as unknown as ElectronApp.ElectronApp["Service"];
      const electronWindow = {
        currentMainOrFirst: Effect.succeedSome(window),
        reveal: (value: unknown) => Effect.sync(() => revealed.resolve(value)),
      } as unknown as ElectronWindow.ElectronWindow["Service"];

      return Effect.gen(function* () {
        const deepLinks = yield* DesktopDeepLinks.DesktopDeepLinks;
        if (entry === "before configure") listeners.get("second-instance")?.({}, ["onicode"]);
        yield* deepLinks.configure;

        assert.equal(quit.mock.calls.length, 0);
        if (entry === "after configure") listeners.get("second-instance")!({}, ["onicode"]);
        assert.strictEqual(yield* Effect.promise(() => revealed.promise), window);
      }).pipe(
        Effect.scoped,
        Effect.provide(layerDesktopDeepLinks(true, [], "darwin", undefined, listeners)),
        Effect.provideService(ElectronApp.ElectronApp, electronApp),
        Effect.provideService(ElectronWindow.ElectronWindow, electronWindow),
        Effect.provideService(DesktopWebLinks.DesktopWebLinks, ignoreWebLinks),
      );
    },
  );

  it.effect("delivers each queued startup link once, before later links", () => {
    const listeners = new Map<string, (...args: unknown[]) => void>();
    const received: string[] = [];
    const first = "onicode://new?prompt=first";
    const second = "onicode://threads/env-1/thread-1";
    const later = "onicode://new?prompt=later";
    const launchPair = "onicode://pair?pairingUrl=https%3A%2F%2Fvps.example.com%2F%23token%3Dabc";
    const delivered = Promise.withResolvers<void>();
    const electronApp = {
      on: (name: string, listener: (...args: unknown[]) => void) =>
        Effect.sync(() => {
          listeners.set(name, listener);
        }),
    } as unknown as ElectronApp.ElectronApp["Service"];

    return Effect.gen(function* () {
      const deepLinks = yield* DesktopDeepLinks.DesktopDeepLinks;
      listeners.get("second-instance")?.({}, ["onicode", first]);
      listeners.get("second-instance")?.({}, ["onicode", second]);
      listeners.get("open-url")?.({ preventDefault: () => undefined }, launchPair);
      assert.deepEqual(received, []);

      yield* deepLinks.configure;
      listeners.get("second-instance")!({}, ["onicode", later]);
      yield* Effect.promise(() => delivered.promise);

      assert.deepEqual(received, [launchPair, first, second, later]);
    }).pipe(
      Effect.scoped,
      Effect.provide(layerDesktopDeepLinks(true, [], "darwin", undefined, listeners)),
      Effect.provideService(ElectronApp.ElectronApp, electronApp),
      Effect.provideService(
        ElectronWindow.ElectronWindow,
        {} as ElectronWindow.ElectronWindow["Service"],
      ),
      Effect.provideService(
        DesktopWebLinks.DesktopWebLinks,
        DesktopWebLinks.DesktopWebLinks.of({
          receive: (url) =>
            Effect.sync(() => {
              received.push(url);
              if (url === later) delivered.resolve();
            }),
          setRendererReady: () => Effect.void,
        }),
      ),
    );
  });

  it.effect.each(["darwin", "win32", "linux"] as const)(
    "quits a second launch before initializing the application runtime on %s",
    (platform) =>
      Effect.gen(function* () {
        requestSingleInstanceLockMock.mockReturnValueOnce(false);
        const events: string[] = [];
        const runtime = layerDesktopDeepLinks(false, events, platform).pipe(
          Layer.flatMap(() =>
            Layer.effectDiscard(
              Effect.sync(() => {
                events.push("initializeApplicationRuntime");
              }),
            ),
          ),
        );
        const exit = yield* Effect.exit(Effect.scoped(Layer.build(runtime)));

        assert.isTrue(Exit.hasInterrupts(exit));
        assert.deepEqual(events, [
          "setPath:userData:/tmp/app-data/onicode-v2",
          "requestSingleInstanceLock",
          "quit",
        ]);
      }),
  );
});

it.effect(
  "provider auth return links reveal the running desktop without reloading it and leave other app URLs alone",
  () => {
    const listeners = new Map<string, (...args: unknown[]) => void>();
    const revealed = Promise.withResolvers<void>();
    const loadURL = vi.fn(async (_url: string) => undefined);
    const window = { loadURL };
    const electronApp = {
      on: (name: string, listener: (...args: unknown[]) => void) =>
        Effect.sync(() => {
          listeners.set(name, listener);
        }),
    } as unknown as ElectronApp.ElectronApp["Service"];
    const electronWindow = {
      currentMainOrFirst: Effect.succeedSome(window),
      reveal: () => Effect.sync(() => revealed.resolve()),
    } as unknown as ElectronWindow.ElectronWindow["Service"];
    return Effect.gen(function* () {
      const deepLinks = yield* DesktopDeepLinks.DesktopDeepLinks;
      yield* deepLinks.configure;
      const event = { preventDefault: vi.fn() };
      listeners.get("open-url")!(event, "onicode-dev://app/auth/callback?code=other-code");
      listeners.get("open-url")!(event, "onicode://app/");
      assert.equal(event.preventDefault.mock.calls.length, 0);
      listeners.get("second-instance")!({}, ["onicode", "onicode-dev://app/"]);
      yield* Effect.promise(() => revealed.promise);
      listeners.get("open-url")!(event, "onicode-dev://app/");
      assert.equal(event.preventDefault.mock.calls.length, 1);
      assert.equal(loadURL.mock.calls.length, 0);
    }).pipe(
      Effect.scoped,
      Effect.provide(layerDesktopDeepLinks(true, [], "darwin", undefined, listeners)),
      Effect.provideService(ElectronApp.ElectronApp, electronApp),
      Effect.provideService(ElectronWindow.ElectronWindow, electronWindow),
      Effect.provideService(DesktopWebLinks.DesktopWebLinks, ignoreWebLinks),
    );
  },
);

it.effect("hands web and app links to the renderer and leaves other links alone", () =>
  Effect.gen(function* () {
    const listeners = new Map<string, (...args: unknown[]) => void>();
    const electronApp = {
      whenReady: Effect.void,
      on: (name: string, listener: (...args: unknown[]) => void) =>
        Effect.sync(() => {
          listeners.set(name, listener);
        }),
    } as unknown as ElectronApp.ElectronApp["Service"];
    const received: Array<string> = [];
    const pairLink = "onicode://pair?pairingUrl=https%3A%2F%2Fvps.example.com%2F%23token%3Dabc";
    yield* Effect.gen(function* () {
      const deepLinks = yield* DesktopDeepLinks.DesktopDeepLinks;
      yield* deepLinks.configure;
      const open = (url: string) => {
        const event = { preventDefault: vi.fn() };
        listeners.get("open-url")!(event, url);
        return event.preventDefault.mock.calls.length;
      };
      assert.strictEqual(open("https://example.com/page"), 1);
      assert.strictEqual(open("http://localhost:3000/"), 1);
      assert.strictEqual(open("onicode://new?prompt=hi"), 1);
      assert.strictEqual(open("onicode://threads/env-1/thread-1"), 1);
      assert.strictEqual(open(pairLink), 1);
      assert.strictEqual(open("onicode://pair"), 0);
      assert.strictEqual(open("mailto:hello@example.com"), 0);
      assert.strictEqual(open("onicode://somewhere-else"), 0);
      yield* Effect.yieldNow;
      assert.deepStrictEqual(received, [
        "https://example.com/page",
        "http://localhost:3000/",
        "onicode://new?prompt=hi",
        "onicode://threads/env-1/thread-1",
        pairLink,
      ]);
    }).pipe(
      Effect.provide(layerDesktopDeepLinks(true, [], "darwin", undefined, listeners)),
      Effect.provideService(HostProcess.Arguments, ["onicode"]),
      Effect.provideService(ElectronApp.ElectronApp, electronApp),
      Effect.provideService(
        ElectronWindow.ElectronWindow,
        {} as ElectronWindow.ElectronWindow["Service"],
      ),
      Effect.provideService(
        DesktopWebLinks.DesktopWebLinks,
        DesktopWebLinks.DesktopWebLinks.of({
          receive: (url) => Effect.sync(() => void received.push(url)),
          setRendererReady: () => Effect.void,
        }),
      ),
    );
  }).pipe(Effect.scoped),
);
