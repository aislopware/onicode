import { useAtomValue } from "@effect/atom-react";
import { resolveProviderInstanceDisplayName } from "@onicode/client-runtime/state/provider-instance-display";
import type { EnvironmentId, ProviderInstanceId, ServerSettings } from "@onicode/contracts";
import { View } from "react-native";

import { ProviderIcon } from "../../../components/ProviderIcon";
import { serverEnvironment } from "../../../state/server";
import { SettingsSwitchRow } from "./SettingsSwitchRow";

/** One switch per enabled provider instance, so two accounts of one provider differ. */
export function ComputerProviderRows(props: {
  readonly environmentId: EnvironmentId;
  readonly access: ServerSettings["agentComputerAccessProviders"];
  readonly disabled: boolean;
  readonly onChange: (instanceId: ProviderInstanceId, value: boolean) => void;
}) {
  const config = useAtomValue(serverEnvironment.configValueAtom(props.environmentId));
  return (config?.providers ?? [])
    .filter((provider) => provider.enabled)
    .map((provider) => (
      <View key={provider.instanceId} className="border-t border-border-subtle">
        <SettingsSwitchRow
          leading={<ProviderIcon provider={provider.driver} iconUrl={provider.iconUrl} size={18} />}
          label={resolveProviderInstanceDisplayName(provider)}
          value={props.access[provider.instanceId] !== false}
          disabled={props.disabled}
          onValueChange={(value) => props.onChange(provider.instanceId, value)}
        />
      </View>
    ));
}
