import type { SentryEffectInstrumentation } from "@onicode/shared/sentry";
import * as Config from "effect/Config";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";

import * as ElectronProtocol from "../electron/ElectronProtocol.ts";
import { parseErrorReportingConsent } from "../settings/DesktopAppSettings.ts";
import * as DesktopEnvironment from "./DesktopEnvironment.ts";

// Release builds name their Sentry project here. The repository names none, so forks send nothing.
declare const __ONICODE_SENTRY_DESKTOP_DSN__: string | undefined;
const builtInDsn =
  typeof __ONICODE_SENTRY_DESKTOP_DSN__ === "undefined" ? "" : __ONICODE_SENTRY_DESKTOP_DSN__;

interface DesktopCrashReportingState {
  /** False when this build has no Sentry project, or in development without ONICODE_SENTRY_DEV=1. */
  readonly available: boolean;
  /** Undefined when Sentry is not running in this process. */
  readonly instrumentation: SentryEffectInstrumentation | undefined;
}

const reportsInDevelopment = Config.Boolean("ONICODE_SENTRY_DEV").pipe(
  Config.withDefault(false),
  Effect.orElseSucceed(() => false),
);

const unavailable: DesktopCrashReportingState = { available: false, instrumentation: undefined };

/** Off unless the pre-ready layer below started Sentry. */
export const DesktopCrashReporting = Context.Reference<DesktopCrashReportingState>(
  "@onicode/desktop/app/DesktopCrashReporting",
  { defaultValue: () => unavailable },
);

/**
 * Starts Sentry in the main process. It must run before Electron is ready, since it registers the
 * protocol renderers report through, and after userData is set, since Sentry stores its queue and
 * crash dumps there. The consent is this app's own desktop setting, apart from any environment's.
 */
const make = Effect.gen(function* () {
  if (builtInDsn.length === 0) return unavailable;
  const environment = yield* DesktopEnvironment.DesktopEnvironment;
  // Local development stays out of Sentry unless ONICODE_SENTRY_DEV=1 asks for it.
  if (environment.isDevelopment && !(yield* reportsInDevelopment)) return unavailable;
  const fileSystem = yield* FileSystem.FileSystem;
  const enabled = yield* fileSystem.readFileString(environment.desktopSettingsPath).pipe(
    Effect.map(parseErrorReportingConsent),
    Effect.catchIf(
      (error) => error.reason._tag === "NotFound",
      () => Effect.succeed(true),
    ),
    Effect.orElseSucceed(() => false),
  );
  if (!enabled) return { available: true, instrumentation: undefined };

  // Required here, not imported, so builds and installs without reporting never load the SDK.
  const Sentry = require("@sentry/electron/main") as typeof import("@sentry/electron/main");
  const SharedSentry = require("@onicode/shared/sentry") as typeof import("@onicode/shared/sentry");
  const { scrubSentryPayload } =
    require("@onicode/shared/sentryScrub") as typeof import("@onicode/shared/sentryScrub");
  // Renderer events, breadcrumbs included, pass through these hooks too.
  const scrub = <T>(payload: T) => scrubSentryPayload(payload, environment.homeDirectory);
  Sentry.init({
    dsn: builtInDsn,
    release: `onicode-desktop@${environment.appVersion}`,
    environment: environment.isDevelopment ? "development" : "production",
    tracesSampleRate: SharedSentry.TRACES_SAMPLE_RATE,
    dataCollection: SharedSentry.privateDataCollection,
    // Outgoing requests go to providers and other third parties; keep trace headers off them.
    tracePropagationTargets: [],
    // A stack on a log message makes Sentry title the issue by its frame, not the message.
    attachStacktrace: false,
    // Console output can quote prompts and file contents; Effect logs give the breadcrumbs.
    integrations: (defaults) => defaults.filter((integration) => integration.name !== "Console"),
    beforeSend: scrub,
    beforeSendTransaction: scrub,
    beforeSendSpan: scrub,
    beforeBreadcrumb: scrub,
  });
  // Electron keeps only the last scheme registration, and Sentry's replaced ours. Sentry now
  // proxies the call and appends its own scheme, so registering again keeps both.
  ElectronProtocol.registerDesktopSchemePrivilegesSync();
  return {
    available: true,
    instrumentation: SharedSentry.makeSentryEffectInstrumentation(),
  } satisfies DesktopCrashReportingState;
});

export const layer = Layer.effect(DesktopCrashReporting, make);
