import * as Schema from "effect/Schema";

import { AgentComputerPermissionMode } from "./settings.ts";

export const ComputerDriverInstall = Schema.Union([
  /** cua-driver publishes no build for this machine's platform. */
  Schema.Struct({ state: Schema.Literal("unsupported") }),
  Schema.Struct({ state: Schema.Literal("missing") }),
  Schema.Struct({
    state: Schema.Literal("installing"),
    downloadedBytes: Schema.Number,
    totalBytes: Schema.Number,
  }),
  Schema.Struct({ state: Schema.Literal("installed") }),
  Schema.Struct({ state: Schema.Literal("failed"), message: Schema.String }),
]);
export type ComputerDriverInstall = typeof ComputerDriverInstall.Type;

export const ComputerDriverDaemon = Schema.Union([
  Schema.Struct({ state: Schema.Literal("stopped") }),
  Schema.Struct({
    state: Schema.Literal("running"),
    permissionMode: AgentComputerPermissionMode,
    /** macOS only: grants of the app that runs this server. Null elsewhere. */
    permissions: Schema.NullOr(
      Schema.Struct({ accessibility: Schema.Boolean, screenRecording: Schema.Boolean }),
    ),
  }),
  Schema.Struct({ state: Schema.Literal("failed"), message: Schema.String }),
]);
export type ComputerDriverDaemon = typeof ComputerDriverDaemon.Type;

export const ComputerAccessStatus = Schema.Struct({
  driverVersion: Schema.String,
  install: ComputerDriverInstall,
  daemon: ComputerDriverDaemon,
});
export type ComputerAccessStatus = typeof ComputerAccessStatus.Type;
