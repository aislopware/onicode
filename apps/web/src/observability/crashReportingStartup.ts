import type { RootOptions } from "react-dom/client";

export type CaughtErrorHandler = NonNullable<RootOptions["onCaughtError"]>;

let reportCaughtError: CaughtErrorHandler | undefined;

/** Starts renderer crash reporting when the desktop main process has it on; otherwise loads nothing. */
export function startCrashReportingIfEnabled(): void {
  if (!import.meta.env.ONICODE_SENTRY_BUILD) return;
  if (window.desktopBridge?.getErrorReporting?.()?.running !== true) return;
  void import("./crashReporting")
    .then(({ startCrashReporting }) => {
      reportCaughtError = startCrashReporting();
    })
    .catch((error: unknown) => console.warn("Crash reporting failed to start", error));
}

/** React's `onCaughtError`: errors an error boundary rendered, which never reach `window.onerror`. */
export const onCaughtReactError: CaughtErrorHandler = (error, errorInfo) => {
  console.error(error);
  reportCaughtError?.(error, errorInfo);
};
