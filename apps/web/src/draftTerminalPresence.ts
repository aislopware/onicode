import type { EnvironmentId, ThreadId } from "@onicode/contracts";

// The draft store must not import the terminal atoms: they load the connection runtime, which
// imports the draft store. state/terminalSessions registers the real check when it loads.
let hasTerminal: (environmentId: EnvironmentId, threadId: string) => boolean = () => false;

export function registerDraftTerminalCheck(
  check: (environmentId: EnvironmentId, threadId: string) => boolean,
): void {
  hasTerminal = check;
}

/** Whether the client knows of a terminal for a draft thread, as its sidebar row shows it. */
export function draftThreadHasKnownTerminal(environmentId: EnvironmentId, threadId: string) {
  return hasTerminal(environmentId, threadId);
}

let closeTerminals: (environmentId: EnvironmentId, threadId: ThreadId) => void = () => {};

export function registerDraftTerminalClose(
  close: (environmentId: EnvironmentId, threadId: ThreadId) => void,
): void {
  closeTerminals = close;
}

/** Ends the terminals a draft thread left behind on a machine no draft there reaches. */
export function closeLeftDraftThreadTerminals(environmentId: EnvironmentId, threadId: ThreadId) {
  closeTerminals(environmentId, threadId);
}
