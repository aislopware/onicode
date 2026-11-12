import {
  NonNegativeInt,
  RemoteDesktopClipboardText,
  RemoteDesktopHostCapabilities,
  RemoteDesktopInputBatch,
  type RemoteDesktopDisplay,
} from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as Electron from "electron";

import * as ElectronApp from "../../electron/ElectronApp.ts";
import * as ElectronWindow from "../../electron/ElectronWindow.ts";
import { claimDisplayMediaRequests } from "../../electron/displayMediaClaims.ts";
import * as MacPermissions from "../../permissions/MacPermissions.ts";
import { loadMacHost } from "../../remoteDesktop/MacHost.ts";
import { loadMacInputPoster } from "../../remoteDesktop/MacInputPoster.ts";
import { makeMacInputTranslator, type MacInputTranslator } from "../../remoteDesktop/macInput.ts";
import * as IpcChannels from "../channels.ts";
import * as DesktopIpc from "../DesktopIpc.ts";

class RemoteDesktopIpcUnauthorizedSenderError extends Schema.TaggedError<RemoteDesktopIpcUnauthorizedSenderError>()(
  "RemoteDesktopIpcUnauthorizedSenderError",
  {},
) {
  override get message(): string {
    return "Remote desktop request was rejected.";
  }
}

const isClipboardText = Schema.is(RemoteDesktopClipboardText);

const displays = (): ReadonlyArray<RemoteDesktopDisplay> => {
  const primary = Electron.screen.getPrimaryDisplay().id;
  return Electron.screen.getAllDisplays().map((display, index) => ({
    id: String(display.id),
    label: display.label.trim() || `Display ${index + 1}`,
    width: Math.max(1, Math.round(display.size.width * display.scaleFactor)),
    height: Math.max(1, Math.round(display.size.height * display.scaleFactor)),
    primary: display.id === primary,
  }));
};

/**
 * The host half of remote desktop that needs the main process: which display the renderer's
 * next capture serves, the OS permissions, and posting viewers' input.
 */
export const installRemoteDesktop = Effect.fn("desktop.ipc.installRemoteDesktop")(function* () {
  const ipc = yield* DesktopIpc.DesktopIpc;
  const app = yield* ElectronApp.ElectronApp;
  const windows = yield* ElectronWindow.ElectronWindow;
  const permissions = yield* MacPermissions.MacPermissions;
  const platform = yield* HostProcess.Platform;
  const mac = platform === "darwin";

  const sessions = new Map<string, MacInputTranslator>();
  // Single use, and the renderer disarms it when its capture request settles either way.
  let armed: {
    readonly source: Electron.DesktopCapturerSource;
    readonly frameTreeNodeId: number;
  } | null = null;
  const claimedSessions = new WeakSet<Electron.Session>();
  let hosting = false;
  let quitting = false;
  let keptWindow: Electron.BrowserWindow | null = null;
  let awake: number | null = null;
  /** The pasteboard's change count at the last look; null until a viewer's first poll sets it. */
  let clipboardSeen: number | null = null;
  const mayControl = () =>
    hosting && mac && Electron.systemPreferences.isTrustedAccessibilityClient(false);

  const mainWindow = Effect.fn("desktop.ipc.remoteDesktop.mainWindow")(function* (
    event: DesktopIpc.DesktopIpcInvokeEvent | undefined,
  ) {
    const main = yield* windows.main;
    if (
      event === undefined ||
      Option.isNone(main) ||
      main.value.webContents.id !== event.sender.id
    ) {
      return yield* new RemoteDesktopIpcUnauthorizedSenderError();
    }
    return main.value;
  });

  const capabilities = Effect.fn("desktop.ipc.remoteDesktop.readCapabilities")(function* () {
    const clipboard = mac ? (yield* Effect.promise(loadMacHost)).clipboardAccess() : null;
    return {
      displays: displays(),
      screenRecording: mac
        ? Electron.systemPreferences.getMediaAccessStatus("screen") === "granted"
        : null,
      inputControl: mac ? Electron.systemPreferences.isTrustedAccessibilityClient(false) : null,
      clipboardRead: clipboard === null ? null : clipboard === "allowed" || clipboard === "unasked",
    } satisfies RemoteDesktopHostCapabilities;
  });

  // While it shares, the renderer is the host: closing its window would cut every viewer off.
  const keepWindow = (window: Electron.BrowserWindow) => {
    if (keptWindow === window) return;
    keptWindow = window;
    window.on("close", (event) => {
      if (!hosting || quitting || !mac) return;
      event.preventDefault();
      window.hide();
    });
    window.once("closed", () => {
      if (keptWindow === window) keptWindow = null;
    });
  };

  yield* app.on("before-quit", () => {
    quitting = true;
  });

  yield* ipc.handle(
    DesktopIpc.makeIpcMethod({
      channel: IpcChannels.REMOTE_DESKTOP_CAPABILITIES_CHANNEL,
      payload: Schema.Void,
      result: RemoteDesktopHostCapabilities,
      handler: Effect.fn("desktop.ipc.remoteDesktop.capabilities")(function* (_input, event) {
        yield* mainWindow(event);
        return yield* capabilities();
      }),
    }),
  );

  yield* ipc.handle(
    DesktopIpc.makeIpcMethod({
      channel: IpcChannels.REMOTE_DESKTOP_REQUEST_PERMISSIONS_CHANNEL,
      payload: Schema.Void,
      result: RemoteDesktopHostCapabilities,
      handler: Effect.fn("desktop.ipc.remoteDesktop.requestPermissions")(function* (_input, event) {
        const window = yield* mainWindow(event);
        if (!mac) return yield* capabilities();
        if (Electron.systemPreferences.getMediaAccessStatus("screen") !== "granted") {
          // macOS lists the app under Screen Recording only once it has tried to capture.
          yield* Effect.promise(() =>
            Electron.desktopCapturer
              .getSources({ types: ["screen"], thumbnailSize: { width: 1, height: 1 } })
              .catch(() => []),
          );
          if (Electron.systemPreferences.getMediaAccessStatus("screen") !== "granted") {
            yield* permissions.showHelper(
              "screen-recording",
              window,
              () => Electron.systemPreferences.getMediaAccessStatus("screen") === "granted",
            );
          }
        }
        Electron.systemPreferences.isTrustedAccessibilityClient(true);
        return yield* capabilities();
      }),
    }),
  );

  yield* ipc.handle(
    DesktopIpc.makeIpcMethod({
      channel: IpcChannels.REMOTE_DESKTOP_SET_HOSTING_CHANNEL,
      payload: Schema.Boolean,
      result: Schema.Void,
      handler: Effect.fn("desktop.ipc.remoteDesktop.setHosting")(function* (active, event) {
        const window = yield* mainWindow(event);
        hosting = active;
        // A hidden host still answers new viewers; throttled timers would stall that.
        window.webContents.setBackgroundThrottling(!active);
        if (active) keepWindow(window);
      }),
    }),
  );

  yield* ipc.handle(
    DesktopIpc.makeIpcMethod({
      channel: IpcChannels.REMOTE_DESKTOP_ARM_CAPTURE_CHANNEL,
      payload: Schema.NullOr(Schema.String),
      result: Schema.Boolean,
      handler: Effect.fn("desktop.ipc.remoteDesktop.armCapture")(function* (displayId, event) {
        const window = yield* mainWindow(event);
        armed = null;
        if (displayId === null) return false;
        const sources = yield* Effect.promise(() =>
          Electron.desktopCapturer
            .getSources({ types: ["screen"], thumbnailSize: { width: 0, height: 0 } })
            .catch(() => []),
        );
        // Some Linux sessions report no display id; their sources come in display order.
        const index = Electron.screen
          .getAllDisplays()
          .findIndex((display) => String(display.id) === displayId);
        const source =
          sources.find((candidate) => candidate.display_id === displayId) ??
          (sources.every((candidate) => candidate.display_id === "") ? sources[index] : undefined);
        if (!source) return false;
        const contents = window.webContents;
        armed = { source, frameTreeNodeId: contents.mainFrame.frameTreeNodeId };
        if (!claimedSessions.has(contents.session)) {
          claimedSessions.add(contents.session);
          claimDisplayMediaRequests(contents.session, (request) => {
            const capture = armed;
            if (!capture || request.frame?.frameTreeNodeId !== capture.frameTreeNodeId) return null;
            armed = null;
            return { video: capture.source };
          });
        }
        return true;
      }),
    }),
  );

  yield* ipc.handle(
    DesktopIpc.makeIpcMethod({
      channel: IpcChannels.REMOTE_DESKTOP_INPUT_CHANNEL,
      payload: RemoteDesktopInputBatch,
      result: Schema.Void,
      handler: Effect.fn("desktop.ipc.remoteDesktop.input")(function* (batch, event) {
        yield* mainWindow(event);
        if (!mayControl()) return;
        const display = Electron.screen
          .getAllDisplays()
          .find((candidate) => String(candidate.id) === batch.displayId);
        if (!display) return;
        const post = yield* Effect.promise(loadMacInputPoster);
        let translator = sessions.get(batch.sessionId);
        if (!translator) {
          translator = makeMacInputTranslator();
          sessions.set(batch.sessionId, translator);
        }
        for (const input of batch.events) {
          for (const native of translator.translate(input, display.bounds)) post(native);
        }
      }),
    }),
  );

  yield* ipc.handle(
    DesktopIpc.makeIpcMethod({
      channel: IpcChannels.REMOTE_DESKTOP_END_SESSION_CHANNEL,
      payload: Schema.String,
      result: Schema.Void,
      handler: Effect.fn("desktop.ipc.remoteDesktop.endSession")(function* (sessionId, event) {
        yield* mainWindow(event);
        const translator = sessions.get(sessionId);
        if (!translator) return;
        sessions.delete(sessionId);
        const post = yield* Effect.promise(loadMacInputPoster);
        for (const native of translator.release()) post(native);
      }),
    }),
  );

  yield* ipc.handle(
    DesktopIpc.makeIpcMethod({
      channel: IpcChannels.REMOTE_DESKTOP_SET_VIEWERS_CHANNEL,
      payload: NonNegativeInt,
      result: Schema.Void,
      handler: Effect.fn("desktop.ipc.remoteDesktop.setViewers")(function* (count, event) {
        yield* mainWindow(event);
        const watched = hosting && count > 0;
        if (!watched) {
          if (awake !== null) Electron.powerSaveBlocker.stop(awake);
          awake = null;
          clipboardSeen = null;
          return;
        }
        if (awake !== null) return;
        // A display that sleeps mid-session leaves the viewer a black picture it cannot wake.
        awake = Electron.powerSaveBlocker.start("prevent-display-sleep");
        if (mac) (yield* Effect.promise(loadMacHost)).wakeDisplay();
      }),
    }),
  );

  yield* ipc.handle(
    DesktopIpc.makeIpcMethod({
      channel: IpcChannels.REMOTE_DESKTOP_READ_CLIPBOARD_CHANNEL,
      payload: Schema.Void,
      result: Schema.NullOr(Schema.String),
      handler: Effect.fn("desktop.ipc.remoteDesktop.readClipboard")(function* (_input, event) {
        yield* mainWindow(event);
        if (!mayControl()) return null;
        const macHost = yield* Effect.promise(loadMacHost);
        const count = macHost.clipboardChangeCount();
        const previous = clipboardSeen;
        clipboardSeen = count;
        // What was copied before anyone connected stays on this machine.
        if (previous === null || previous === count) return null;
        const access = macHost.clipboardAccess();
        // "ask" would prompt on every copy, so that waits for the user to allow it in Settings.
        const text =
          access === "allowed"
            ? yield* Effect.promise(() => Electron.clipboard.readText().catch(() => ""))
            : access === "unasked"
              ? yield* Effect.promise(macHost.readClipboardDetached)
              : "";
        return isClipboardText(text) ? text : null;
      }),
    }),
  );

  yield* ipc.handle(
    DesktopIpc.makeIpcMethod({
      channel: IpcChannels.REMOTE_DESKTOP_WRITE_CLIPBOARD_CHANNEL,
      payload: RemoteDesktopClipboardText,
      result: Schema.Void,
      handler: Effect.fn("desktop.ipc.remoteDesktop.writeClipboard")(function* (text, event) {
        yield* mainWindow(event);
        if (!mayControl()) return;
        yield* Effect.promise(() => Electron.clipboard.writeText(text).catch(() => undefined));
        // Our own write is not news for the viewers.
        if (clipboardSeen !== null) {
          clipboardSeen = (yield* Effect.promise(loadMacHost)).clipboardChangeCount();
        }
      }),
    }),
  );

  yield* ipc.handle(
    DesktopIpc.makeIpcMethod({
      channel: IpcChannels.REMOTE_DESKTOP_CAPTURE_KEYBOARD_CHANNEL,
      payload: Schema.Boolean,
      result: Schema.Void,
      handler: Effect.fn("desktop.ipc.remoteDesktop.captureKeyboard")(function* (capture, event) {
        const window = yield* mainWindow(event);
        viewerCapturesKeyboard.set(window.webContents, capture);
      }),
    }),
  );
});

const viewerCapturesKeyboard = new WeakMap<Electron.WebContents, boolean>();

/** True while a remote desktop viewer in this window wants menu shortcuts such as Cmd+Q itself. */
export const remoteDesktopCapturesKeyboard = (contents: Electron.WebContents) =>
  viewerCapturesKeyboard.get(contents) === true;
