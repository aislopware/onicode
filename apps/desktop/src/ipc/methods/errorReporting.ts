import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

import * as DesktopCrashReporting from "../../app/DesktopCrashReporting.ts";
import * as ElectronWindow from "../../electron/ElectronWindow.ts";
import * as DesktopAppSettings from "../../settings/DesktopAppSettings.ts";
import * as IpcChannels from "../channels.ts";
import { makeIpcMethod, makeSyncIpcMethod } from "../DesktopIpc.ts";

export const getErrorReporting = makeSyncIpcMethod({
  channel: IpcChannels.GET_ERROR_REPORTING_CHANNEL,
  result: Schema.NullOr(Schema.Struct({ running: Schema.Boolean, saved: Schema.Boolean })),
  handler: Effect.fn("desktop.ipc.errorReporting.get")(function* () {
    const crashReporting = yield* DesktopCrashReporting.DesktopCrashReporting;
    if (!crashReporting.available) return null;
    const appSettings = yield* DesktopAppSettings.DesktopAppSettings;
    return {
      running: crashReporting.instrumentation !== undefined,
      saved: (yield* appSettings.get).errorReportingEnabled,
    };
  }),
});

export const setErrorReportingEnabled = makeIpcMethod({
  channel: IpcChannels.SET_ERROR_REPORTING_ENABLED_CHANNEL,
  payload: Schema.Boolean,
  result: Schema.Void,
  handler: Effect.fn("desktop.ipc.errorReporting.setEnabled")(function* (enabled) {
    const appSettings = yield* DesktopAppSettings.DesktopAppSettings;
    yield* appSettings.setErrorReportingEnabled(enabled);
  }),
});

class WindowScreenshotError extends Schema.TaggedError<WindowScreenshotError>()(
  "WindowScreenshotError",
  { cause: Schema.Defect() },
) {
  override get message(): string {
    return "Could not capture the app window.";
  }
}

/** A JPEG of the app window for a bug report; the user chooses whether to attach it. */
export const captureWindowScreenshot = makeIpcMethod({
  channel: IpcChannels.CAPTURE_WINDOW_SCREENSHOT_CHANNEL,
  payload: Schema.Void,
  result: Schema.NullOr(Schema.Uint8Array),
  handler: Effect.fn("desktop.ipc.errorReporting.captureWindowScreenshot")(function* (_, event) {
    const crashReporting = yield* DesktopCrashReporting.DesktopCrashReporting;
    const main = yield* (yield* ElectronWindow.ElectronWindow).main;
    if (
      crashReporting.instrumentation === undefined ||
      event === undefined ||
      Option.isNone(main) ||
      main.value.webContents.id !== event.sender.id
    ) {
      return null;
    }
    const contents = main.value.webContents;
    return yield* Effect.tryPromise({
      try: async () => new Uint8Array((await contents.capturePage()).toJPEG(85)),
      catch: (cause) => new WindowScreenshotError({ cause }),
    }).pipe(
      Effect.catch((error) => Effect.logWarning(error.message, { error }).pipe(Effect.as(null))),
    );
  }),
});
