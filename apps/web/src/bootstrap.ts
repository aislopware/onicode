import { showBootError } from "./lib/bootError";
import { chunkLoadState, handleChunkLoadErrors } from "./lib/chunkReloadGuard";
import { startCrashReportingIfEnabled } from "./observability/crashReportingStartup";

startCrashReportingIfEnabled();
handleChunkLoadErrors();

// Shared dev chunks need the React refresh preamble first. Catch failures before React mounts.
export const boot = import("./main")
  .then(({ startup }) => startup)
  .catch((error: unknown) => {
    if (!chunkLoadState.reloadScheduled) showBootError(error);
  });
