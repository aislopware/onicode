import * as Application from "expo-application";
import Constants from "expo-constants";

import type { BugReport, BugReportScreenshot, SentryReporter } from "./crash-reporting-sentry";

export type { BugReportScreenshot };

const configuredDsn: unknown = Constants.expoConfig?.extra?.sentryDsn;
const dsn = typeof configuredDsn === "string" ? configuredDsn : "";

// A production build served by Metro is still local work; report from it only on request.
const reportsInDevelopment = Constants.expoConfig?.extra?.sentryInDevelopment === true;

/** Whether this build was made with a Sentry DSN. Builds without one never load the SDK. */
export const crashReportingAvailable = dsn.length > 0 && (!__DEV__ || reportsInDevelopment);

let launchPreference: boolean | undefined;
let reporter: SentryReporter | undefined;
let navigationContainer: unknown;
let shakeHandler: (() => void) | undefined;

/**
 * Starts Sentry once per launch when this build has a DSN and `enabled` is the device's setting.
 * The SDK is imported only then, so builds and devices without reporting never evaluate it.
 */
export function startCrashReporting(enabled: boolean): void {
  if (launchPreference !== undefined) return;
  launchPreference = enabled;
  if (!crashReportingAvailable || !enabled) return;
  void import("./crash-reporting-sentry")
    .then(({ startSentry }) => {
      reporter = startSentry({
        dsn,
        release: `onicode-mobile@${Constants.expoConfig?.version ?? "0.0.0"}`,
        dist: Application.nativeBuildVersion ?? undefined,
        environment:
          Constants.expoConfig?.extra?.appVariant === "production" ? "production" : "development",
      });
      if (navigationContainer !== undefined) {
        reporter.registerNavigationContainer(navigationContainer);
      }
      if (shakeHandler !== undefined) reporter.onShake(shakeHandler);
    })
    .catch((error: unknown) => console.warn("Crash reporting failed to start", error));
}

/** The error reporting setting this launch started with; a different saved value needs a restart. */
export function crashReportingLaunchPreference(): boolean | undefined {
  return launchPreference;
}

/** Errors an error boundary rendered a fallback for, which never reach the global handler. */
export function reportRenderError(error: unknown, componentStack: string | undefined): void {
  reporter?.captureRenderError(error, componentStack);
}

/** Ends the app start span once the splash screen hides. */
export function markCrashReportingAppLoaded(): void {
  reporter?.appLoaded();
}

/** The root navigation container, each time one becomes ready, for navigation spans. */
export function registerCrashReportingNavigation(container: unknown): void {
  navigationContainer = container;
  reporter?.registerNavigationContainer(container);
}

/** Whether this launch can send a bug report: only while error reports run. */
export function bugReportsReady(): boolean {
  return reporter !== undefined;
}

/** Whether the report was handed to Sentry, which stores it until it is sent. */
export function sendBugReport(report: BugReport): boolean {
  if (!reporter) return false;
  try {
    reporter.sendBugReport(report);
    return true;
  } catch (error) {
    console.warn("Sending a bug report failed", error);
    return false;
  }
}

/** An unmasked capture of the screen, offered to the user before it goes with a report. */
export async function captureBugReportScreenshot(): Promise<BugReportScreenshot | undefined> {
  return reporter?.captureScreenshot().catch(() => undefined);
}

/** The last error this launch reported, so a report from an error screen links to it. */
export function lastReportedErrorId(): string | undefined {
  return reporter?.lastEventId();
}

/** Opens a bug report on a shake while `handler` is set, once Sentry has started. */
export function setShakeToReportHandler(handler: (() => void) | undefined): void {
  shakeHandler = handler;
  reporter?.onShake(handler);
}
