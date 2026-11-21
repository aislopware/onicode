import { availableScratchWorkspaceRoot } from "@onicode/client-runtime/operations/projects";
import { isScratchProject } from "@onicode/client-runtime/state/projects";
import type { EnvironmentProject } from "@onicode/client-runtime/state/shell";
import {
  type EnvironmentId,
  type ProjectId,
  resolveEnvironmentMachineKind,
} from "@onicode/contracts";

import type { EnvironmentOption } from "../components/BranchToolbar.logic";
import {
  deriveLogicalProjectKeyFromSettings,
  isPickableProjectMember,
  selectProjectGroupingSettings,
  selectRepositoryCounterpart,
} from "@onicode/client-runtime/state/project-grouping";
import { useProjects } from "../state/entities";
import { useEnvironments, usePrimaryEnvironmentId } from "../state/environments";
import { useClientSettings } from "./useSettings";

export function useLogicalProjectEnvironments(
  activeProject: EnvironmentProject | null,
  scratchDraft: boolean,
): readonly EnvironmentOption[] {
  const { environments } = useEnvironments();
  const allProjects = useProjects();
  const primaryEnvironmentId = usePrimaryEnvironmentId();
  const projectGroupingSettings = useClientSettings(selectProjectGroupingSettings);

  if (!activeProject) return [];
  const environmentById = new Map(
    environments.map((environment) => [environment.environmentId, environment]),
  );
  const envs: EnvironmentOption[] = [];
  const pushEnvironment = (environmentId: EnvironmentId, projectId: ProjectId | null) => {
    const environment = environmentById.get(environmentId) ?? null;
    envs.push({
      environmentId,
      projectId,
      label: environment?.label ?? environmentId,
      isPrimary: environmentId === primaryEnvironmentId,
      machine: resolveEnvironmentMachineKind(environment?.serverConfig ?? null),
    });
  };
  if (scratchDraft) {
    for (const environment of environments) {
      const scratchRoot = availableScratchWorkspaceRoot(
        environment.connection.phase,
        environment.serverConfig,
      );
      if (scratchRoot === null && environment.environmentId !== activeProject.environmentId)
        continue;
      const scratchProject =
        environment.environmentId === activeProject.environmentId
          ? activeProject
          : allProjects.find(
              (p) =>
                p.environmentId === environment.environmentId && isScratchProject(p, scratchRoot),
            );
      pushEnvironment(environment.environmentId, scratchProject?.id ?? null);
    }
  } else {
    const logicalKey = deriveLogicalProjectKeyFromSettings(activeProject, projectGroupingSettings);
    const membersByEnvironment = new Map<EnvironmentId, EnvironmentProject[]>();
    for (const p of allProjects) {
      if (deriveLogicalProjectKeyFromSettings(p, projectGroupingSettings) !== logicalKey) continue;
      const members = membersByEnvironment.get(p.environmentId);
      if (members) members.push(p);
      else membersByEnvironment.set(p.environmentId, [p]);
    }
    for (const [environmentId, members] of membersByEnvironment) {
      const open = members.filter(isPickableProjectMember);
      const project =
        environmentId === activeProject.environmentId
          ? activeProject
          : selectRepositoryCounterpart(activeProject, open.length > 0 ? open : members);
      pushEnvironment(environmentId, project?.id ?? null);
    }
  }
  envs.sort((a, b) => {
    if (a.isPrimary !== b.isPrimary) return a.isPrimary ? -1 : 1;
    return a.label.localeCompare(b.label);
  });
  return envs;
}
