import type { AnyRouter } from "@tanstack/react-router";
import { isAppModalPage } from "../components/sidebar/mainAppLocation";
import { dismissContextMenu } from "../contextMenuFallback";
import { useRightPanelStore } from "../rightPanelStore";

const AREA_SELECTOR = "[data-workspace-area]";
const FOCUS_BOUNDARY_SELECTOR =
  '[role="dialog"], [role="alertdialog"], [aria-modal="true"], dialog[open], [data-slot$="-popup"]:not([data-slot="tooltip-popup"]):not([role="tooltip"])';
const FOCUSABLE_SELECTOR =
  'button:not(:disabled), a[href], input:not(:disabled), textarea:not(:disabled), select:not(:disabled), [contenteditable="true"], [tabindex]';

/** Props that make an element one stop in the F6 cycle; `order` sets its place in the cycle. */
export function workspaceAreaProps(name: string, order: number, label?: string) {
  return {
    "data-workspace-area": name,
    "data-workspace-order": order,
    ...(label ? { "aria-label": label, role: "region" } : {}),
    tabIndex: -1,
  } as const;
}

export interface WorkspaceFocusTarget {
  /** Left out only for a `returnTo` outside any area. */
  area?: string | undefined;
  /** Names of the areas around `area`, outermost first; a side chat nests the main chat's names. */
  within?: readonly string[];
  selector?: string;
  restore?: boolean;
  /** A control to return to while connected, else its replacement in `area` once that remounts. */
  returnTo?: HTMLElement;
}

function workspaceArea(element: Element): HTMLElement | null {
  const area = element.closest<HTMLElement>(AREA_SELECTOR);
  if (area) return area;
  const root = element.getRootNode();
  return root instanceof ShadowRoot ? workspaceArea(root.host) : null;
}

/** Names of the areas an element sits in, outermost first: a side chat's are `panel`, `composer`. */
function areaNames(element: Element): string[] {
  const area = workspaceArea(element);
  if (!area) return [];
  return [
    ...(area.parentElement ? areaNames(area.parentElement) : []),
    area.dataset.workspaceArea!,
  ];
}

const sameNames = (left: readonly string[], right: readonly string[]) =>
  left.length === right.length && left.every((name, index) => name === right[index]);

/** What finds `element` again: its area, or the same-named control once that area remounts. */
export function workspaceFocusTargetFor(element: HTMLElement): WorkspaceFocusTarget {
  const names = areaNames(element);
  return { area: names.at(-1), within: names.slice(0, -1), returnTo: element };
}

/** Whether `element` sits inside `host`'s shadow tree, as a file tree's focused row does. */
function insideShadowOf(host: Element, element: Element) {
  for (let root = element.getRootNode(); root instanceof ShadowRoot; root = root.host.getRootNode())
    if (root.host === host) return true;
  return false;
}

function isWorkspaceElementVisible(element: HTMLElement): boolean {
  if (
    !element.isConnected ||
    element.closest(
      '[hidden], [inert], [data-closed], [aria-hidden="true"], [data-chat-column-maximized-away="true"], [data-slot="sidebar"][data-state="collapsed"], [data-workspace-unavailable="true"]',
    )
  )
    return false;
  const style = getComputedStyle(element);
  if (style.display === "none" || style.visibility === "hidden") return false;
  const root = element.getRootNode();
  if (
    root instanceof ShadowRoot &&
    root.host instanceof HTMLElement &&
    !isWorkspaceElementVisible(root.host)
  )
    return false;
  return Array.from(element.getClientRects()).some((rect) => rect.width > 0 && rect.height > 0);
}

function scrollingAncestors(target: HTMLElement) {
  const scrollers: HTMLElement[] = [];
  for (let element = target.parentElement; element; element = element.parentElement) {
    const { overflowY } = getComputedStyle(element);
    if (
      (overflowY === "auto" || overflowY === "scroll") &&
      element.scrollHeight > element.clientHeight
    )
      scrollers.push(element);
  }
  return scrollers;
}

function isInScrollView(target: HTMLElement) {
  const rect = target.getBoundingClientRect();
  return scrollingAncestors(target).every((scroller) => {
    const box = scroller.getBoundingClientRect();
    return rect.bottom > box.top && rect.top < box.bottom;
  });
}

/**
 * Whether focus can go back to an element without surprising the user: it is still in a
 * visible workspace area (a closing drawer drops its area) and in view, so no list scrolls.
 */
export function canReturnFocusTo(element: HTMLElement) {
  return (
    workspaceArea(element) !== null && isWorkspaceElementVisible(element) && isInScrollView(element)
  );
}

/**
 * Brings a restored control into view, innermost scrolling list first.
 * scrollIntoView would also scroll overflow-hidden layout containers out of place.
 */
function revealInScrollers(target: HTMLElement) {
  for (const scroller of scrollingAncestors(target)) {
    const box = scroller.getBoundingClientRect();
    const rect = target.getBoundingClientRect();
    if (rect.top < box.top) scroller.scrollTop -= box.top - rect.top;
    else if (rect.bottom > box.bottom) scroller.scrollTop += rect.bottom - box.bottom;
  }
}

/** Keeps area focus local, like VS Code's layout service focusPart/visible-neighbour actions. */
export function createWorkspaceFocusController(doc: Document) {
  const lastFocused = new Map<string, HTMLElement>();
  let lastArea: string | undefined;
  // An area that took focus itself from the keyboard, or the non-tabbable default (the
  // conversation's scroller) standing in for it. Chromium's :focus-visible also matches a
  // clicked area after any later key press, so the ring and window return read this mark.
  let keyboardArea: HTMLElement | undefined;
  let previousTarget: EventTarget | null = null;
  const unmarkKeyboardArea = () => {
    keyboardArea?.removeAttribute("data-workspace-keyboard-focus");
    keyboardArea = undefined;
  };
  const standsInFor = (element: HTMLElement, area: HTMLElement) =>
    element === area ||
    (element.tabIndex < 0 && element.hasAttribute("data-workspace-focus-default"));
  const markKeyboardArea = (target: HTMLElement) => {
    if (!target.matches(":focus-visible")) return;
    keyboardArea = target;
    target.setAttribute("data-workspace-keyboard-focus", "");
  };
  const remember = (event: FocusEvent) => {
    if (event.target !== keyboardArea) unmarkKeyboardArea();
    // Window activation refocuses the same element; only focus arriving from elsewhere counts.
    const arrived = event.target !== previousTarget;
    previousTarget = event.target;
    if (!(event.target instanceof HTMLElement)) return;
    const area = workspaceArea(event.target);
    if (!area?.dataset.workspaceArea) return;
    if (arrived && standsInFor(event.target, area)) markKeyboardArea(event.target);
    lastArea = area.dataset.workspaceArea;
    const key = area.dataset.workspaceFocusKey ?? areaNames(area).join(" ");
    lastFocused.delete(key);
    // Custom file trees retarget focusin to their nonfocusable shadow host.
    const target = event.composedPath().find((element) => element instanceof HTMLElement);
    lastFocused.set(key, target instanceof HTMLElement ? target : event.target);
    // Surface history must not retain an unbounded number of detached panel trees.
    if (lastFocused.size > 64) lastFocused.delete(lastFocused.keys().next().value!);
  };
  doc.addEventListener("focusin", remember);

  const focusArea = (area: HTMLElement, restore = true, nested = true, returnTo?: HTMLElement) => {
    const key = area.dataset.workspaceFocusKey ?? areaNames(area).join(" ");
    const remembered = restore ? lastFocused.get(key) : undefined;
    // The page reports a file tree's shadow host as focused; the row inside it is remembered.
    const previous =
      returnTo && remembered && insideShadowOf(returnTo, remembered)
        ? remembered
        : (returnTo ?? remembered);
    const usable = (element: HTMLElement) =>
      isWorkspaceElementVisible(element) && !element.matches(":disabled");
    const belongs = (element: HTMLElement) => workspaceArea(element) === area && usable(element);
    // A side chat's message input sits in its own area inside the panel. Opening the
    // panel focuses it, but F6 visits that area on its own, so it moves past it here.
    // A nested timeline stands in only for its own area, never ahead of that input.
    const preferred = Array.from(
      area.querySelectorAll<HTMLElement>(
        '[data-workspace-focus-default], [data-sidebar-row][aria-current="page"], [data-settings-section][aria-current="page"], .xterm-helper-textarea, [contenteditable="true"]',
      ),
    ).find((element) =>
      nested && !standsInFor(element, area) ? usable(element) : belongs(element),
    );
    const fallback = Array.from(area.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).find(
      (element) => element.tabIndex >= 0 && belongs(element),
    );
    // Panel surfaces may unmount when hidden. Recover the same named control when
    // it mounts again, without sharing focus history between different surface tabs.
    const restored =
      previous && !previous.isConnected
        ? Array.from(area.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).find(
            (element) =>
              belongs(element) &&
              element.tagName === previous.tagName &&
              ((previous.id && element.id === previous.id) ||
                (previous.getAttribute("aria-label") &&
                  element.getAttribute("aria-label") === previous.getAttribute("aria-label")) ||
                (previous.matches("button") &&
                  previous.textContent?.trim() &&
                  element.textContent === previous.textContent)),
          )
        : undefined;
    const surface = area.querySelector<HTMLElement>("[data-right-panel-surface-content]");
    const surfaceControl = surface
      ? Array.from(surface.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).find(
          (element) => belongs(element) && element.tabIndex >= 0,
        )
      : undefined;
    // Moving the timeline to an old control would also stop it following a running turn,
    // so the conversation reuses a control only while it is still in view. A remembered
    // area is only the fallback it was then, so controls mounted since take precedence.
    const isConversation = area.dataset.workspaceArea === "conversation";
    const targets = new Set([
      previous &&
      previous !== area &&
      belongs(previous) &&
      (!isConversation || isInScrollView(previous))
        ? previous
        : undefined,
      isConversation ? undefined : restored,
      preferred,
      surfaceControl,
      isConversation ? undefined : fallback,
      area,
    ]);
    for (const target of targets) {
      if (!target) continue;
      target.focus({ preventScroll: true });
      const root = target.getRootNode();
      const active = root instanceof ShadowRoot ? root.activeElement : doc.activeElement;
      if (active === target || (active && target.contains(active))) {
        if (target !== area && !isConversation) revealInScrollers(target);
        // Focusing the already focused area fires no focusin, as when F6 has nowhere else to go.
        if (standsInFor(target, area)) markKeyboardArea(target);
        return true;
      }
    }
    return false;
  };
  const focusTarget = ({
    area: name,
    within,
    selector,
    restore = true,
    returnTo,
  }: WorkspaceFocusTarget) => {
    const ownArea = returnTo?.isConnected ? workspaceArea(returnTo) : null;
    if (returnTo?.isConnected && !ownArea) {
      if (!isWorkspaceElementVisible(returnTo) || returnTo.matches(":disabled")) return false;
      returnTo.focus({ preventScroll: true });
      return doc.activeElement === returnTo;
    }
    const areas = Array.from(doc.querySelectorAll<HTMLElement>(AREA_SELECTOR)).filter(
      isWorkspaceElementVisible,
    );
    const area = ownArea
      ? areas.find((element) => element === ownArea)
      : name === "content"
        ? (areas.find((element) => element.dataset.workspaceArea === "composer") ??
          areas.find(
            (element) =>
              element.dataset.workspaceArea === "pull-requests" || element.matches("main"),
          ) ??
          areas.find((element) => element.dataset.workspaceArea === "conversation") ??
          areas.find((element) => element.dataset.workspaceArea === "panel"))
        : areas.find(
            (element) =>
              element.dataset.workspaceArea === name &&
              (!within || sameNames(areaNames(element).slice(0, -1), within)),
          );
    if (!area) return false;
    const surface = area.querySelector("[data-right-panel-surface-content]");
    // A lazy surface's Suspense fallback is empty. Wait for its content instead
    // of claiming focus on the tab strip before the useful controls mount.
    if (surface && !surface.hasChildNodes()) return false;
    if (!selector && surface?.contains(doc.activeElement)) return true;
    if (selector) {
      const target = area.querySelector<HTMLElement>(selector);
      if (!target || !isWorkspaceElementVisible(target)) return false;
      target.focus({ preventScroll: true });
      return doc.activeElement === target;
    }
    return focusArea(area, restore, true, returnTo);
  };
  const focus = (name: string, restore = true, selector?: string) =>
    focusTarget({ area: name, restore, ...(selector ? { selector } : {}) });
  const move = (direction: 1 | -1, fromArea?: string, closingPalette = false) => {
    // HTML context menus close on blur, unlike modal dialogs and Base UI popups.
    dismissContextMenu(false);
    // A palette closes before running its command. Other popups keep their focus boundary.
    // Inline listboxes (such as Settings search) belong to their workspace area, while
    // portalled listboxes and menus can also supply a boundary without a popup data-slot.
    const activePopup = doc.activeElement?.closest<HTMLElement>('[role="menu"], [role="listbox"]');
    // Settings opens as a route modal with its own areas; navigation stays inside it.
    const appModal = Array.from(doc.querySelectorAll<HTMLElement>("[data-app-modal]")).find(
      isWorkspaceElementVisible,
    );
    const boundaries = Array.from(
      doc.querySelectorAll<HTMLElement>(FOCUS_BOUNDARY_SELECTOR),
    ).filter((boundary) => boundary !== appModal);
    if (activePopup && !activePopup.closest(AREA_SELECTOR)) boundaries.push(activePopup);
    // A modal keeps focus wherever it is; any other popup only while it holds focus,
    // so hover popups and notifications never swallow the key.
    if (
      boundaries.some(
        (boundary) =>
          !(closingPalette && boundary.closest("[data-command-palette]")) &&
          (boundary.matches('[aria-modal="true"]') || boundary.contains(doc.activeElement)) &&
          isWorkspaceElementVisible(boundary),
      )
    )
      return false;
    const areas = Array.from((appModal ?? doc).querySelectorAll<HTMLElement>(AREA_SELECTOR))
      .filter(isWorkspaceElementVisible)
      .sort(
        (left, right) => Number(left.dataset.workspaceOrder) - Number(right.dataset.workspaceOrder),
      );
    if (areas.length === 0) return false;
    const activeArea = doc.activeElement?.closest<HTMLElement>(AREA_SELECTOR);
    const currentId = fromArea ?? activeArea?.dataset.workspaceArea ?? lastArea;
    // A side chat nests areas with the main chat's names, so prefer the focused element.
    const activeIndex = fromArea || !activeArea ? -1 : areas.indexOf(activeArea);
    const current =
      activeIndex === -1
        ? areas.findIndex((area) => area.dataset.workspaceArea === currentId)
        : activeIndex;
    const index =
      current === -1
        ? direction === 1
          ? 0
          : areas.length - 1
        : (current + direction + areas.length) % areas.length;
    const area = areas[index]!;
    return focusArea(area, true, false);
  };

  // Focus that drops to the body because its element was hidden, made inert, disabled, or removed
  // returns to a remounted control, else its area, or the composer for a finished timeline prompt.
  // A side chat repeats the main chat's names, so both stay inside the chat that lost focus.
  const recover = (event: FocusEvent) => {
    const lost = event.target;
    if (!(lost instanceof HTMLElement) || event.relatedTarget) return;
    const names = areaNames(lost);
    const areaName = names.at(-1);
    const recoveryArea = areaName === "conversation" ? "composer" : areaName;
    const label = lost.getAttribute("aria-label");
    requestAnimationFrame(() => {
      if (
        doc.activeElement !== doc.body ||
        !doc.hasFocus() ||
        (isWorkspaceElementVisible(lost) && !lost.matches(":disabled"))
      )
        return;
      const remounted = Array.from(doc.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).find(
        (element) =>
          element.tagName === lost.tagName &&
          ((lost.id && element.id === lost.id) ||
            (label && element.getAttribute("aria-label") === label)) &&
          sameNames(areaNames(element), names) &&
          isWorkspaceElementVisible(element) &&
          !element.matches(":disabled"),
      );
      if (remounted) remounted.focus({ preventScroll: true });
      else if (!recoveryArea || !focusTarget({ area: recoveryArea, within: names.slice(0, -1) }))
        focus("content");
    });
  };
  doc.addEventListener("focusout", recover);

  // A request names a destination React has not committed yet, so it retries as the
  // DOM changes. User input cancels it, and it gives up after two seconds.
  let pending: WorkspaceFocusTarget | undefined;
  let frame = 0;
  let deadline = 0;
  let cancelOnInput = false;
  const observer = new MutationObserver(() => schedule());
  const cancelRequest = () => {
    pending = undefined;
    cancelOnInput = false;
    observer.disconnect();
    cancelAnimationFrame(frame);
    clearTimeout(deadline);
  };
  const schedule = () => {
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(() => {
      if (pending && focusTarget(pending)) cancelRequest();
    });
  };
  const request = (target: WorkspaceFocusTarget) => {
    cancelRequest();
    pending = target;
    // The key or click that made the request may still be propagating.
    queueMicrotask(() => {
      if (pending === target) cancelOnInput = true;
    });
    observer.observe(doc.body, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["inert", "aria-hidden", "data-workspace-unavailable"],
    });
    deadline = window.setTimeout(cancelRequest, 2000);
    schedule();
  };
  const onUserInput = () => {
    if (cancelOnInput) cancelRequest();
  };
  const onPointerDown = () => {
    unmarkKeyboardArea();
    onUserInput();
  };
  doc.addEventListener("pointerdown", onPointerDown, true);
  doc.addEventListener("keydown", onUserInput, true);

  return {
    move: (direction: 1 | -1, fromArea?: string, closingPalette = false) => {
      cancelRequest();
      return move(direction, fromArea, closingPalette);
    },
    focus,
    request,
    dispose: () => {
      cancelRequest();
      unmarkKeyboardArea();
      doc.removeEventListener("focusin", remember);
      doc.removeEventListener("focusout", recover);
      doc.removeEventListener("pointerdown", onPointerDown, true);
      doc.removeEventListener("keydown", onUserInput, true);
    },
  };
}

let controller: ReturnType<typeof createWorkspaceFocusController> | undefined;
let focusRequestVersion = 0;
export const getWorkspaceFocusRequestVersion = () => focusRequestVersion;

/**
 * Starts workspace focus for the app: F6 area moves, focus requests, focus-loss recovery,
 * focus into a panel the user opens, and focus into the Pull Requests page on entry.
 */
export function startWorkspaceFocus(router: AnyRouter): () => void {
  const active = createWorkspaceFocusController(document);
  controller = active;
  const stopPanels = useRightPanelStore.subscribe((state, previous) => {
    if (state.userActionRevisionByThreadKey === previous.userActionRevisionByThreadKey) return;
    for (const [threadKey, revision] of Object.entries(state.userActionRevisionByThreadKey)) {
      if (previous.userActionRevisionByThreadKey[threadKey] === revision) continue;
      const next = state.byThreadKey[threadKey];
      const before = previous.byThreadKey[threadKey];
      if (next?.isOpen && (!before?.isOpen || next.activeSurfaceId !== before.activeSurfaceId))
        requestWorkspaceFocus({ area: "panel" });
    }
  });
  const stopRoute = router.subscribe("onResolved", ({ fromLocation, toLocation }) => {
    // Closing Settings or Usage over the page restores focus itself; that is no entry.
    if (
      toLocation.pathname === "/pull-requests" &&
      fromLocation?.pathname !== "/pull-requests" &&
      !(fromLocation && isAppModalPage(fromLocation.pathname))
    )
      requestWorkspaceFocus({ area: "pull-requests" });
  });
  return () => {
    stopPanels();
    stopRoute();
    active.dispose();
    if (controller === active) controller = undefined;
  };
}

/** Moves to the next or previous visible area. `fromArea` overrides the focused area. */
export function moveWorkspaceFocus(direction: 1 | -1, fromArea?: string): boolean {
  return controller?.move(direction, fromArea) ?? false;
}

/** Explicit user actions request focus after React has committed the destination. */
export function requestWorkspaceFocus(target: WorkspaceFocusTarget) {
  focusRequestVersion++;
  controller?.request(target);
}

export function requestWorkspaceContentFocus() {
  // Resolve after the layout commits: closing a maximized panel makes the
  // composer available again, while hiding a sidebar may leave only the panel.
  requestWorkspaceFocus({ area: "content" });
}

let pendingPaletteDirection: 1 | -1 | undefined;
export function queueWorkspaceFocusFromPalette(direction: 1 | -1) {
  pendingPaletteDirection = direction;
}

/** Runs at the palette's normal focus restoration milestone, after its focus trap closes. */
export function restoreWorkspaceFocusFromPalette(): boolean {
  if (pendingPaletteDirection === undefined) return false;
  const direction = pendingPaletteDirection;
  pendingPaletteDirection = undefined;
  window.requestAnimationFrame(() => controller?.move(direction, undefined, true));
  return true;
}
