import { squashAtomCommandFailure } from "@onicode/client-runtime/state/runtime";
import {
  findProjectByPath,
  inferProjectTitleFromPath,
} from "@onicode/client-runtime/state/projects";
import { useAtomValue } from "@effect/atom-react";
import type { DesktopAppActivationRequest } from "@onicode/contracts";
import { Atom } from "effect/reactivity";
import { msg } from "gt-react";

import {
  handleDesktopAppActivationRequest,
  resolveLocalWorkspaceState,
} from "../desktopAppActivation";
import { m } from "../i18n/messages";
import { isLocalEnvironmentDisabled } from "../localEnvironment";
import { newProjectId } from "../lib/utils";
import { readProjects, waitForProject } from "../state/entities";
import { appAtomRegistry } from "../rpc/atomRegistry";
import {
  localEnvironmentDiscoverySettledAtom,
  localEnvironmentIdAtom,
} from "../state/primaryEnvironment";
import { projectEnvironment } from "../state/projects";
import { environmentPresentations } from "../state/presentation";
import { environmentShell } from "../state/shell";
import { useAtomCommand } from "../state/use-atom-command";
import { useOrchestrationCommand } from "../state/use-orchestration-command";
import { reportProjectFailure } from "./useProjectGroupUpdate";
import { useNewThreadHandler } from "./useHandleNewThread";

let localEnvironmentOff: boolean | undefined;
// Read per open, not per render: one drop opens its folders across several awaits.
const localWorkspaceAtom = Atom.make((get) => {
  // A sync IPC read; the setting only changes across a relaunch.
  localEnvironmentOff ??= isLocalEnvironmentDisabled();
  const environmentId = get(localEnvironmentIdAtom);
  const environment =
    environmentId === null ? null : get(environmentPresentations.presentationAtom(environmentId));
  const state = resolveLocalWorkspaceState({
    environmentId,
    environment,
    shellStatus: environmentId === null ? null : get(environmentShell.statusAtom(environmentId)),
    discoverySettled: get(localEnvironmentDiscoverySettledAtom),
    localEnvironmentOff,
  });
  const serverConfig = environment?.serverConfig ?? null;
  return {
    state,
    target:
      state === "ready" && environmentId !== null && serverConfig !== null
        ? { environmentId, platform: serverConfig.environment.platform.os }
        : null,
  };
}).pipe(Atom.withLabel("web-local-workspace"));

/**
 * Opens a folder on this computer as a project in this computer's environment: an
 * existing project opens (and unarchives), a new one is added. Used by `onicode <dir>`
 * activations and folders dropped on the sidebar. See `resolveLocalWorkspaceState` for `state`.
 */
export function useOpenLocalWorkspace() {
  const { state } = useAtomValue(localWorkspaceAtom);
  const createProject = useAtomCommand(projectEnvironment.create, { reportFailure: false });
  const updateProject = useOrchestrationCommand(projectEnvironment.update, {
    label: "project unarchive",
  });
  const openThread = useNewThreadHandler();

  const open = (request: DesktopAppActivationRequest) =>
    handleDesktopAppActivationRequest(request, {
      getTarget: () => appAtomRegistry.get(localWorkspaceAtom).target,
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
