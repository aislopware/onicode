import * as Schema from "effect/Schema";

import { PortSchema, PositiveInt, TrimmedNonEmptyString } from "./baseSchemas.ts";

// A supervisor must not restart a server refused by the state-directory owner.
export const SERVER_EXIT_CODE_STATE_DIR_OWNED = 78;
// A server the managed policy refused, so the desktop app blames the policy rather than the release.
export const SERVER_EXIT_CODE_MANAGED_POLICY = 77;

export const DesktopBackendBootstrap = Schema.Struct({
  mode: Schema.Literal("desktop"),
  port: PortSchema,
  // Omit in WSL to use the Linux home instead of the mapped Windows directory.
  oniHome: Schema.optional(Schema.String),
  host: Schema.String,
  desktopBootstrapToken: Schema.String,
  // Derives rotating tokens instead of trusting desktopBootstrapToken indefinitely; see @onicode/shared/desktopBootstrapToken.
  desktopBootstrapSecret: Schema.optionalKey(Schema.String),
  // Absent means the backend must hydrate PATH, as WSL and older desktops require.
  shellEnvironmentPrepared: Schema.optionalKey(Schema.Boolean),
  tailscaleServeEnabled: Schema.Boolean,
  tailscaleServePort: PortSchema,
  /** Serve only loopback and tailnet connections on the wildcard bind. */
  tailnetOnly: Schema.optionalKey(Schema.Boolean),
  otlpTracesUrl: Schema.optional(Schema.String),
  otlpMetricsUrl: Schema.optional(Schema.String),
  otlpLogsUrl: Schema.optional(Schema.String),
  desktopTelemetryFd: Schema.optionalKey(PositiveInt),
  desktopTelemetryControlFd: Schema.optionalKey(PositiveInt),
  // Whether the spawning desktop app can install updates when the server requests one.
  desktopAppUpdate: Schema.optionalKey(Schema.Boolean),
  desktopBrowserFd: Schema.optionalKey(PositiveInt),
  desktopBrowserControlFd: Schema.optionalKey(PositiveInt),
  resourceMonitorPath: Schema.optionalKey(TrimmedNonEmptyString),
});

export type DesktopBackendBootstrap = typeof DesktopBackendBootstrap.Type;

/** In `<oniHome>/runtime` while an update restarts the backend; SSH launchers wait on it. */
export const DESKTOP_UPDATE_IN_PROGRESS_MARKER_FILE = "desktop-update-in-progress";
export const DESKTOP_UPDATE_IN_PROGRESS_TTL_MS = 60_000;
