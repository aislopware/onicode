import { createTailnetEnvironmentAtoms } from "@onicode/client-runtime/state/tailnet";
import { AuthAccessReadScope } from "@onicode/contracts";

import { connectionAtomRuntime } from "../connection/runtime";
import { useEnvironments } from "./environments";
import { useEnvironmentScope } from "./session";

export const tailnetEnvironment = createTailnetEnvironmentAtoms(connectionAtomRuntime);

/** The first connected environment that can list the devices on its machine's tailnet. */
export function useTailnetDevicesSource() {
  const { environments } = useEnvironments();
  const source =
    environments.find(
      (environment) =>
        environment.connection.phase === "connected" &&
        environment.serverConfig?.environment.capabilities.tailnetDevices === true,
    ) ?? null;
  const canRead = useEnvironmentScope(source?.environmentId ?? null, AuthAccessReadScope);
  return canRead ? source : null;
}
