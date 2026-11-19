import type { UnifiedSettings } from "@onicode/contracts";
import { useGT } from "gt-react";

import type { ProviderInstanceEntry } from "../../providerInstances";
import { useEnvironmentProviders, useEnvironments } from "../../state/environments";
import { createScopedModelDisabledReason } from "./scopedModelAvailability.logic";
import { useSettingsScope } from "./SettingsScopeContext";

export function useScopedModelDisabledReason(
  settings: UnifiedSettings,
  entries: readonly ProviderInstanceEntry[],
  textGeneration = false,
) {
  const gt = useGT();
  const { targets } = useSettingsScope();
  const { environments } = useEnvironments();
  const providersByEnvironment = useEnvironmentProviders();
  return createScopedModelDisabledReason({
    settings,
    entries,
    targets,
    environments,
    providersByEnvironment,
    textGeneration,
    unavailableMessage: (environment) =>
      environment === undefined
        ? gt(
            "This model is unavailable on a selected environment. Select that environment to choose its model separately.",
          )
        : gt(
            "This model is unavailable on {environment}. Select that environment to choose its model separately.",
            { environment },
          ),
  });
}
