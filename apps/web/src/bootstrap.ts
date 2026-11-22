import { showBootError } from "./lib/bootError";
import { startCrashReportingIfEnabled } from "./observability/crashReportingStartup";

startCrashReportingIfEnabled();

// Shared dev chunks need the React refresh preamble first. Catch failures before React mounts.
void import("./main").then(({ startup }) => startup).catch(showBootError);
