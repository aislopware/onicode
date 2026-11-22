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
  Sentry.addEventProcessor((event) => {
    if (event.type === "feedback" && feedbackReplayId !== undefined && event.contexts?.feedback) {
      event.contexts.feedback.replay_id = feedbackReplayId;
    }
    return scrub(event);
  });
  if (replay) {
    activeReplay = replay;
    pauseReplayOnAddressesWithQuery(replay);
  }
  const report = reactErrorHandler();
  return (error, errorInfo) => report(error, { componentStack: errorInfo.componentStack ?? null });
}

type ReplayIntegration = ReturnType<typeof Sentry.replayIntegration>;

let activeReplay: ReplayIntegration | undefined;
// Set while a send owns the replay, so navigation does not restart it underneath.
let replayHeld = false;
let replayTasks: Promise<unknown> = Promise.resolve();
let feedbackReplayId: string | undefined;

/** Undefined when this SDK version no longer lets Onicode mask every content attribute. */
function makeReplayIntegration(): ReplayIntegration | undefined {
  const replay = Sentry.replayIntegration({
    maskAllText: true,
    maskAllInputs: true,
    blockAllMedia: true,
    // A new window must not resume a replay recorded on an address this one has not checked.
    stickySession: false,
    // The buffer restarts at each 60s snapshot; an error just after one still gets its replay.
    minReplayDuration: 0,
    // rrweb records addresses before any attribute masking, and file links put paths in them.
    block: ["a[href]", "iframe", "webview"],
    beforeAddRecordingEvent: scrub,
    beforeErrorSampling: (event) => {
      const replayId = event.tags?.["replayId"];
      if (typeof replayId === "string") sendErrorReplay(replayId);
      return false;
    },
  });
  // The public options mask only named attributes; this masks all but layout and state.
  const options = (replay as unknown as { _recordingOptions?: { maskAttributeFn?: unknown } })
    ._recordingOptions;
  if (options?.maskAttributeFn === undefined) return undefined;
  options.maskAttributeFn = maskReplayAttribute;
  return replay;
}

const addressIsPrivate = () => addressHasQuery(window.location.hash, window.location.search);

/** Runs sends one at a time, holding the replay; it buffers afresh after each. */
function withHeldReplay<T>(
  task: (replay: ReplayIntegration) => Promise<T>,
): Promise<T | undefined> {
  const replay = activeReplay;
  if (replay === undefined) return Promise.resolve(undefined);
  const run = replayTasks.then(async () => {
    replayHeld = true;
    try {
      return await task(replay);
    } finally {
      // Without a flush, stop drops the buffer synchronously, so nothing recorded here is sent.
      void replay.stop({ flush: false });
      replayHeld = false;
      if (!addressIsPrivate()) replay.startBuffering();
    }
  });
  replayTasks = run.catch(() => {});
  return run;
}

/**
 * Sends the recording buffered so far and stops it, returning its id. Sentry would otherwise turn
 * the buffer into a session that keeps uploading until the session ends.
 */
async function sendBufferedReplay(
  replay: ReplayIntegration,
  expectedId?: string,
): Promise<string | undefined> {
  const replayId = replay.getReplayId();
  // A stopped replay would restart in full recording on flush, possibly on a private address.
  if (replayId === undefined || (expectedId !== undefined && replayId !== expectedId)) return;
  await replay.flush({ continueRecording: false }).catch(() => {});
  void replay.stop({ flush: false });
  return replayId;
}

function sendErrorReplay(replayId: string): void {
  setTimeout(() => void withHeldReplay((replay) => sendBufferedReplay(replay, replayId)));
}

export interface BugReport {
  readonly message: string;
  readonly email: string | undefined;
  readonly associatedEventId: string | undefined;
  readonly screenshot: Uint8Array | undefined;
}

/** Sends a user's bug report with the replay buffered before it, linked to it. */
export async function sendBugReport(report: BugReport): Promise<void> {
  const send = async () => {
    // Resolves once the main process has the server's answer, and rejects when it refused.
    await Sentry.sendFeedback(
      {
        message: report.message,
        ...(report.email ? { email: report.email } : {}),
        ...(report.associatedEventId ? { associatedEventId: report.associatedEventId } : {}),
        url: `${location.origin}${location.pathname}`,
        source: "onicode",
      },
      {
        includeReplay: false,
        ...(report.screenshot
          ? {
              attachments: [
                { filename: "screenshot.jpg", data: report.screenshot, contentType: "image/jpeg" },
              ],
            }
          : {}),
      },
    );
  };
  if (activeReplay === undefined) return send();
  await withHeldReplay(async (replay) => {
    // The replay stays stopped until the report is sent, so Sentry does not flush it again.
    feedbackReplayId = await sendBufferedReplay(replay);
    try {
      await send();
    } finally {
      feedbackReplayId = undefined;
    }
  });
}

/** The id of the last error this window reported, for a report about it. */
export const lastReportedEventId = (): string | undefined => Sentry.lastEventId();

function pauseReplayOnAddressesWithQuery(replay: ReplayIntegration): void {
  const sync = () => {
    if (replayHeld) return;
    const paused = addressIsPrivate();
    if (paused && replay.getReplayId() !== undefined) void replay.stop({ flush: false });
    else if (!paused && replay.getReplayId() === undefined) replay.startBuffering();
  };
  // The router's hash history uses pushState, which fires no hashchange. Sentry can restart an
  // expired replay a few microtasks after the address changed, so check once more after that.
  window.navigation?.addEventListener("currententrychange", () => {
    sync();
    setTimeout(sync);
  });
  sync();
}
