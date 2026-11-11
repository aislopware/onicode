import { createTailnetEnvironmentAtoms } from "@onicode/client-runtime/state/tailnet";

import { connectionAtomRuntime } from "../connection/runtime";

export const tailnetEnvironment = createTailnetEnvironmentAtoms(connectionAtomRuntime);
