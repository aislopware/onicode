import { createProviderSessionEnvironmentAtoms } from "@onicode/client-runtime/state/provider-session";

import { connectionAtomRuntime } from "../connection/runtime";

export const providerSessionEnvironment =
  createProviderSessionEnvironmentAtoms(connectionAtomRuntime);
