import * as Effect from "effect/Effect";

import { crashReportingAvailable, startCrashReporting } from "../../lib/crash-reporting";
import * as Runtime from "../../lib/runtime";
import * as MobilePreferences from "../../persistence/mobile-preferences";

/**
 * Starts crash reporting with this device's setting once preferences load. Read on its own: the
 * shared preferences state treats a failed load as empty, which would undo an opt-out.
 */
export function installCrashReporting(): () => void {
  if (!crashReportingAvailable) return () => {};
  void Runtime.runtime
    .runPromise(
      MobilePreferences.MobilePreferencesStore.pipe(
        Effect.flatMap((store) => store.load),
        Effect.map((preferences) => preferences.errorReportingEnabled !== false),
        Effect.orElseSucceed(() => false),
      ),
    )
    .then(startCrashReporting, () => startCrashReporting(false));
  return () => {};
}
