import { createRemoteDesktopEnvironmentAtoms } from "@onicode/client-runtime/state/remote-desktop";

import { connectionAtomRuntime } from "../connection/runtime";

// Mobile only views; it never mounts `host`, so nothing asks it to share a screen.
export const remoteDesktopEnvironment = createRemoteDesktopEnvironmentAtoms(
  connectionAtomRuntime,
  async () => {
    throw new Error("This device cannot share its screen.");
  },
);
