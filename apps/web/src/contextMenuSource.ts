import type { ContextMenuSource } from "@onicode/contracts";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";

// Chromium uses no button for keyboard context menus and zero clicks for keyboard activation.
export function contextMenuSourceOf(
  event: Pick<MouseEvent, "type" | "button" | "detail">,
): ContextMenuSource {
  return (event.type === "click" ? event.detail === 0 : event.button === -1) ? "keyboard" : "mouse";
}

// macOS Chromium sends no contextmenu for the menu keys, so an element with a menu asks for its own.
export function requestContextMenuOnMenuKey(event: ReactKeyboardEvent<HTMLElement>): void {
  if (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
  if (event.altKey || event.ctrlKey || event.metaKey) return;
  event.preventDefault();
  const rect = event.currentTarget.getBoundingClientRect();
  event.currentTarget.dispatchEvent(
    // A PointerEvent keeps button -1 (a MouseEvent clamps it to 0), which reads as the keyboard.
    new PointerEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      button: -1,
      clientX: rect.left,
      clientY: rect.bottom,
    }),
  );
}
