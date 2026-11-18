import { useNavigate, useRouter, type AnyRouter } from "@tanstack/react-router";

import { withoutAssistantCitationHash } from "../../lib/assistantCitationNavigation";
import { resolveThreadRouteTarget, type ThreadRouteTarget } from "../../threadRoutes";

/** Settings and Usage open as modals over the last main-app view. */
export function isAppModalPage(pathname: string) {
  return pathname === "/settings" || pathname.startsWith("/settings/") || pathname === "/usage";
}

export function isSidebarUtilityPage(pathname: string) {
  return (
    pathname.startsWith("/projects/") ||
    pathname === "/pull-requests" ||
    pathname === "/issues" ||
    pathname.startsWith("/remote-desktop/")
  );
}

let mainAppHref: string | null = null;
let appModalReturnHref: string | null = null;
let appModalReturnHistoryIndex: number | null = null;
let appModalReturnThreadTarget: ThreadRouteTarget | null = null;
let initialHistoryIndex = 0;
let mainAppLocationVersion = 0;
let appModalReturnsFocus = false;

/** Changes each time a page other than Settings or Usage resolves. */
export const getMainAppLocationVersion = () => mainAppLocationVersion;

/** Remembers where Settings, Usage, and the utility pages lead back to. */
export function startMainAppLocationTracking(router: AnyRouter): () => void {
  initialHistoryIndex = router.state.location.state.__TSR_index;
  const record = () => {
    const { pathname } = router.state.location;
    if (isAppModalPage(pathname)) {
      appModalReturnsFocus = false;
      return;
    }
    mainAppLocationVersion++;
    const href = withoutAssistantCitationHash(router.state.location.href);
    appModalReturnHref = href;
    // Only an entry that is exactly the return location can be stepped back to.
    appModalReturnHistoryIndex =
      href === router.state.location.href ? router.state.location.state.__TSR_index : null;
    appModalReturnThreadTarget = resolveThreadRouteTarget(
      router.state.matches.at(-1)?.params ?? {},
    );
    if (!isSidebarUtilityPage(pathname)) mainAppHref = href;
  };
  record();
  return router.subscribe("onResolved", record);
}

/** The thread Settings or Usage opened over, for views mounted while the modal is open. */
export function getAppModalReturnThreadTarget() {
  return appModalReturnThreadTarget;
}

export function useNavigateToMainApp() {
  const navigate = useNavigate();
  return () => navigate({ href: mainAppHref ?? "/" });
}

/** Set when Settings or Usage closes and returns focus to its opener itself; cleared on the next open. */
export const markAppModalReturnsFocus = () => {
  appModalReturnsFocus = true;
};
export const isAppModalReturningFocus = () => appModalReturnsFocus;

/** Leaves Settings or Usage for the page that opened it. False when a newer navigation already left. */
export function useCloseAppModal() {
  const navigate = useNavigate();
  const router = useRouter();
  return () => {
    // The modal closes after its exit animation. A navigation made meanwhile has already left
    // it and may still be loading; returning now would replace it.
    if (!isAppModalPage(router.history.location.pathname)) return false;
    const href = appModalReturnHref ?? mainAppHref ?? "/";
    const returnIndex = appModalReturnHistoryIndex ?? 0;
    const steps = router.state.location.state.__TSR_index - returnIndex;
    // Stepping back past this document's first entry reloads the page. Written as a negated
    // comparison so a missing (NaN) history index also falls back to client navigation.
    if (!(returnIndex > initialHistoryIndex && steps > 0)) {
      void navigate({ href, replace: true });
      return true;
    }
    // Go back to the entry before the return page and push it again: only a push drops the
    // modal's entries from forward history, and reusing the old entry avoids a duplicate stop.
    const unsubscribe = router.history.subscribe(() => {
      unsubscribe();
      void navigate({ href });
    });
    router.history.go(-(steps + 1));
    return true;
  };
}
