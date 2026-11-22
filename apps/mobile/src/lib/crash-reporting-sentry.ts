import { scrubSentryPayload } from "@onicode/shared/sentryScrub";
import * as Sentry from "@sentry/react-native";

export interface SentryReporter {
  readonly captureRenderError: (error: unknown, componentStack: string | undefined) => void;
  readonly appLoaded: () => void;
  readonly registerNavigationContainer: (container: unknown) => void;
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
      Sentry.mobileReplayIntegration({
        maskAllText: true,
        maskAllImages: true,
        maskAllVectors: true,
      }),
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
  };
}

const scrub = <T>(payload: T): T => scrubSentryPayload(payload, "");
