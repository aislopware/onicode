import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import { expect, vi } from "vite-plus/test";
import { it } from "@effect/vitest";
import * as HostProcess from "@onicode/shared/HostProcess";

const native = vi.hoisted(() => ({
  trusted: vi.fn(() => true),
  post: vi.fn(),
  clipboard: { text: "", changes: 0, access: "allowed", wakes: 0 },
  blocker: { start: vi.fn(() => 7), stop: vi.fn() },
  appListeners: new Map<string, () => void>(),
}));
vi.mock("electron", () => ({
  app: {
    on: (event: string, listener: () => void) => native.appListeners.set(event, listener),
    removeListener: (event: string) => native.appListeners.delete(event),
  },
  screen: {
    getPrimaryDisplay: () => ({ id: 5 }),
    getAllDisplays: () => [
      {
        id: 5,
        label: "",
        scaleFactor: 2,
        size: { width: 1000, height: 500 },
        bounds: { x: 0, y: 0, width: 1000, height: 500 },
      },
    ],
  },
  clipboard: {
    readText: () => Promise.resolve(native.clipboard.text),
    writeText: (text: string) => {
      native.clipboard.text = text;
      native.clipboard.changes += 1;
      return Promise.resolve();
    },
  },
  powerSaveBlocker: native.blocker,
  systemPreferences: {
    isTrustedAccessibilityClient: native.trusted,
    getMediaAccessStatus: () => "granted",
  },
}));
vi.mock("../../remoteDesktop/MacHost.ts", () => ({
  loadMacHost: () =>
    Promise.resolve({
      clipboardChangeCount: () => native.clipboard.changes,
      clipboardAccess: () => native.clipboard.access,
      readClipboardDetached: () => Promise.resolve(`${native.clipboard.text} (detached)`),
      wakeDisplay: () => {
        native.clipboard.wakes += 1;
      },
    }),
}));
vi.mock("../../remoteDesktop/MacInputPoster.ts", () => ({
  loadMacInputPoster: () => Promise.resolve(native.post),
}));

import * as ElectronApp from "../../electron/ElectronApp.ts";
import * as ElectronWindow from "../../electron/ElectronWindow.ts";
import * as MacPermissions from "../../permissions/MacPermissions.ts";
import * as DesktopIpc from "../DesktopIpc.ts";
import { installRemoteDesktop } from "./remoteDesktop.ts";

it.effect("posts input and keeps its window only while the main window is hosting", () =>
  Effect.gen(function* () {
    const handlers = new Map<string, DesktopIpc.DesktopIpcHandleListener>();
    const windowListeners = new Map<string, (event: { preventDefault: () => void }) => void>();
    const window = {
      on: (event: string, listener: (event: { preventDefault: () => void }) => void) =>
        windowListeners.set(event, listener),
      once: vi.fn(),
      hide: vi.fn(),
      webContents: { id: 1, setBackgroundThrottling: vi.fn() },
    };
    const main = { sender: { id: 1 } };
    const call = (channel: string, event: DesktopIpc.DesktopIpcInvokeEvent, payload?: unknown) =>
      Effect.promise(() => Promise.resolve(handlers.get(channel)!(event, payload)));
    const press = {
      sessionId: "viewer",
      displayId: "5",
      events: [{ type: "button", button: 0, down: true, x: 0.5, y: 0.5, clicks: 1 }],
    };
    const close = () => {
      const event = { preventDefault: vi.fn() };
      windowListeners.get("close")?.(event);
      return event.preventDefault.mock.calls.length > 0;
    };

    yield* Effect.scoped(
      Effect.gen(function* () {
        yield* installRemoteDesktop();

        yield* call("desktop:remote-desktop-input", main, press);
        expect(native.post).not.toHaveBeenCalled();

        yield* Effect.promise(() =>
          expect(
            handlers.get("desktop:remote-desktop-set-hosting")!({ sender: { id: 2 } }, true),
          ).rejects.toBeDefined(),
        );
        yield* call("desktop:remote-desktop-set-hosting", main, true);
        expect(window.webContents.setBackgroundThrottling).toHaveBeenLastCalledWith(false);

        native.trusted.mockReturnValue(false);
        yield* call("desktop:remote-desktop-input", main, press);
        expect(native.post).not.toHaveBeenCalled();

        native.trusted.mockReturnValue(true);
        yield* call("desktop:remote-desktop-input", main, press);
        expect(native.post).toHaveBeenLastCalledWith(
          expect.objectContaining({ kind: "mouse", x: 500, y: 250 }),
        );

        // Ending the session lets go of the button the viewer still held.
        const posted = native.post.mock.calls.length;
        yield* call("desktop:remote-desktop-end-session", main, "viewer");
        expect(native.post.mock.calls.length).toBeGreaterThan(posted);

        // What was copied before a viewer connected stays here; later copies go out once each.
        const copy = (text: string) => {
          native.clipboard.text = text;
          native.clipboard.changes += 1;
        };
        const readClipboard = call("desktop:remote-desktop-read-clipboard", main);
        copy("before");
        yield* call("desktop:remote-desktop-set-viewers", main, 1);
        expect(native.blocker.start).toHaveBeenCalledWith("prevent-display-sleep");
        expect(native.clipboard.wakes).toBe(1);
        expect(yield* readClipboard).toBeNull();
        copy("copied on the host");
        expect(yield* readClipboard).toBe("copied on the host");
        expect(yield* readClipboard).toBeNull();
        yield* call("desktop:remote-desktop-write-clipboard", main, "from the viewer");
        expect(native.clipboard.text).toBe("from the viewer");
        expect(yield* readClipboard).toBeNull();
        // Before its first paste prompt, macOS is read from outside this process, which the
        // prompt would stop; once it asks every time, a copy is left unread.
        native.clipboard.access = "unasked";
        copy("first");
        expect(yield* readClipboard).toBe("first (detached)");
        native.clipboard.access = "ask";
        copy("secret");
        expect(yield* readClipboard).toBeNull();
        native.trusted.mockReturnValue(false);
        yield* call("desktop:remote-desktop-write-clipboard", main, "refused");
        expect(native.clipboard.text).toBe("secret");
        native.trusted.mockReturnValue(true);
        yield* call("desktop:remote-desktop-set-viewers", main, 0);
        expect(native.blocker.stop).toHaveBeenCalledWith(7);

        expect(close()).toBe(true);
        expect(window.hide).toHaveBeenCalledOnce();
        native.appListeners.get("before-quit")!();
        expect(close()).toBe(false);

        yield* call("desktop:remote-desktop-set-hosting", main, false);
        yield* call("desktop:remote-desktop-input", main, press);
        expect(native.post.mock.calls.length).toBe(posted + 1);
      }),
    ).pipe(
      Effect.provideService(HostProcess.Platform, "darwin"),
      Effect.provide([
        ElectronApp.layer,
        Layer.succeed(ElectronWindow.ElectronWindow, {
          main: Effect.succeedSome(window),
        } as unknown as ElectronWindow.ElectronWindow["Service"]),
        Layer.succeed(MacPermissions.MacPermissions, {
          showHelper: () => Effect.void,
        } as unknown as MacPermissions.MacPermissions["Service"]),
        DesktopIpc.layerTest({
          handle: (channel, handler) => {
            handlers.set(channel, handler);
          },
          removeHandler: (channel) => {
            handlers.delete(channel);
          },
          on: vi.fn(),
          removeAllListeners: vi.fn(),
        }),
      ]),
    );
  }),
);
