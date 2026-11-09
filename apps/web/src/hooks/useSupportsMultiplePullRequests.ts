import type { EnvironmentId } from "@onicode/contracts";
import { useServerConfigs } from "~/state/entities";

export function useSupportsMultiplePullRequests(environmentId: EnvironmentId | null): boolean {
  const configs = useServerConfigs();
  return environmentId !== null && configs.get(environmentId) !== undefined;
}
