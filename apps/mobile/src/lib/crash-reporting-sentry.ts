import { scrubSentryPayload } from "@onicode/shared/sentryScrub";
import * as Sentry from "@sentry/react-native";
import {
  startShakeListener,
  stopShakeListener,
} from "@sentry/react-native/dist/js/feedback/ShakeToReportBug";
import { NATIVE } from "@sentry/react-native/dist/js/wrapper";

import { createReplayScrollPause, type ReplayScrollPause } from "./replayScrollPause";

export interface BugReport {
  readonly message: string;
  readonly email: string | undefined;
  readonly associatedEventId: string | undefined;
  readonly screenshot: BugReportScreenshot | undefined;
  /** The replay sent when the sheet opened. */
  readonly replayId: Promise<string | undefined>;
}

export interface BugReportScreenshot {
  readonly data: Uint8Array;
  readonly contentType: string;
  readonly filename: string;
}

export interface SentryReporter {
  readonly captureRenderError: (error: unknown, componentStack: string | undefined) => string;
  readonly appLoaded: () => void;
  readonly registerNavigationContainer: (container: unknown) => void;
  /** Sends the replay buffered before the sheet opened, resolving to its id. */
  readonly beginBugReport: () => Promise<string | undefined>;
  /** Resolves once Sentry has the report, which the native transport stores until it is sent. */
  readonly sendBugReport: (report: BugReport) => Promise<void>;
  readonly captureScreenshot: () => Promise<BugReportScreenshot | undefined>;
  /** Native shake detection, which needs no permission. `undefined` stops it. */
  readonly onShake: (handler: (() => void) | undefined) => void;
  readonly scrollPause: ReplayScrollPause;
}

/** Share of traces sent to Sentry. Errors are always sent. Matches desktop and server. */
const TRACES_SAMPLE_RATE = 0.05;

/** Starts Sentry with Onicode's privacy settings. Loaded only by `startCrashReporting`. */
export function startSentry(options: {
  readonly dsn: string;
  readonly release: string;
  readonly dist: string | undefined;
  readonly environment: string;
}): SentryReporter {
  const navigation = Sentry.reactNavigationIntegration();
  let replayTasks: Promise<unknown> = Promise.resolve();
  const replay = Sentry.mobileReplayIntegration({
    maskAllText: true,
    maskAllImages: true,
    maskAllVectors: true,
  });
  const scrollPause = createReplayScrollPause(replay);
  Sentry.init({
    dsn: options.dsn,
    release: options.release,
    ...(options.dist === undefined ? {} : { dist: options.dist }),
    environment: options.environment,
    sendDefaultPii: false,
    // Requests go to the user's own servers; keep trace headers off them.
    tracePropagationTargets: [],
    tracesSampleRate: TRACES_SAMPLE_RATE,
    // Replays are buffered on the device and sent only with an error.
    replaysSessionSampleRate: 0,
    replaysOnErrorSampleRate: 1,
    integrations: [
      navigation,
      // Console output can quote prompts and file contents.
      Sentry.breadcrumbsIntegration({ console: false }),
      replay,
    ],
    // Requests to a user's servers can carry connection tickets in their query.
    beforeSend: scrub,
    beforeSendTransaction: scrub,
    beforeBreadcrumb: scrub,
  });
  return {
    captureRenderError: (error, componentStack) =>
      Sentry.captureException(error, {
        captureContext: { contexts: { react: { componentStack: componentStack ?? null } } },
        mechanism: { type: "auto.function.react.error_boundary", handled: true },
      }),
    appLoaded: () => Sentry.appLoaded(),
    registerNavigationContainer: (container) => navigation.registerNavigationContainer(container),
    // One at a time: each sends the buffer, which the next one must not read before it restarts.
    beginBugReport: () => {
      const run = replayTasks.then(async () => {
        // A paused iOS replay is not running, and captures nothing.
        scrollPause.resumeNow();
        try {
          // The native capture answers with the replay it uploaded; the integration's cached id
          // can name an earlier one.
          return (await NATIVE.captureReplay(false)) ?? undefined;
        } catch (error) {
          console.warn("Sending the replay for a bug report failed", error);
          return undefined;
        } finally {
          // The capture turns the buffer into a session that keeps uploading until stopped.
          await replay.stop().catch(() => {});
          replay.startBuffering();
        }
      });
      replayTasks = run.catch(() => {});
      return run;
    },
    // The native transport stores the envelope before sending, so a report survives going offline.
    sendBugReport: async (report) => {
      // A stuck native capture must not hold the report; it then goes without its replay.
      const replayId = await Promise.race([
        report.replayId.catch(() => undefined),
        new Promise<undefined>((resolve) => setTimeout(resolve, 10_000)),
      ]);
      Sentry.withScope((scope) => {
        if (replayId) {
          scope.addEventProcessor((event) => {
            if (event.type === "feedback" && event.contexts?.feedback) {
              event.contexts.feedback.replay_id = replayId;
            }
            return event;
          });
        }
        Sentry.captureFeedback(
          {
            message: report.message,
            ...(report.email ? { email: report.email } : {}),
            ...(report.associatedEventId ? { associatedEventId: report.associatedEventId } : {}),
            source: "onicode",
          },
          {
            includeReplay: false,
            ...(report.screenshot
              ? {
                  attachments: [
                    {
                      filename: report.screenshot.filename,
                      data: report.screenshot.data,
                      contentType: report.screenshot.contentType,
                    },
                  ],
                }
              : {}),
          },
          scope,
        );
      });
    },
    captureScreenshot: async () => (await NATIVE.captureScreenshot())?.[0],
    onShake: (handler) => {
      stopShakeListener();
      if (handler) startShakeListener(handler);
    },
    scrollPause,
  };
}

const scrub = <T>(payload: T): T => scrubSentryPayload(payload, "");
