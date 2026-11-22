import { runAtomCommand } from "@onicode/client-runtime/state/runtime";
import { AuthTerminalOperateScope, type EnvironmentId, type ThreadId } from "@onicode/contracts";

import { appAtomRegistry } from "../rpc/atomRegistry";
import { readEnvironmentScope } from "../state/session";
import { terminalEnvironment } from "../state/terminal";

/** Ends every terminal of a draft thread that is going away, so none keeps running unreachable. */
export function closeDraftThreadTerminals(environmentId: EnvironmentId, threadId: ThreadId): void {
  if (!readEnvironmentScope(environmentId, AuthTerminalOperateScope)) return;
  void runAtomCommand(
    appAtomRegistry,
    terminalEnvironment.close,
    { environmentId, input: { threadId, deleteHistory: true } },
    { reportFailure: false },
  );
}
