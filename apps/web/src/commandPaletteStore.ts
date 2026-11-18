import { useEffect } from "react";
import { create } from "zustand";

import {
  type CommandPaletteUiAction,
  type CommandPaletteUiState,
  reduceCommandPaletteUiState,
} from "./components/CommandPalette.logic";
import { getMainAppLocationVersion } from "./components/sidebar/mainAppLocation";
import { getWorkspaceFocusRequestVersion } from "./lib/workspaceFocus";

const INITIAL_STATE: CommandPaletteUiState = {
  open: false,
  mode: "command",
  openIntent: null,
  session: 0,
};

export const useCommandPaletteStore = create<CommandPaletteUiState>(() => INITIAL_STATE);

let returnFocus: {
  element: HTMLElement | null;
  href: string;
  focusRequest: number;
  location: number;
} | null = null;
// A command that navigates closes the palette before the page it opens mounts, and focus
// recovery may move focus meanwhile. The palette hands its opener to that page until a key or
// press after the close, or until a page other than Settings or Usage resolves.
let handover: { element: HTMLElement | null; location: number } | null = null;
let inputSinceClose = false;
const endHandover = () => {
  inputSinceClose = true;
  handover = null;
  window.removeEventListener("keydown", endHandover, true);
  window.removeEventListener("pointerdown", endHandover, true);
};

export function dispatchCommandPalette(action: CommandPaletteUiAction): void {
  const state = useCommandPaletteStore.getState();
  const next = reduceCommandPaletteUiState(state, action);
  if (!state.open && next.open) {
    endHandover();
    // The workspace turns inert before the popup mounts, which blurs the opener.
    returnFocus = {
      element: document.activeElement instanceof HTMLElement ? document.activeElement : null,
      href: window.location.href,
      focusRequest: getWorkspaceFocusRequestVersion(),
      location: getMainAppLocationVersion(),
    };
  }
  if (state.open && !next.open) {
    // The key or press that closes it is already past the window's capture phase, where
    // workspace shortcuts such as F6 stop keys before the document sees them.
    inputSinceClose = false;
    window.addEventListener("keydown", endHandover, true);
    window.addEventListener("pointerdown", endHandover, true);
  }
  useCommandPaletteStore.setState(next, true);
}

/**
 * Where the closing palette should return focus: `false` when a command navigated or asked
 * for workspace focus (the destination owns it), else the opener, or `true` for the default.
 */
export function getCommandPaletteReturnFocus(): HTMLElement | boolean {
  if (
    !returnFocus ||
    window.location.href !== returnFocus.href ||
    getWorkspaceFocusRequestVersion() !== returnFocus.focusRequest
  )
    return false;
  const { element } = returnFocus;
  return element?.isConnected && !element.matches(":disabled") ? element : true;
}

/** Called when the palette closes without returning focus because its command took it. */
export function handOverCommandPaletteOpener(): void {
  // A command that asked for workspace focus already placed it; only a navigation hands over.
  if (
    !returnFocus ||
    inputSinceClose ||
    getWorkspaceFocusRequestVersion() !== returnFocus.focusRequest
  )
    return;
  handover = { element: returnFocus.element, location: returnFocus.location };
}

/** The element the palette opened from, while the palette or the command it ran owns focus. */
export function getCommandPaletteOpener(): HTMLElement | null {
  if (returnFocus && document.activeElement?.closest("[data-command-palette]"))
    return returnFocus.element;
  return handover?.location === getMainAppLocationVersion() ? handover.element : null;
}

// The palette is only mounted on app-shell routes (not error pages or
// landings), so its shortcuts must stay inert elsewhere.
let mountedPalettes = 0;

export function isCommandPaletteMounted(): boolean {
  return mountedPalettes > 0;
}

/**
 * Marks the palette as mounted while the calling component is. An unmounted
 * palette cannot stay open, so the last unmount resets the store.
 */
export function useCommandPaletteMount(): void {
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- The palette's mount gates its global shortcuts, and leaving the route while it is open calls no close handler, so only unmount can reset the store.
  useEffect(() => {
    mountedPalettes += 1;
    return () => {
      mountedPalettes -= 1;
      if (mountedPalettes === 0) {
        useCommandPaletteStore.setState(
          (state) => ({ ...INITIAL_STATE, session: state.session + 1 }),
          true,
        );
      }
    };
  }, []);
}
