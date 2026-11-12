import type { LiveActivity } from "expo-widgets";

import type { AgentActivityProps } from "../../widgets/AgentActivity";

export function getAgentLiveActivities(): Array<LiveActivity<AgentActivityProps>> {
  return [];
}

export function observeAgentLiveActivityStartToken(_onToken: (token: string) => void): () => void {
  return () => undefined;
}
