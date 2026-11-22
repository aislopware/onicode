import { privateDataCollection, TRACES_SAMPLE_RATE } from "@onicode/shared/sentry";
import { scrubSentryPayload } from "@onicode/shared/sentryScrub";
import * as Sentry from "@sentry/electron/renderer";
import { init as reactInit, reactErrorHandler } from "@sentry/react";

import type { CaughtErrorHandler } from "./crashReportingStartup";
import { createReplayHold } from "./replayHold";
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
  if (replay) {
    const hold = createReplayHold(replay, addressIsPrivate);
    replayHold = hold;
    // The router's hash history uses pushState, which fires no hashchange. Sentry can restart an
    // expired replay a few microtasks after the address changed, so check once more after that.
    window.navigation?.addEventListener("currententrychange", () => {
      hold.syncAddress();
      setTimeout(hold.syncAddress);
    });
    hold.syncAddress();
  }
  const report = reactErrorHandler((error: unknown, _errorInfo, eventId) => {
    if (typeof error === "object" && error !== null) reportedEventIds.set(error, eventId);
  });
  return (error, errorInfo) => report(error, { componentStack: errorInfo.componentStack ?? null });
}

type ReplayIntegration = ReturnType<typeof Sentry.replayIntegration>;

let replayHold: ReturnType<typeof createReplayHold> | undefined;
const reportedEventIds = new WeakMap<object, string>();

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
      // After the error is sent, so its replay link and the replay land together.
      if (typeof replayId === "string") setTimeout(() => void replayHold?.sendBuffered(replayId));
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

export interface BugReport {
  readonly message: string;
  readonly email: string | undefined;
  readonly associatedEventId: string | undefined;
  readonly screenshot: Uint8Array | undefined;
  readonly replayId: string | undefined;
}

/** Sends the replay buffered so far, from before the report form opened, and resolves to its id. */
export const beginBugReport = (): Promise<string | undefined> =>
  replayHold?.sendBuffered() ?? Promise.resolve(undefined);

/**
 * Sends a user's bug report, linked to the replay `beginBugReport` sent. Resolves once the main
 * process has it: sent, or queued while offline. Rejects when Sentry refused it.
 */
export function sendBugReport(report: BugReport): Promise<void> {
  const client = Sentry.getClient();
  if (client === undefined) return Promise.reject(new Error("Sentry is not running."));
  let eventId: string | undefined;
  const answered = new Promise<void>((resolve, reject) => {
    const timeout = setTimeout(() => {
      stopListening();
      reject(new Error("The main process did not answer."));
    }, 30_000);
    const stopListening = client.on("afterSendEvent", (event, response) => {
      if (event.event_id !== eventId) return;
      clearTimeout(timeout);
      stopListening();
      // No status: the main process stored it offline and sends it when it can.
      const status = response?.statusCode;
      if (status === undefined || (status >= 200 && status < 300)) resolve();
      else reject(new Error(`Sentry answered ${status}.`));
    });
  });
  eventId = Sentry.withScope((scope) => {
    const { replayId } = report;
    if (replayId !== undefined) {
      scope.addEventProcessor((event) => {
        if (event.type === "feedback" && event.contexts?.feedback) {
          event.contexts.feedback.replay_id = replayId;
        }
        return event;
      });
    }
    return Sentry.captureFeedback(
      {
        message: report.message,
        ...(report.email ? { email: report.email } : {}),
        ...(report.associatedEventId ? { associatedEventId: report.associatedEventId } : {}),
        url: `${location.origin}${location.pathname}`,
        source: "onicode",
      },
      {
        // Sentry's own link turns the buffer into a session that keeps uploading.
        includeReplay: false,
        ...(report.screenshot
          ? {
              attachments: [
                { filename: "screenshot.jpg", data: report.screenshot, contentType: "image/jpeg" },
              ],
            }
          : {}),
      },
      scope,
    );
  });
  return answered;
}

/** The Sentry event an error boundary's error was reported as. */
export const reportedEventId = (error: unknown): string | undefined =>
  typeof error === "object" && error !== null ? reportedEventIds.get(error) : undefined;
