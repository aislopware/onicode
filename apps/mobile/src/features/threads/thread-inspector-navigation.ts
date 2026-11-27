import { StackActions, useNavigation } from "@react-navigation/native";
import { createContext, use } from "react";

import { resolveMobileSideThreadPresentation } from "@onicode/client-runtime/side-thread";

import { useWorkspacePaneState } from "../layout/AdaptiveWorkspaceLayout";
import { isNearestThreadRoute } from "./return-to-thread";

export type ThreadInspectorMode = "route" | "git" | "files" | "preview";

export type ThreadInspectorTarget = {
  /** "linkedThread" is a thread on a linked environment, read through `linked.viaEnvironmentId`. */
  readonly type: "thread" | "linkedThread" | "device" | "localPreview";
  readonly environmentId: string;
  readonly threadId: string;
  readonly linked?: {
    readonly viaEnvironmentId: string;
    readonly label: string;
    readonly title?: string;
  };
  readonly deviceKey?: string;
  /** The environment's localhost page a "localPreview" shows. */
  readonly url?: string;
};

export const ThreadInspectorNavigationContext = createContext<
  ((target: ThreadInspectorTarget) => void) | null
>(null);

export function useOpenThreadInspector() {
  const navigation = useNavigation();
  const { fileInspector } = useWorkspacePaneState();
  const openInInspector = use(ThreadInspectorNavigationContext);
  return (
    source: { readonly environmentId: string; readonly threadId: string },
    target: ThreadInspectorTarget,
  ) => {
    const presentation = resolveMobileSideThreadPresentation({
      inspectorAvailable: fileInspector.supported,
    });
    if (presentation === "full-screen") return false;
    if (openInInspector) openInInspector(target);
    else {
      if (!isNearestThreadRoute(navigation.getState()?.routes, source)) return false;
      navigation.dispatch(
        StackActions.popTo("Thread", { ...source, inspector: target }, { merge: true }),
      );
    }
    return true;
  };
}
