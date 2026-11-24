import type { EnvironmentId } from "@onicode/contracts";

import { useEnvironmentSettings } from "~/hooks/useSettings";
import { useEnvironmentQuery } from "~/state/query";
import { serverEnvironment } from "~/state/server";

const selectPublicWebhooks = (settings: { readonly publicWebhooksEnabled: boolean }) =>
  settings.publicWebhooksEnabled;
const selectPublicAgents = (settings: { readonly publicAgentsEnabled: boolean }) =>
  settings.publicAgentsEnabled;

/**
 * An environment's public access: the two settings, Funnel's status, and the public MCP URL,
 * which exists only while public agents are on and Funnel answers.
 */
export function usePublicAccess(environmentId: EnvironmentId) {
  const webhooks = useEnvironmentSettings(environmentId, selectPublicWebhooks);
  const agents = useEnvironmentSettings(environmentId, selectPublicAgents);
  const status =
    useEnvironmentQuery(serverEnvironment.publicAccessStatus({ environmentId, input: {} })).data ??
    null;
  const mcpUrl = agents && status?._tag === "on" ? `${status.origin}/mcp` : null;
  return { webhooks, agents, status, mcpUrl };
}
