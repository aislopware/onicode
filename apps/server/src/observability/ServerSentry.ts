import * as HostProcess from "@onicode/shared/HostProcess";
import { scrubSentryPayload } from "@onicode/shared/sentryScrub";
import * as Config from "effect/Config";
import * as Effect from "effect/Effect";

import * as ServerConfig from "../config.ts";
import packageJson from "../../package.json" with { type: "json" };

// Release builds name their Sentry project here. The repository names none, so forks send nothing.
declare const __ONICODE_SENTRY_SERVER_DSN__: string | undefined;
const builtInDsn =
  typeof __ONICODE_SENTRY_SERVER_DSN__ === "undefined" ? "" : __ONICODE_SENTRY_SERVER_DSN__;

const reportsInDevelopment = Config.Boolean("ONICODE_SENTRY_DEV").pipe(
  Config.withDefault(false),
  Effect.orElseSucceed(() => false),
);

/** True when this run can send reports, so the user's choice decides whether it does. */
export const isAvailable = Effect.fn("ServerSentry.isAvailable")(function* () {
  if (builtInDsn.length === 0) return false;
  const config = yield* ServerConfig.ServerConfig;
  // Local development stays out of Sentry unless ONICODE_SENTRY_DEV=1 asks for it.
  return !config.dev || (yield* reportsInDevelopment);
});

// Spans that fire thousands of times a minute and only add noise to a trace.
const ignoredSpanNames = new Set([
  "AgentSessionScanner.readCwd",
  "AgentSessionScanner.transcriptChunkReader",
  "WorkspacePaths.resolveRelativePathWithinRoot",
  "ProjectFaviconResolver.findExistingFile",
  "ProjectFaviconResolver.findIconFromSource",
  "ProjectFaviconResolver.firstInOrder",
  "SAVEPOINT",
  "RELEASE SAVEPOINT",
]);

/**
 * Starts Sentry for the life of the scope when this build has a DSN and the user has not turned
 * reporting off. Returns undefined otherwise.
 */
export const start = Effect.fn("ServerSentry.start")(function* () {
  const config = yield* ServerConfig.ServerConfig;
  if (!(yield* isAvailable()) || config.errorReportingEnabled !== true) return undefined;
  const homeDirectory = yield* HostProcess.HomeDirectory;
  // Loaded only when reporting runs, so builds and installs without it pay nothing at startup.
  const [Sentry, SharedSentry] = yield* Effect.promise(() =>
    Promise.all([import("@sentry/node"), import("@onicode/shared/sentry")]),
  );
  const scrub = <T>(payload: T) => scrubSentryPayload(payload, homeDirectory);

  yield* Effect.acquireRelease(
    Effect.sync(() =>
      Sentry.init({
        dsn: builtInDsn,
        release: `onicode-server@${packageJson.version}`,
        environment: SharedSentry.sentryEnvironment(ServerConfig.releaseChannel),
        tracesSampleRate: SharedSentry.TRACES_SAMPLE_RATE,
        dataCollection: SharedSentry.privateDataCollection,
        // Outgoing requests go to providers and other third parties; keep trace headers off them.
        tracePropagationTargets: [],
        // The machine's name often carries the user's.
        includeServerName: false,
        // A stack on a log message makes Sentry title the issue by its frame, not the message.
        attachStacktrace: false,
        // Effect records its own spans and logs; the Node auto-instrumentation would duplicate them.
        defaultIntegrations: [
          Sentry.eventFiltersIntegration(),
          Sentry.functionToStringIntegration(),
          Sentry.linkedErrorsIntegration(),
          Sentry.dedupeIntegration(),
          Sentry.nodeContextIntegration(),
          Sentry.onUncaughtExceptionIntegration(),
          // Node crashes on an unhandled rejection; a lenient handler would keep a broken server alive.
          Sentry.onUnhandledRejectionIntegration({ mode: "strict" }),
        ],
        initialScope: { tags: { "onicode.server.managed_by": config.mode } },
        beforeSend: scrub,
        beforeSendTransaction: scrub,
        beforeSendSpan: scrub,
        beforeBreadcrumb: scrub,
      }),
    ),
    () => Effect.promise(() => Sentry.close(2_000)),
  );

  return SharedSentry.makeSentryEffectInstrumentation({ ignoredSpanNames });
});
