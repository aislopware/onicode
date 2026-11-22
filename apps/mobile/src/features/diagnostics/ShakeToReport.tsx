import { useAtomValue } from "@effect/atom-react";
import { AsyncResult } from "effect/reactivity";
import { useEffect } from "react";

import { crashReportingAvailable, setShakeToReportHandler } from "../../lib/crash-reporting";
import { mobilePreferencesAtom } from "../../state/preferences";
import { openBugReport } from "./bug-report";

/** Opens a bug report when the device is shaken, while the device's setting allows it. */
export function ShakeToReport() {
  const preferences = useAtomValue(mobilePreferencesAtom);
  const enabled =
    crashReportingAvailable &&
    AsyncResult.isSuccess(preferences) &&
    preferences.value.shakeToReportEnabled !== false;
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- starts and stops the native shake listener
  useEffect(() => {
    if (!enabled) return;
    setShakeToReportHandler(() => void openBugReport({ screenshot: true }));
    return () => setShakeToReportHandler(undefined);
  }, [enabled]);
  return null;
}
