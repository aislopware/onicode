import { scopeThreadRef } from "@onicode/client-runtime/environment";
import {
  resolveSideThreadOpenTarget,
  type SideThreadOpenIntent,
} from "@onicode/client-runtime/side-thread";
import type {
  OrchestrationV2ThreadShell,
  ScopedThreadRef,
  SideThreadOpenTarget,
} from "@onicode/contracts";

import type { ToolCallDiffTarget } from "../diffPanelStore";
import type { LinkedThreadTarget } from "@onicode/client-runtime/linked-thread";
import type { useNavigate } from "@tanstack/react-router";

import { getClientSettings } from "../hooks/useSettings";
import { linkedThreadRouteParams } from "../linkedThreadSurface";
import { rightPanelHostRef, useRightPanelStore } from "../rightPanelStore";

const SIDE_CHAT_PANE_SELECTOR = "[data-side-chat-pane]";
const FLOATING_PORTAL_SELECTOR = "[data-base-ui-portal]";

const matching = (root: Element, selector: string) => [
  ...(root.matches(selector) ? [root] : []),
  ...root.querySelectorAll(selector),
];

/**
 * Whether an event started in the side chat pane (`data-side-chat-pane`), including
 * portaled popovers and menus whose ARIA owner sits inside that pane.
 */
export function isInSideChatPane(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  if (target.closest(SIDE_CHAT_PANE_SELECTOR) !== null) return true;
  if (target.closest(FLOATING_PORTAL_SELECTOR) === null) return false;
  const doc = target.ownerDocument;
  // Runs on every key press in a popover: one walk of the document finds the only subtrees an
  // owner counts in, and each chain of ancestors is looked up in those with one selector.
  const roots = [
    ...doc.querySelectorAll(`${SIDE_CHAT_PANE_SELECTOR}, ${FLOATING_PORTAL_SELECTOR}`),
  ];
  const panes = roots.filter((root) => root.matches(SIDE_CHAT_PANE_SELECTOR));
  if (panes.length === 0) return false;
  const portals = roots.filter((root) => !root.matches(SIDE_CHAT_PANE_SELECTOR));
  const visited = new Set<Element>();
  const pending: Element[] = [target];
  while (pending.length > 0) {
    const named = new Map<string, Element[]>();
    for (
      let el: Element | null = pending.pop()!;
      el && el !== doc.body && el !== doc.documentElement;
      el = el.parentElement
    ) {
      if (visited.has(el)) break;
      visited.add(el);
      if (el.id) named.set(el.id, [...(named.get(el.id) ?? []), el]);
    }
    if (named.size === 0) continue;
    const selector = [...named.keys()]
      .map((id) => `[aria-owns~="${CSS.escape(id)}"], [aria-controls~="${CSS.escape(id)}"]`)
      .join(", ");
    if (panes.some((pane) => pane.matches(selector) || pane.querySelector(selector) !== null)) {
      return true;
    }
    for (const owner of portals.flatMap((portal) => matching(portal, selector))) {
      if (visited.has(owner)) continue;
      const ids = [owner.getAttribute("aria-owns"), owner.getAttribute("aria-controls")]
        .join(" ")
        .split(/[\t\n\f\r ]+/);
      // An owner inside the element it names would only lead back up this chain.
      if (ids.some((id) => named.get(id)?.some((el) => !el.contains(owner)))) pending.push(owner);
    }
  }
  return false;
}

/**
 * Opens an embedded side thread's tool diffs in its parent's diff panel and runs its shell commands
 * in the parent's terminal drawer, since the embedded view hides its own. Other panel actions reach
 * the parent through `hostRightPanel`; diff selection and the drawer are not panel state.
 */
export interface SideChatParentPanel {
  /** The thread whose panel shows the embedded one. */
  readonly hostRef: ScopedThreadRef;
  readonly openToolDiff: (target: ToolCallDiffTarget) => void;
  readonly runShellCommand: (command: string) => void;
}

/**
 * Where opening `threadRef` should route. A side chat shows as a tab of its parent, so this opens
 * that tab and returns the parent; any other thread returns `threadRef`.
 */
export function resolveThreadOpenRoute(
  threadRef: ScopedThreadRef,
  thread: Pick<OrchestrationV2ThreadShell, "lineage"> | null,
): ScopedThreadRef {
  const parentThreadId = thread?.lineage.sideChat === true ? thread.lineage.parentThreadId : null;
  if (parentThreadId === null) return threadRef;
  const parentRef = scopeThreadRef(threadRef.environmentId, parentThreadId);
  useRightPanelStore.getState().openThread(parentRef, threadRef.threadId);
  return parentRef;
}

/**
 * Whether `panelRef`'s right panel can show `threadRef` as a tab: a thread of the same environment
 * that is not the one the panel belongs to.
 */
function canOpenThreadInPanel(
  threadRef: ScopedThreadRef,
  panelRef: ScopedThreadRef | null,
): panelRef is ScopedThreadRef {
  if (panelRef === null) return false;
  const host = rightPanelHostRef(panelRef);
  return host.environmentId === threadRef.environmentId && host.threadId !== threadRef.threadId;
}

/**
 * Opens a side chat or subagent where the click asked: as a tab beside `panelRef`, the thread the
 * click happened in, or on the main screen through `navigate`. Returns where it went.
 */
export function openSideThread(input: {
  readonly threadRef: ScopedThreadRef;
  readonly panelRef: ScopedThreadRef | null;
  readonly intent: SideThreadOpenIntent;
  readonly navigate: (threadRef: ScopedThreadRef) => void;
}): SideThreadOpenTarget {
  const { threadRef, panelRef } = input;
  const target = resolveSideThreadOpenTarget({
    preference: getClientSettings().sideThreadOpenTarget,
    intent: input.intent,
    panelAvailable: canOpenThreadInPanel(threadRef, panelRef),
  });
  if (target === "panel" && panelRef !== null) {
    useRightPanelStore.getState().openThread(panelRef, threadRef.threadId);
  } else {
    input.navigate(threadRef);
  }
  return target;
}

/**
 * Opens the read-only view of a linked environment's thread where the click asked: as a tab in
 * `panelRef`'s right panel, or on the main screen, which is also where it goes without a panel.
 */
export function openLinkedThreadRead(input: {
  readonly panelRef: ScopedThreadRef | null;
  readonly target: LinkedThreadTarget;
  readonly intent: SideThreadOpenIntent;
  readonly navigate: ReturnType<typeof useNavigate>;
}) {
  const { panelRef, target } = input;
  const place = resolveSideThreadOpenTarget({
    preference: getClientSettings().sideThreadOpenTarget,
    intent: input.intent,
    panelAvailable: panelRef !== null,
  });
  if (place === "panel" && panelRef !== null) {
    useRightPanelStore.getState().openLinkedThread(panelRef, target);
    return;
  }
  void input.navigate({
    to: "/linked/$viaEnvironmentId/$linkedEnvironmentId/$linkedThreadId",
    params: linkedThreadRouteParams(target),
  });
}
