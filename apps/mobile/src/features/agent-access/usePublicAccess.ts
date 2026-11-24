import { useAtomValue } from "@effect/atom-react";
import type { EnvironmentId } from "@onicode/contracts";

import { useEnvironmentQuery } from "../../state/query";
import { serverEnvironment } from "../../state/server";
import { publicMcpUrl } from "./agentAccess.logic";

/** An environment's two public-access settings, Funnel's status, and the public MCP URL. */
export function usePublicAccess(environmentId: EnvironmentId) {
  const settingsAtom = serverEnvironment.settingsValueAtom(environmentId);
  const webhooks = useAtomValue(
    settingsAtom,
    (settings) => settings?.publicWebhooksEnabled === true,
  );
  const agents = useAtomValue(settingsAtom, (settings) => settings?.publicAgentsEnabled === true);
  const status =
    useEnvironmentQuery(serverEnvironment.publicAccessStatus({ environmentId, input: {} })).data ??
    null;
  return { webhooks, agents, status, mcpUrl: publicMcpUrl(agents, status) };
}
