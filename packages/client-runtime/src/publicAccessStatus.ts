import {
  isManagedSettingPath,
  type ManagedPolicySummary,
  type PublicAccessFailure,
  type PublicAccessStatus,
} from "@onicode/contracts";
import { mFallback, msg } from "gt-i18n";
import type { MFunctionType } from "gt-i18n/types";

import { managedCapabilityDenial } from "./contractLabels.ts";

const FAILURE_MESSAGES: Record<PublicAccessFailure, string> = {
  "not-installed": msg("Tailscale is not installed on this machine."),
  "not-running": msg("Tailscale is not running on this machine."),
  "not-logged-in": msg("This machine is not logged in to Tailscale."),
  "https-disabled": msg(
    "HTTPS certificates are off for this tailnet. Turn them on in the Tailscale admin console under DNS.",
  ),
  "funnel-not-allowed": msg(
    'Your tailnet policy does not allow Funnel on this machine. Add the "funnel" node attribute for it in the Tailscale admin console under Access controls.',
  ),
  "port-in-use": msg("Another Tailscale Serve or Funnel route already uses the public port."),
  "permission-denied": msg(
    "Tailscale refused to change Funnel. Run `sudo tailscale set --operator=$USER` once.",
  ),
  "policy-denied": msg("Your organization does not allow connections from other devices."),
  unknown: msg("Tailscale could not turn Funnel on."),
};

/** What Funnel is doing for an environment's public access, or null while it is off. */
export function publicAccessStatusText(
  status: PublicAccessStatus,
  resolve: MFunctionType = mFallback,
): string | null {
  switch (status._tag) {
    case "off":
      return null;
    case "starting":
      return resolve(msg("Starting Tailscale Funnel…"));
    case "on":
      return resolve(msg("Public at {origin}", { origin: status.origin }));
    case "failed":
      return status.reason === "policy-denied"
        ? resolve(FAILURE_MESSAGES[status.reason])
        : `${resolve(FAILURE_MESSAGES[status.reason])} ${resolve(
            msg("Onicode tries again every 30 seconds."),
          )}`;
  }
}

/**
 * Why a public access switch cannot change, or undefined when it can. A locked setting stays as it
 * is; a policy that refuses the capability still lets a switch that is on go off.
 */
export function publicAccessSwitchDenial(
  policy: ManagedPolicySummary | undefined,
  setting: "publicWebhooksEnabled" | "publicAgentsEnabled",
  on: boolean,
  resolve: MFunctionType = mFallback,
): string | undefined {
  if (policy !== undefined && isManagedSettingPath(policy, setting)) {
    return resolve(msg("Managed by {organization}.", { organization: policy.organization }));
  }
  if (on) return undefined;
  return (
    managedCapabilityDenial(policy, "remoteClients", resolve) ??
    (setting === "publicWebhooksEnabled"
      ? managedCapabilityDenial(policy, "webhookTasks", resolve)
      : undefined)
  );
}
