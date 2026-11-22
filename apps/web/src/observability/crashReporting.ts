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
        ...defaults.filter(
          (integration) => integration.name !== "Console" && integration.name !== "Breadcrumbs",
        ),
        // Click breadcrumbs quote attributes such as aria-label, which can name paths.
        Sentry.breadcrumbsIntegration({ dom: false }),
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
  let sendErrorReplay = (_replayId: unknown) => {};
  const replay = Sentry.replayIntegration({
    maskAllText: true,
    maskAllInputs: true,
    blockAllMedia: true,
    // A new window must not resume a replay recorded on an address this one has not checked.
    stickySession: false,
    // rrweb records addresses before any attribute masking, and file links put paths in them.
    block: ["a[href]", "iframe", "webview"],
    beforeAddRecordingEvent: scrub,
    beforeErrorSampling: (event) => {
      sendErrorReplay(event.tags?.["replayId"]);
      return false;
    },
  });
  // The public options mask only named attributes; this masks all but layout and state.
  const options = (replay as unknown as { _recordingOptions?: { maskAttributeFn?: unknown } })
    ._recordingOptions;
  if (options?.maskAttributeFn === undefined) return undefined;
  options.maskAttributeFn = maskReplayAttribute;
  sendErrorReplay = makeErrorReplaySender(replay);
  return replay;
}

const addressIsPrivate = () => addressHasQuery(window.location.hash, window.location.search);

// Sentry drops a replay shorter than its 4.999s `minReplayDuration`.
const MIN_REPLAY_MS = 5_500;
// Sentry starts buffering inside init.
let bufferStartedAt = Date.now();

function startBuffering(replay: ReplayIntegration): void {
  replay.startBuffering();
  bufferStartedAt = Date.now();
}

/** Buffers a new replay unless the current address may carry a prompt. */
function restartBuffering(replay: ReplayIntegration): void {
  // Without a flush, stop drops the buffer synchronously, so nothing recorded here is sent.
  void replay.stop({ flush: false });
  if (!addressIsPrivate()) startBuffering(replay);
}

/**
 * Sends the recording buffered before an error, then buffers afresh. Sentry would otherwise keep
 * uploading everything after the first error until the session ends.
 */
function makeErrorReplaySender(replay: ReplayIntegration): (replayId: unknown) => void {
  let sending = false;
  return (replayId) => {
    if (sending) return;
    sending = true;
    const send = () => {
      // A stopped replay would restart in full recording here, possibly on a private address.
      if (replayId === undefined || replay.getReplayId() !== replayId) {
        sending = false;
        return;
      }
      void replay
        .flush({ continueRecording: false })
        .catch(() => {})
        .then(() => {
          sending = false;
          restartBuffering(replay);
        });
    };
    setTimeout(send, Math.max(0, bufferStartedAt + MIN_REPLAY_MS - Date.now()));
  };
}

function pauseReplayOnAddressesWithQuery(replay: ReplayIntegration): void {
  const sync = () => {
    const paused = addressIsPrivate();
    if (paused && replay.getReplayId() !== undefined) void replay.stop({ flush: false });
    else if (!paused && replay.getReplayId() === undefined) startBuffering(replay);
  };
  // The router's hash history uses pushState, which fires no hashchange.
  window.navigation?.addEventListener("currententrychange", sync);
  sync();
}
