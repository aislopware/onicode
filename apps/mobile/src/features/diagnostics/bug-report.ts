import {
  type BugReportScreenshot,
  bugReportsReady,
  captureBugReportScreenshot,
  crashReportingAvailable,
  crashReportingLaunchPreference,
} from "../../lib/crash-reporting";
import { rootNavigationRef, whenRootNavigationReady } from "../../lib/rootNavigation";

/**
 * Whether this launch can send a bug report. `unavailable`: the build has no Sentry project.
 * `off`: this device's error reports are off. `restart`: they were turned on after this launch.
 */
export type BugReportAvailability = "unavailable" | "off" | "restart" | "ready";

export function bugReportAvailability(savedEnabled: boolean | null): BugReportAvailability {
  if (!crashReportingAvailable) return "unavailable";
  if (bugReportsReady()) return "ready";
  if (crashReportingLaunchPreference() === true) return "restart";
  return savedEnabled === true ? "restart" : "off";
}

export interface BugReportDraft {
  readonly associatedEventId: string | undefined;
  readonly screenshot: BugReportScreenshot | undefined;
}

// Route params must stay serializable, so the screenshot waits here for the sheet.
let pendingDraft: BugReportDraft = { associatedEventId: undefined, screenshot: undefined };

/** The draft `openBugReport` prepared, taken once by the sheet it opened. */
export function takeBugReportDraft(): BugReportDraft {
  const draft = pendingDraft;
  pendingDraft = { associatedEventId: undefined, screenshot: undefined };
  return draft;
}

/**
 * Opens the bug report sheet. `screenshot` captures the screen first, so the report can show what
 * the user was looking at; Settings skips it because the capture would only show Settings.
 */
export async function openBugReport(options: {
  readonly screenshot: boolean;
  readonly associatedEventId?: string | undefined;
}): Promise<void> {
  if (rootNavigationRef.isReady() && rootNavigationRef.getCurrentRoute()?.name === "BugReport") {
    return;
  }
  const screenshot = options.screenshot ? await captureBugReportScreenshot() : undefined;
  pendingDraft = { associatedEventId: options.associatedEventId, screenshot };
  whenRootNavigationReady(() => rootNavigationRef.navigate("BugReport"));
}

/** A data URI for the screenshot preview. */
export function screenshotUri(screenshot: BugReportScreenshot): string {
  let binary = "";
  for (let start = 0; start < screenshot.data.length; start += 0x8000) {
    binary += String.fromCharCode(...screenshot.data.subarray(start, start + 0x8000));
  }
  return `data:${screenshot.contentType};base64,${btoa(binary)}`;
}
