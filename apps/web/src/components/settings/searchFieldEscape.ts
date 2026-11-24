import type { KeyboardEvent } from "react";

import { isImeKeyDown } from "../../lib/imeKeyDown";

type SearchFieldKeyDown = Pick<
  KeyboardEvent,
  | "key"
  | "repeat"
  | "defaultPrevented"
  | "altKey"
  | "ctrlKey"
  | "metaKey"
  | "shiftKey"
  | "preventDefault"
  | "stopPropagation"
> & { readonly nativeEvent: Pick<globalThis.KeyboardEvent, "isComposing" | "keyCode"> };

/**
 * Esc empties a filled search field without closing the Settings page or dialog around it; an
 * empty field lets Esc through. Pair it with `type="text"`: native search inputs clear on Esc
 * even while an IME is composing.
 */
export function clearSearchOnEscape(
  event: SearchFieldKeyDown,
  query: string,
  clear: () => void,
): void {
  if (event.key !== "Escape" || event.defaultPrevented) return;
  if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
  const composing = isImeKeyDown(event.nativeEvent);
  // A held Esc that just emptied the field must not go on to close the page.
  if (!composing && query === "" && !event.repeat) return;
  event.stopPropagation();
  // The Esc that cancels an IME composition belongs to the IME, not to the dialog around it.
  if (composing) return;
  event.preventDefault();
  if (query !== "") clear();
}
