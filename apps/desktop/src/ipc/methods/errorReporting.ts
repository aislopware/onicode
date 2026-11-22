import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";

import * as DesktopCrashReporting from "../../app/DesktopCrashReporting.ts";
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
