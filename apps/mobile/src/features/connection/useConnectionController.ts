import type { EnvironmentId } from "@onicode/contracts";

import { environmentCatalog } from "../../connection/catalog";
import { updateBearerConnection } from "../../connection/onboarding";
import { useAtomCommand } from "../../state/use-atom-command";

export function useConnectionController() {
  const updateBearer = useAtomCommand(updateBearerConnection, { reportFailure: false });
  const removeEnvironmentMutation = useAtomCommand(environmentCatalog.remove, "environment remove");
  const retryEnvironmentMutation = useAtomCommand(environmentCatalog.retryNow, "environment retry");
  const setEnvironmentEnabledMutation = useAtomCommand(
    environmentCatalog.setEnabled,
    "environment toggle",
  );

  const removeEnvironment = (environmentId: EnvironmentId) =>
    removeEnvironmentMutation(environmentId);
  const retryEnvironment = (environmentId: EnvironmentId) =>
    retryEnvironmentMutation(environmentId);
  const setEnvironmentEnabled = (environmentId: EnvironmentId, enabled: boolean) =>
    setEnvironmentEnabledMutation({ environmentId, enabled });
  const updateEnvironment = (
    environmentId: EnvironmentId,
    updates: { readonly label: string; readonly displayUrl: string },
  ) =>
    updateBearer({
      environmentId,
      label: updates.label,
      httpBaseUrl: updates.displayUrl,
    });

  return {
    removeEnvironment,
    retryEnvironment,
    setEnvironmentEnabled,
    updateEnvironment,
  };
}
