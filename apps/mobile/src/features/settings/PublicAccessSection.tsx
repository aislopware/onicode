import { useAtomValue } from "@effect/atom-react";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@onicode/client-runtime/state/runtime";
import { errorDisplayMessage } from "@onicode/client-runtime/errors";
import {
  AuthAccessWriteScope,
  type EnvironmentId,
  type ServerSettingsPatch,
} from "@onicode/contracts";
import { useGT, useMessages } from "gt-react-native";
import { useState } from "react";
import { View } from "react-native";

import { AppText as Text } from "../../components/ui/AppText";
import {
  publicAccessStatusText,
  publicAccessSwitchDenial,
} from "@onicode/client-runtime/public-access-status";
import { usePublicAccess } from "../agent-access/usePublicAccess";
import { environmentPresentations } from "../../state/presentation";
import { serverEnvironment } from "../../state/server";
import { useEnvironmentScope } from "../../state/session";
import { useAtomCommand } from "../../state/use-atom-command";
import { SettingsSection } from "./components/SettingsSection";
import { SettingsSwitchRow } from "./components/SettingsSwitchRow";

/** Changes the public-access settings, and says why a change failed. */
export function usePublicAccessWriter(environmentId: EnvironmentId) {
  const m = useMessages();
  const gt = useGT();
  const updateSettings = useAtomCommand(serverEnvironment.updateSettings, {
    label: "public access",
    reportFailure: false,
  });
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const write = async (patch: ServerSettingsPatch) => {
    setPending(true);
    setError(null);
    const result = await updateSettings({ environmentId, input: { patch } });
    setPending(false);
    if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
      const cause = squashAtomCommandFailure(result);
      setError(
        (cause instanceof Error ? errorDisplayMessage(cause, m) : null) ??
          gt("The setting could not be saved. Try again."),
      );
    }
  };
  return { write, pending, error };
}

/**
 * What this environment publishes to the internet through Tailscale Funnel. Each route group has
 * its own switch; nothing else of the API is ever public.
 */
export function PublicAccessSection({ environmentId }: { readonly environmentId: EnvironmentId }) {
  const gt = useGT();
  const m = useMessages();
  const { webhooks, agents, status } = usePublicAccess(environmentId);
  const canChange = useEnvironmentScope(environmentId, AuthAccessWriteScope);
  const policy = useAtomValue(
    environmentPresentations.presentationAtom(environmentId),
    (presentation) => presentation?.serverConfig?.managedPolicy,
  );
  const webhooksDenial = publicAccessSwitchDenial(policy, "publicWebhooksEnabled", webhooks, m);
  const agentsDenial = publicAccessSwitchDenial(policy, "publicAgentsEnabled", agents, m);
  const { write, pending, error } = usePublicAccessWriter(environmentId);
  const statusText = status === null ? null : publicAccessStatusText(status, m);
  return (
    <SettingsSection
      title={gt("Public access")}
      footer={gt(
        "Published through Tailscale Funnel. The tailnet needs HTTPS certificates and the funnel node attribute.",
      )}
    >
      <SettingsSwitchRow
        label={gt("Public webhooks")}
        subtitle={
          webhooksDenial ??
          gt(
            "Lets senders on the internet, such as GitHub, reach the webhook addresses of scheduled tasks and workflows. Off, they work only on your network and tailnet.",
          )
        }
        value={webhooks}
        disabled={pending || !canChange || webhooksDenial !== undefined}
        onValueChange={(publicWebhooksEnabled) => void write({ publicWebhooksEnabled })}
      />
      <SettingsSwitchRow
        label={gt("Public agents")}
        subtitle={
          agentsDenial ??
          gt(
            "Lets cloud apps, such as ChatGPT and Claude, reach /mcp and its sign-in. Every app still needs your approval.",
          )
        }
        value={agents}
        disabled={pending || !canChange || agentsDenial !== undefined}
        onValueChange={(publicAgentsEnabled) => void write({ publicAgentsEnabled })}
      />
      {statusText !== null || error !== null ? (
        <View className="gap-1 border-t border-border-subtle px-4 py-3">
          {statusText !== null ? (
            <Text
              selectable
              className={
                status?._tag === "failed"
                  ? "text-sm text-danger-foreground"
                  : "text-sm text-foreground-muted"
              }
            >
              {statusText}
            </Text>
          ) : null}
          {error !== null ? (
            <Text selectable accessibilityRole="alert" className="text-sm text-danger-foreground">
              {error}
            </Text>
          ) : null}
        </View>
      ) : null}
    </SettingsSection>
  );
}
