import { privateDataCollection, TRACES_SAMPLE_RATE } from "@onicode/shared/sentry";
import { scrubSentryPayload } from "@onicode/shared/sentryScrub";
import * as Sentry from "@sentry/electron/renderer";
import { init as reactInit, reactErrorHandler } from "@sentry/react";

import type { CaughtErrorHandler } from "./crashReportingStartup";
import { addressHasQuery, maskReplayAttribute } from "./replayPrivacy";

// The main process adds the home folder scrub to events; spans and replays skip its hooks.
const scrub = <T>(payload: T): T => scrubSentryPayload(payload, "");

/**
 * Reports renderer errors, performance and replays through the main process, which owns the DSN,
 * consent and event scrubbing. Loaded only when the main process started Sentry.
 */
export function startCrashReporting(): CaughtErrorHandler {
  const replay = makeReplayIntegration();
  Sentry.init(
    {
      tracesSampleRate: TRACES_SAMPLE_RATE,
      dataCollection: privateDataCollection,
      // Requests go to the user's own servers; keep trace headers off them.
      tracePropagationTargets: [],
      // Replays are buffered in memory and sent only with an error.
      replaysSessionSampleRate: 0,
      replaysOnErrorSampleRate: 1,
      integrations: (defaults) => [
        // Console output can quote prompts and file contents.
        ...defaults.filter((integration) => integration.name !== "Console"),
        Sentry.browserTracingIntegration(),
        ...(replay ? [replay] : []),
      ],
      beforeSendTransaction: scrub,
      beforeSendSpan: scrub,
      beforeBreadcrumb: scrub,
    },
    reactInit,
  );
  // Replay events, with the page URLs they list, reach Sentry through event processors only.
  Sentry.addEventProcessor(scrub);
  if (replay) pauseReplayOnAddressesWithQuery(replay);
  const report = reactErrorHandler();
  return (error, errorInfo) => report(error, { componentStack: errorInfo.componentStack ?? null });
}

type ReplayIntegration = ReturnType<typeof Sentry.replayIntegration>;

/** Undefined when this SDK version no longer lets Onicode mask every content attribute. */
function makeReplayIntegration(): ReplayIntegration | undefined {
  const replay = Sentry.replayIntegration({
    maskAllText: true,
    maskAllInputs: true,
    blockAllMedia: true,
    // Their addresses are recorded unmasked and can carry tokens and file paths.
    block: ['a[href*="://"]', 'a[href^="mailto:"]', "iframe", "webview"],
    beforeAddRecordingEvent: scrub,
  });
  // The public options mask only named attributes; this masks all but layout and state.
  const options = (replay as unknown as { _recordingOptions?: { maskAttributeFn?: unknown } })
    ._recordingOptions;
  if (options?.maskAttributeFn === undefined) return undefined;
  options.maskAttributeFn = maskReplayAttribute;
  return replay;
}

function pauseReplayOnAddressesWithQuery(replay: ReplayIntegration): void {
  const sync = () => {
    const paused = addressHasQuery(window.location.hash, window.location.search);
    if (paused && replay.getReplayId() !== undefined) void replay.stop();
    else if (!paused && replay.getReplayId() === undefined) replay.startBuffering();
  };
  // The router's hash history uses pushState, which fires no hashchange.
  window.navigation?.addEventListener("currententrychange", sync);
  sync();
}
