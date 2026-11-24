import { AuthAccessWriteScope, type EnvironmentId } from "@onicode/contracts";
import { useGT, useMessages } from "gt-react";

import { useUpdateEnvironmentSettings } from "~/hooks/useSettings";
import { useManagedPolicy } from "~/state/managedPolicy";
import { useEnvironmentScope } from "~/state/session";
import { Switch } from "../ui/switch";
import { usePublicAccess } from "./publicAccess";
import {
  publicAccessStatusText,
  publicAccessSwitchDenial,
} from "@onicode/client-runtime/public-access-status";
import { SettingsRow, SettingsSection } from "./settingsLayout";
import { useSearchableSetting } from "./settingsSearch";

/**
 * What this environment publishes to the internet through Tailscale Funnel. Each route group has
 * its own switch; nothing else of the API is ever public.
 */
export function PublicAccessSettings({ environmentId }: { readonly environmentId: EnvironmentId }) {
  const gt = useGT();
  const m = useMessages();
  const { webhooks, agents, status } = usePublicAccess(environmentId);
  const canChange = useEnvironmentScope(environmentId, AuthAccessWriteScope);
  const updateSettings = useUpdateEnvironmentSettings(environmentId);
  const policy = useManagedPolicy(environmentId);
  const webhooksDenial = publicAccessSwitchDenial(policy, "publicWebhooksEnabled", webhooks, m);
  const agentsDenial = publicAccessSwitchDenial(policy, "publicAgentsEnabled", agents, m);
  const statusText = status === null ? null : publicAccessStatusText(status, m);
  const searchableSetting = useSearchableSetting();
  return (
    <SettingsSection {...searchableSetting("public-access")}>
      <SettingsRow
        title={gt("Public webhooks")}
        description={gt(
          "Lets senders on the internet, such as GitHub, reach the webhook addresses of scheduled tasks and workflows. Off, they work only on your network and tailnet.",
        )}
        managed={webhooksDenial}
        control={
          <Switch
            checked={webhooks}
            disabled={!canChange || webhooksDenial !== undefined}
            aria-label={gt("Public webhooks")}
            onCheckedChange={(checked) => void updateSettings({ publicWebhooksEnabled: checked })}
          />
        }
      />
      <SettingsRow
        title={gt("Public agents")}
        description={gt(
          "Lets cloud apps, such as ChatGPT and Claude, reach /mcp and its sign-in. Every app still needs your approval.",
        )}
        managed={agentsDenial}
        control={
          <Switch
            checked={agents}
            disabled={!canChange || agentsDenial !== undefined}
            aria-label={gt("Public agents")}
            onCheckedChange={(checked) => void updateSettings({ publicAgentsEnabled: checked })}
          />
        }
      />
      <p
        className={
          status?._tag === "failed"
            ? "px-3 py-2 text-xs text-destructive-foreground sm:px-4"
            : "px-3 py-2 text-xs text-muted-foreground sm:px-4"
        }
      >
        {statusText ??
          gt(
            "Published through Tailscale Funnel. The tailnet needs HTTPS certificates and the funnel node attribute.",
          )}
      </p>
    </SettingsSection>
  );
}
