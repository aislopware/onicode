import { create } from "zustand";

import {
  beginBugReport,
  bugReportAvailability,
  type BugReportAvailability,
} from "./observability/crashReportingStartup";

interface BugReportState {
  readonly open: boolean;
  /** Read when the form opened, so it does not change while the form closes. */
  readonly availability: BugReportAvailability;
  /** An error the report is about, when it was opened from an error screen. */
  readonly associatedEventId: string | undefined;
  /** Taken before the form opened, so it shows what the user was looking at. */
  readonly screenshot: Uint8Array | undefined;
  readonly screenshotUrl: string | undefined;
  /** The replay sent when the form opened: what led up to the report, not the form itself. */
  readonly replayId: Promise<string | undefined> | undefined;
  /** Bumps on each open, so each report starts empty and a late send closes only its own form. */
  readonly session: number;
  readonly sentSession: number | undefined;
}

export const useBugReportStore = create<BugReportState>(() => ({
  open: false,
  availability: "unavailable",
  associatedEventId: undefined,
  screenshot: undefined,
  screenshotUrl: undefined,
  replayId: undefined,
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
    const availability = bugReportAvailability();
    const ready = availability === "ready";
    if (ready) await animationsSettled();
    const screenshot = ready
      ? await window.desktopBridge?.captureWindowScreenshot?.().catch(() => null)
      : null;
    const replayId = ready ? beginBugReport() : undefined;
    const previousUrl = useBugReportStore.getState().screenshotUrl;
    // Revoked only now, so the last form's preview stayed while it closed.
    if (previousUrl) URL.revokeObjectURL(previousUrl);
    useBugReportStore.setState((state) => ({
      open: true,
      availability,
      associatedEventId: options.associatedEventId,
      screenshot: screenshot ?? undefined,
      screenshotUrl: screenshot
        ? URL.createObjectURL(new Blob([new Uint8Array(screenshot)], { type: "image/jpeg" }))
        : undefined,
      replayId,
      session: state.session + 1,
    }));
    return useBugReportStore.getState().session;
  } finally {
    opening = false;
  }
}

/** Closes the form of `session`, if it is still the open one, and records whether it was sent. */
export function closeBugReport(session: number, sent: boolean): void {
  useBugReportStore.setState((state) => ({
    ...(state.session === session ? { open: false } : {}),
    sentSession: sent ? session : state.sentSession,
  }));
}

/** Waits for closing menus and dialogs to finish animating, so the screenshot does not show them. */
async function animationsSettled(): Promise<void> {
  const finite = document
    .getAnimations()
    .filter((animation) => animation.effect?.getComputedTiming().endTime !== Infinity);
  if (finite.length === 0) return;
  await Promise.race([
    Promise.allSettled(finite.map((animation) => animation.finished)),
    new Promise((resolve) => setTimeout(resolve, 500)),
  ]);
}
