import { useAtomValue } from "@effect/atom-react";
import { scopeThreadRef } from "@onicode/client-runtime/environment";
import type { AssetResource, EnvironmentId, ThreadId } from "@onicode/contracts";
import { useMemo } from "react";

import { useAssetUrlState, useRefreshAssetUrl } from "../../state/assets";
import { environmentThreadDetails } from "../../state/threads";
import { NO_WORKSPACE_MUTATION_ID_ATOM } from "../threads/workspaceMutationIdContext";
import {
  isAbsolutePath,
  isAudioPreviewFile,
  isVideoPreviewFile,
  resolveWorkspaceFilePath,
  tracksWorkspaceMutations,
} from "./filePath";

export function useWorkspaceFileAssetUrlState(props: {
  readonly cwd: string | null;
  readonly environmentId: EnvironmentId | null;
  readonly relativePath: string | null;
  readonly threadId: ThreadId | null;
  readonly draftCwd?: string | null;
}) {
  const absolutePath =
    props.cwd !== null && props.relativePath !== null
      ? resolveWorkspaceFilePath(props.cwd, props.relativePath)
      : null;

  const relativePath = props.relativePath;
  const draftCwd = props.draftCwd ?? null;
  const resource = useMemo<AssetResource | null>(() => {
    if (absolutePath === null || relativePath === null) return null;
    if (props.threadId !== null) {
      return {
        _tag:
          isVideoPreviewFile(absolutePath) ||
          isAudioPreviewFile(absolutePath) ||
          isAbsolutePath(relativePath)
            ? "media-file"
            : "workspace-file",
        threadId: props.threadId,
        path: absolutePath,
      };
    }
    if (draftCwd === null) return null;
    return { _tag: "draft-workspace-file", cwd: draftCwd, path: relativePath };
  }, [absolutePath, relativePath, props.threadId, draftCwd]);
  // A file outside the root is granted by its inode, so a replaced image needs a new grant.
  const workspaceMutationId = useAtomValue(
    absolutePath !== null &&
      tracksWorkspaceMutations(absolutePath) &&
      props.environmentId !== null &&
      props.threadId !== null
      ? environmentThreadDetails.workspaceMutationIdAtom(
          scopeThreadRef(props.environmentId, props.threadId),
        )
      : NO_WORKSPACE_MUTATION_ID_ATOM,
  );
  const state = useAssetUrlState(props.environmentId, resource, workspaceMutationId);
  const refresh = useRefreshAssetUrl(props.environmentId, resource);
  return { ...state, resource, refresh, workspaceMutationId };
}
