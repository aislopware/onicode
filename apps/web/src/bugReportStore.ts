import { create } from "zustand";

import { bugReportAvailability } from "./observability/crashReportingStartup";

interface BugReportState {
  readonly open: boolean;
  /** An error the report is about, when it was opened from an error screen. */
  readonly associatedEventId: string | undefined;
  /** Taken before the dialog opened, so it shows what the user was looking at. */
  readonly screenshot: Uint8Array | undefined;
  readonly screenshotUrl: string | undefined;
  /** Bumps on each open, so the dialog starts each report empty. */
  readonly session: number;
  readonly sentSession: number | undefined;
}

export const useBugReportStore = create<BugReportState>(() => ({
  open: false,
  associatedEventId: undefined,
  screenshot: undefined,
  screenshotUrl: undefined,
  session: 0,
  sentSession: undefined,
}));

let opening = false;

/** Opens the report form, resolving to its session so a caller can tell when that report is sent. */
export async function openBugReport(
  options: { readonly associatedEventId?: string | undefined } = {},
): Promise<number | undefined> {
  if (opening || useBugReportStore.getState().open) return undefined;
  opening = true;
  try {
    const screenshot =
      bugReportAvailability() === "ready"
        ? await window.desktopBridge?.captureWindowScreenshot?.().catch(() => null)
        : null;
    useBugReportStore.setState((state) => ({
      open: true,
      associatedEventId: options.associatedEventId,
      screenshot: screenshot ?? undefined,
      screenshotUrl: screenshot
        ? URL.createObjectURL(new Blob([new Uint8Array(screenshot)], { type: "image/jpeg" }))
        : undefined,
      session: state.session + 1,
    }));
    return useBugReportStore.getState().session;
  } finally {
    opening = false;
  }
}

export function closeBugReport(sent: boolean): void {
  const { screenshotUrl } = useBugReportStore.getState();
  if (screenshotUrl) URL.revokeObjectURL(screenshotUrl);
  useBugReportStore.setState((state) => ({
    open: false,
    screenshot: undefined,
    screenshotUrl: undefined,
    sentSession: sent ? state.session : state.sentSession,
  }));
}
