import { scrubSentryPayload } from "@onicode/shared/sentryScrub";
import * as Sentry from "@sentry/react-native";
import {
  startShakeListener,
  stopShakeListener,
} from "@sentry/react-native/dist/js/feedback/ShakeToReportBug";
import { NATIVE } from "@sentry/react-native/dist/js/wrapper";

export interface BugReport {
  readonly message: string;
  readonly email: string | undefined;
  readonly associatedEventId: string | undefined;
  readonly screenshot: BugReportScreenshot | undefined;
}

export interface BugReportScreenshot {
  readonly data: Uint8Array;
  readonly contentType: string;
  readonly filename: string;
}

export interface SentryReporter {
  readonly captureRenderError: (error: unknown, componentStack: string | undefined) => void;
  readonly appLoaded: () => void;
  readonly registerNavigationContainer: (container: unknown) => void;
  readonly sendBugReport: (report: BugReport) => void;
  readonly captureScreenshot: () => Promise<BugReportScreenshot | undefined>;
  /** Native shake detection, which needs no permission. `undefined` stops it. */
  readonly onShake: (handler: (() => void) | undefined) => void;
  readonly lastEventId: () => string | undefined;
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
  const replay = Sentry.mobileReplayIntegration({
    maskAllText: true,
    maskAllImages: true,
    maskAllVectors: true,
  });
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
    captureRenderError: (error, componentStack) => {
      Sentry.captureException(error, {
        captureContext: { contexts: { react: { componentStack: componentStack ?? null } } },
        mechanism: { type: "auto.function.react.error_boundary", handled: true },
      });
    },
    appLoaded: () => Sentry.appLoaded(),
    registerNavigationContainer: (container) => navigation.registerNavigationContainer(container),
    // The native transport stores the envelope before sending, so a report survives going offline.
    sendBugReport: (report) => {
      // The SDK's own replay link turns the buffer into a session that keeps uploading, so the
      // report links the buffer itself, sends it, and goes back to buffering.
      const replayId = replay.getReplayId();
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
      if (replayId) {
        void replay
          .flush({ continueRecording: false })
          .then(() => replay.startBuffering())
          .catch((error: unknown) =>
            console.warn("Sending the replay for a bug report failed", error),
          );
      }
    },
    captureScreenshot: async () => (await NATIVE.captureScreenshot())?.[0],
    onShake: (handler) => {
      stopShakeListener();
      if (handler) startShakeListener(handler);
    },
    lastEventId: () => Sentry.lastEventId(),
  };
}

const scrub = <T>(payload: T): T => scrubSentryPayload(payload, "");
