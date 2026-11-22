import type { RootOptions } from "react-dom/client";

import type { BugReport } from "./crashReporting";

export type CaughtErrorHandler = NonNullable<RootOptions["onCaughtError"]>;

let reportCaughtError: CaughtErrorHandler | undefined;
let crashReporting: typeof import("./crashReporting") | undefined;

/** Starts renderer crash reporting when the desktop main process has it on; otherwise loads nothing. */
export function startCrashReportingIfEnabled(): void {
  if (!import.meta.env.ONICODE_SENTRY_BUILD) return;
  if (window.desktopBridge?.getErrorReporting?.()?.running !== true) return;
  void import("./crashReporting")
    .then((module) => {
      reportCaughtError = module.startCrashReporting();
      crashReporting = module;
    })
    .catch((error: unknown) => console.warn("Crash reporting failed to start", error));
}

/** React's `onCaughtError`: errors an error boundary rendered, which never reach `window.onerror`. */
export const onCaughtReactError: CaughtErrorHandler = (error, errorInfo) => {
  console.error(error);
  reportCaughtError?.(error, errorInfo);
};

/**
 * Whether this window can send a bug report. `unavailable`: the build has no Sentry project.
 * `off`: the app's error reports are off. `restart`: they were turned on after this launch, or
 * reporting failed to start in this window.
 */
export type BugReportAvailability = "unavailable" | "off" | "restart" | "ready";

let supported: boolean | undefined;

/** Whether this build can send bug reports at all. Fixed for the launch, so asked once. */
export const bugReportsSupported = (): boolean =>
  (supported ??= bugReportAvailability() !== "unavailable");

export function bugReportAvailability(): BugReportAvailability {
  if (!import.meta.env.ONICODE_SENTRY_BUILD) return "unavailable";
  const state = window.desktopBridge?.getErrorReporting?.();
  if (!state) return "unavailable";
  // Turned off since launch: Sentry still runs, but the user said no.
  if (!state.saved) return "off";
  return state.running && crashReporting !== undefined ? "ready" : "restart";
}

/** Sends the replay buffered before the form opened, resolving to its id for the report. */
export function beginBugReport(): Promise<string | undefined> {
  return crashReporting?.beginBugReport() ?? Promise.resolve(undefined);
}

export async function sendBugReport(report: BugReport): Promise<void> {
  if (crashReporting === undefined) throw new Error("Error reports are off in this window.");
  await crashReporting.sendBugReport(report);
}

/** The Sentry event an error screen's error was reported as, so a report about it links to it. */
export const reportedErrorId = (error: unknown): string | undefined =>
  crashReporting?.reportedEventId(error);
