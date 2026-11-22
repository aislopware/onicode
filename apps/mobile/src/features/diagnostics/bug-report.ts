import {
  beginBugReportReplay,
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
  // Turned off since launch: Sentry still runs, but the user said no.
  if (savedEnabled === false) return "off";
  if (bugReportsReady()) return "ready";
  if (crashReportingLaunchPreference() === true) return "restart";
  return savedEnabled === true ? "restart" : "off";
}

export interface BugReportDraft {
  readonly associatedEventId: string | undefined;
  readonly screenshot: BugReportScreenshot | undefined;
  /** Sends the replay buffered before the sheet opened, once; the sheet calls it when it can send. */
  readonly sendReplay: () => Promise<string | undefined>;
}

const emptyDraft: BugReportDraft = {
  associatedEventId: undefined,
  screenshot: undefined,
  sendReplay: () => Promise.resolve(undefined),
};

// Route params must stay serializable, so the screenshot waits here for the sheet.
let pendingDraft = emptyDraft;
let opening = false;

/** The draft `openBugReport` prepared for the sheet it opened. Reading it twice is safe. */
export function currentBugReportDraft(): BugReportDraft {
  return pendingDraft;
}

/** Lets go of the screenshot once the sheet that showed `draft` is gone. */
export function releaseBugReportDraft(draft: BugReportDraft): void {
  if (pendingDraft === draft) pendingDraft = emptyDraft;
}

/**
 * Opens the bug report sheet. `screenshot` captures the screen first, so the report can show what
 * the user was looking at; Settings skips it because the capture would only show Settings.
 */
export async function openBugReport(options: {
  readonly screenshot: boolean;
  readonly associatedEventId?: string | undefined;
}): Promise<void> {
  if (opening) return;
  if (rootNavigationRef.isReady() && rootNavigationRef.getCurrentRoute()?.name === "BugReport") {
    return;
  }
  opening = true;
  try {
    const screenshot = options.screenshot ? await captureBugReportScreenshot() : undefined;
    let replay: Promise<string | undefined> | undefined;
    pendingDraft = {
      associatedEventId: options.associatedEventId,
      screenshot,
      sendReplay: () => (replay ??= beginBugReportReplay()),
    };
  } finally {
    opening = false;
  }
  whenRootNavigationReady(() => {
    if (rootNavigationRef.getCurrentRoute()?.name !== "BugReport") {
      rootNavigationRef.navigate("BugReport");
    }
  });
}

/** A data URI for the screenshot preview. */
export function screenshotUri(screenshot: BugReportScreenshot): string {
  let binary = "";
  for (let start = 0; start < screenshot.data.length; start += 0x8000) {
    binary += String.fromCharCode(...screenshot.data.subarray(start, start + 0x8000));
  }
  return `data:${screenshot.contentType};base64,${btoa(binary)}`;
}
