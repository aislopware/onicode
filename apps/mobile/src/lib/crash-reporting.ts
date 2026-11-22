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

/**
 * Errors an error boundary rendered a fallback for, which never reach the global handler. Returns
 * the Sentry event, so a bug report from that fallback links to it.
 */
export function reportRenderError(
  error: unknown,
  componentStack: string | undefined,
): string | undefined {
  return reporter?.captureRenderError(error, componentStack);
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

/** Sends the replay buffered so far, before the report sheet opened, resolving to its id. */
export function beginBugReportReplay(): Promise<string | undefined> {
  return reporter?.beginBugReport() ?? Promise.resolve(undefined);
}

/** Whether the report was handed to Sentry, which stores it until it is sent. */
export async function sendBugReport(report: BugReport): Promise<boolean> {
  if (!reporter) return false;
  try {
    await reporter.sendBugReport(report);
    return true;
  } catch (error) {
    console.warn("Sending a bug report failed", error);
    return false;
  }
}

/** A capture of the screen with text and images masked, offered to the user before it is sent. */
export async function captureBugReportScreenshot(): Promise<BugReportScreenshot | undefined> {
  return reporter?.captureScreenshot().catch(() => undefined);
}

/** Opens a bug report on a shake while `handler` is set, once Sentry has started. */
export function setShakeToReportHandler(handler: (() => void) | undefined): void {
  shakeHandler = handler;
  reporter?.onShake(handler);
}
