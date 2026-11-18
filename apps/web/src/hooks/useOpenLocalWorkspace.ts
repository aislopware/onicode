import { squashAtomCommandFailure } from "@onicode/client-runtime/state/runtime";
import {
  findProjectByPath,
  inferProjectTitleFromPath,
} from "@onicode/client-runtime/state/projects";
import { useAtomValue } from "@effect/atom-react";
import type { DesktopAppActivationRequest } from "@onicode/contracts";
import { msg } from "gt-react";
import { useState } from "react";

import {
  handleDesktopAppActivationRequest,
  resolveLocalWorkspaceState,
} from "../desktopAppActivation";
import { m } from "../i18n/messages";
import { isLocalEnvironmentDisabled } from "../localEnvironment";
import { newProjectId } from "../lib/utils";
import { readProjects, waitForProject } from "../state/entities";
import { useEnvironment } from "../state/environments";
import {
  localEnvironmentDiscoverySettledAtom,
  localEnvironmentIdAtom,
} from "../state/primaryEnvironment";
import { projectEnvironment } from "../state/projects";
import { useEnvironmentShellCaughtUp } from "../state/shell";
import { useAtomCommand } from "../state/use-atom-command";
import { useOrchestrationCommand } from "../state/use-orchestration-command";
import { reportProjectFailure } from "./useProjectGroupUpdate";
import { useNewThreadHandler } from "./useHandleNewThread";

/**
 * Opens a folder on this computer as a project in this computer's environment: an
 * existing project opens (and unarchives), a new one is added. Used by `onicode <dir>`
 * activations and folders dropped on the sidebar. See `resolveLocalWorkspaceState` for `state`.
 */
export function useOpenLocalWorkspace() {
  const localEnvironmentId = useAtomValue(localEnvironmentIdAtom);
  const localEnvironment = useEnvironment(localEnvironmentId);
  // A sync IPC read; the setting only changes across a relaunch.
  const [localEnvironmentOff] = useState(isLocalEnvironmentDisabled);
  const createProject = useAtomCommand(projectEnvironment.create, { reportFailure: false });
  const updateProject = useOrchestrationCommand(projectEnvironment.update, {
    label: "project unarchive",
  });
  const openThread = useNewThreadHandler();
  const state = resolveLocalWorkspaceState({
    environmentId: localEnvironmentId,
    environment: localEnvironment,
    shellCaughtUp: useEnvironmentShellCaughtUp(localEnvironmentId),
    discoverySettled: useAtomValue(localEnvironmentDiscoverySettledAtom),
    localEnvironmentOff,
  });

  const open = (request: DesktopAppActivationRequest) =>
    handleDesktopAppActivationRequest(request, {
      getTarget: () => {
        if (
          localEnvironment?.connection.phase !== "connected" ||
          localEnvironment.serverConfig === null
        ) {
          return null;
        }
        return {
          environmentId: localEnvironment.environmentId,
          platform: localEnvironment.serverConfig.environment.platform.os,
        };
      },
      findProject: (environmentId, workspaceRoot) =>
        findProjectByPath(
          readProjects().filter((project) => project.environmentId === environmentId),
          workspaceRoot,
        ) ?? null,
      createProject: async (environmentId, workspaceRoot) => {
        const projectId = newProjectId();
        const result = await createProject({
          environmentId,
          input: {
            projectId,
            title: inferProjectTitleFromPath(workspaceRoot),
            workspaceRoot,
            createWorkspaceRootIfMissing: false,
            defaultModelSelection: null,
          },
        });
        if (result._tag === "Failure") {
          const error = squashAtomCommandFailure(result);
          throw error instanceof Error
            ? error
            : new Error(m(msg("Onicode could not add the project.")));
        }
        return projectId;
      },
      waitForProject: async (projectRef) => {
        await waitForProject(projectRef);
      },
      openThread: (projectRef) => {
        const project = readProjects().find(
          (candidate) =>
            candidate.environmentId === projectRef.environmentId &&
            candidate.id === projectRef.projectId,
        );
        // Opening an archived project's folder again brings the project back.
        if (project?.archivedAt != null) {
          void updateProject({
            environmentId: projectRef.environmentId,
            input: { projectId: projectRef.projectId, archived: false },
          }).then((result) => reportProjectFailure(m(msg("Failed to unarchive project")), result));
        }
        return openThread(projectRef, { acceptOpenedMeanwhile: true });
      },
    });

  return { state, open };
}
