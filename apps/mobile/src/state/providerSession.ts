import {
  createDraftActivityReporter,
  createProviderSessionEnvironmentAtoms,
} from "@onicode/client-runtime/state/provider-session";
import { runAtomCommand } from "@onicode/client-runtime/state/runtime";

import { connectionAtomRuntime } from "../connection/runtime";
import { appAtomRegistry } from "./atom-registry";

export const providerSessionEnvironment =
  createProviderSessionEnvironmentAtoms(connectionAtomRuntime);

/** Tells the thread's provider that the user is typing its next prompt. Carries no text. */
export const reportDraftActivity = createDraftActivityReporter((ref) => {
  void runAtomCommand(
    appAtomRegistry,
    providerSessionEnvironment.draftActivity,
    { environmentId: ref.environmentId, input: { threadId: ref.threadId } },
    { reportFailure: false },
  );
});
