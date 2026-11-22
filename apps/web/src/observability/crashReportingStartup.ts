import type { RootOptions } from "react-dom/client";

import type { BugReport } from "./crashReporting";

export type CaughtErrorHandler = NonNullable<RootOptions["onCaughtError"]>;

let reportCaughtError: CaughtErrorHandler | undefined;
let crashReporting: Promise<typeof import("./crashReporting")> | undefined;
let lastReportedEventId: (() => string | undefined) | undefined;

/** Starts renderer crash reporting when the desktop main process has it on; otherwise loads nothing. */
export function startCrashReportingIfEnabled(): void {
  if (!import.meta.env.ONICODE_SENTRY_BUILD) return;
  if (window.desktopBridge?.getErrorReporting?.()?.running !== true) return;
  crashReporting = import("./crashReporting");
  void crashReporting
    .then((module) => {
      reportCaughtError = module.startCrashReporting();
      lastReportedEventId = module.lastReportedEventId;
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
 * `off`: the app's error reports are off. `restart`: they were turned on after this launch.
 */
export type BugReportAvailability = "unavailable" | "off" | "restart" | "ready";

export function bugReportAvailability(): BugReportAvailability {
  if (!import.meta.env.ONICODE_SENTRY_BUILD) return "unavailable";
  const state = window.desktopBridge?.getErrorReporting?.();
  if (!state) return "unavailable";
  if (state.running) return crashReporting === undefined ? "restart" : "ready";
  return state.saved ? "restart" : "off";
}

export async function sendBugReport(report: BugReport): Promise<void> {
  if (crashReporting === undefined) throw new Error("Error reports are off in this window.");
  const module = await crashReporting;
  await module.sendBugReport(report);
}

/** The last error this window reported, so a report from an error screen links to it. */
export const lastReportedErrorId = (): string | undefined => lastReportedEventId?.();
