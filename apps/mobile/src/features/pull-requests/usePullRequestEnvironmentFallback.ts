import { useFocusEffect, useNavigation } from "@react-navigation/native";
import { withProjectChangeRequestHost } from "@onicode/client-runtime/pull-request-link-input";
import { useCallback } from "react";

import { useProjects } from "../../state/entities";
import { useConnectionsReady, useWorkspaceEnvironments } from "../../state/workspace";
import {
  pullRequestFallbackEnvironment,
  type PullRequestEnvironmentTarget,
} from "./pullRequestEnvironmentFallback";

/** Returns the params to read with: a missing host is filled from the first render. */
export function usePullRequestEnvironmentFallback<Params extends PullRequestEnvironmentTarget>(
  params: Params,
): Params {
  const navigation = useNavigation();
  const projects = useProjects();
  const isReady = useConnectionsReady();
  const environments = useWorkspaceEnvironments();
  const fallback = isReady ? pullRequestFallbackEnvironment(params, projects, environments) : null;
  // A link that left the host out is the change request a row or a link with the host opens.
  const own = projects.find(
    (project) => project.environmentId === params.environmentId && project.id === params.projectId,
  );
  const resolved = withProjectChangeRequestHost(params, own?.repositoryIdentity);
  const missingHost = params.host === undefined ? resolved.host : undefined;
  // Only while focused: a screen kept as a placeholder throws on any navigation action.
  useFocusEffect(
    useCallback(() => {
      if (fallback) navigation.setParams(fallback);
      else if (missingHost !== undefined) navigation.setParams({ host: missingHost });
    }, [fallback, missingHost, navigation]),
  );
  return resolved;
}
