import { Dialog as DialogPrimitive } from "@base-ui/react/dialog";
import { useRouterState } from "@tanstack/react-router";
import { T, useGT } from "gt-react";
import { XIcon } from "lucide-react";
import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";

import { getCommandPaletteOpener } from "../commandPaletteStore";
import { useNativeTitlebarHeight } from "../lib/windowControlsOverlay";
import {
  requestWorkspaceFocus,
  workspaceFocusTargetFor,
  type WorkspaceFocusTarget,
} from "../lib/workspaceFocus";
import { markAppModalReturnsFocus, useCloseAppModal } from "./sidebar/mainAppLocation";
import { GlobalVoiceInputIndicator } from "./chat/GlobalVoiceInputIndicator";
import { Button } from "./ui/button";
import { DialogBackdrop } from "./ui/dialog";
import { Tooltip, TooltipPopup, TooltipTrigger } from "./ui/tooltip";

// Settings and Usage are sibling routes, so a modal that mounts while another is mounted
// replaces it (Usage opened from Settings) and returns to what that one opened from.
let mountedModals = 0;
let mountedModalOpener: WorkspaceFocusTarget | null = null;

/**
 * Large modal for the Settings and Usage routes. Esc, the close button, or a press on the
 * backdrop returns to the page that opened it. Keyboard guards that skip dialogs exempt
 * `[data-app-modal]`, because these pages own their shortcuts.
 */
export function AppRouteModal({ label, children }: { label: string; children: ReactNode }) {
  const closeAppModal = useCloseAppModal();
  const [open, setOpen] = useState(true);
  // A close skipped for a navigation still loading normally ends with this modal unmounted.
  // One that comes back to a modal page (a redirect) keeps it mounted, so it shows again on
  // the new entry instead of staying closed on its own route.
  const entryKey = useRouterState({ select: (state) => state.resolvedLocation?.state.__TSR_key });
  const [skippedOn, setSkippedOn] = useState<{ entryKey: string | undefined } | null>(null);
  const shown = open || (skippedOn !== null && skippedOn.entryKey !== entryKey);
  const close = () => {
    setOpen(false);
    setSkippedOn(null);
  };
  // Text selection that ends on the backdrop is not a dismissal.
  const backdropPressRef = useRef(false);
  // Focus the surface, not its first control: no stray focus ring, no phone keyboard.
  // On close, return focus to the opener only after a keyboard close; after a pointer close it
  // would paint a focus ring.
  const popupRef = useRef<HTMLDivElement>(null);
  // Read the opener's area now: the page under the modal may unmount and take it along. With
  // nothing focused there is no opener; Base UI would send focus to the page's first control.
  const [opener] = useState(() => {
    if (mountedModals > 0) return mountedModalOpener;
    const element = getCommandPaletteOpener() ?? document.activeElement;
    return element instanceof HTMLElement && element !== document.body
      ? workspaceFocusTargetFor(element)
      : null;
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- records the mounted modal's opener for a modal that replaces it, and drops it with the last modal so it holds no closed page.
  useEffect(() => {
    mountedModals += 1;
    mountedModalOpener = opener;
    return () => {
      mountedModals -= 1;
      if (mountedModals === 0) mountedModalOpener = null;
    };
  }, [opener]);
  // A key or press during the exit animation moves focus on, and the close must not undo it.
  const inputWhileClosingRef = useRef<{ seen: boolean; stop: AbortController } | null>(null);
  // Clear the native window controls (macOS traffic lights, the Windows and Linux caption
  // buttons). Below sm the modal fills the window and leaves that strip empty; up to lg its
  // 1.5rem inset is shorter than the strip.
  const nativeTitlebarHeight = useNativeTitlebarHeight();

  return (
    <DialogPrimitive.Root
      open={shown}
      // Portaled layers outside this tree (toasts, confirm dialogs) are not backdrop presses.
      disablePointerDismissal
      onOpenChange={(nextOpen) => {
        if (nextOpen) return;
        // Closing unmounts a focused field without a blur, so commit-on-blur rows would
        // drop what was typed. Blur first so they commit.
        const focused = document.activeElement;
        if (focused instanceof HTMLElement && popupRef.current?.contains(focused)) focused.blur();
        // Base UI returns focus once the exit animation ends. The closing key or press is past
        // the window's capture phase, so only later input counts.
        const watch = { seen: false, stop: new AbortController() };
        const see = () => {
          watch.seen = true;
        };
        // Window capture: workspace shortcuts such as F6 stop the key before the document.
        for (const type of ["keydown", "pointerdown"])
          window.addEventListener(type, see, { capture: true, signal: watch.stop.signal });
        inputWhileClosingRef.current?.stop.abort();
        inputWhileClosingRef.current = watch;
        close();
      }}
      onOpenChangeComplete={(nextOpen) => {
        if (!nextOpen && !closeAppModal()) setSkippedOn({ entryKey });
      }}
    >
      <DialogPrimitive.Portal>
        <DialogBackdrop />
        <DialogPrimitive.Viewport
          style={
            {
              "--app-modal-titlebar-height": `${nativeTitlebarHeight}px`,
              "--app-modal-top-inset": `max(env(safe-area-inset-top), ${nativeTitlebarHeight}px)`,
              "--app-modal-sm-top": `max(1.5rem, ${nativeTitlebarHeight}px)`,
            } as CSSProperties
          }
          // Electron hit-tests drag regions under the backdrop regardless of z-index; without
          // this, presses above the modal move the window instead of dismissing it.
          className="fixed inset-0 z-50 flex items-center justify-center [-webkit-app-region:no-drag] sm:p-6 sm:pt-(--app-modal-sm-top) lg:p-10"
          onPointerDown={(event) => {
            backdropPressRef.current = event.target === event.currentTarget;
          }}
          onClick={(event) => {
            if (backdropPressRef.current && event.target === event.currentTarget) close();
            backdropPressRef.current = false;
          }}
        >
          <DialogPrimitive.Popup
            ref={popupRef}
            initialFocus={popupRef}
            finalFocus={(closeType) => {
              const watch = inputWhileClosingRef.current;
              watch?.stop.abort();
              inputWhileClosingRef.current = null;
              const element = opener?.returnTo;
              if (closeType !== "keyboard" || !element || watch?.seen) return false;
              markAppModalReturnsFocus();
              if (element.isConnected && !element.closest("[inert]")) return element;
              // The thread behind the modal stays inert, and a page it replaced unmounted, until
              // the route closes; the opener or its remounted replacement takes focus then.
              requestWorkspaceFocus(opener);
              return false;
            }}
            aria-label={label}
            data-slot="dialog-popup"
            data-app-modal=""
            className="relative flex size-full min-h-0 min-w-0 flex-col overflow-hidden bg-background max-sm:pt-(--app-modal-top-inset) max-sm:pb-safe text-foreground outline-none transition-[opacity,scale] duration-150 ease-out [-webkit-app-region:no-drag] data-ending-style:scale-[0.985] data-ending-style:opacity-0 data-starting-style:scale-[0.985] data-starting-style:opacity-0 sm:max-h-[60rem] sm:max-w-[80rem] sm:rounded-2xl sm:border sm:shadow-2xl/40"
          >
            {/* The emptied strip still moves the window, as the titlebar it replaces does. */}
            <div
              aria-hidden
              className="absolute inset-x-0 top-0 hidden h-(--app-modal-titlebar-height) [-webkit-app-region:drag] max-sm:block"
            />
            {children}
            <GlobalVoiceInputIndicator />
          </DialogPrimitive.Popup>
        </DialogPrimitive.Viewport>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}

export function AppRouteModalClose() {
  const gt = useGT();
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <DialogPrimitive.Close
            aria-label={gt("Close")}
            render={<Button size="icon-sm" variant="ghost" />}
          />
        }
      >
        <XIcon />
      </TooltipTrigger>
      <TooltipPopup side="bottom">
        <T>Close (Esc)</T>
      </TooltipPopup>
    </Tooltip>
  );
}
