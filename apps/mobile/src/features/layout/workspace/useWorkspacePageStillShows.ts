import { useNavigation, useRoute } from "@react-navigation/native";
import { useLayoutEffect, useRef } from "react";

import { workspacePageStillShows } from "./workspace-detail-selection";

/**
 * Asks whether the calling page still shows what it did, for a late result to check before it
 * navigates. A thread preview's own navigator keeps answering with its last state after the
 * preview unmounts, so a page that unmounted never counts.
 */
export function useWorkspacePageStillShows() {
  const navigation = useNavigation();
  const route = useRoute();
  const mounted = useRef(true);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- tracks unmount within its commit, which a preview navigator's retained state does not show
  useLayoutEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  return () => mounted.current && workspacePageStillShows(navigation.getState(), route);
}
