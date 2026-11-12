import { addPushToStartTokenListener } from "expo-widgets";

import AgentActivity from "../../widgets/AgentActivity";

export function getAgentLiveActivities() {
  AgentActivity.dismissEnded();
  return AgentActivity.getInstances();
}

export function observeAgentLiveActivityStartToken(onToken: (token: string) => void): () => void {
  const subscription = addPushToStartTokenListener((event) =>
    onToken(event.activityPushToStartToken),
  );
  return () => subscription.remove();
}
