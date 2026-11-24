import type { KeyboardEvent } from "react";

type SearchFieldKeyDown = Pick<
  KeyboardEvent,
  "key" | "keyCode" | "repeat" | "preventDefault" | "stopPropagation"
> & { readonly nativeEvent: Pick<globalThis.KeyboardEvent, "isComposing"> };

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
  if (event.key !== "Escape" || event.nativeEvent.isComposing || event.keyCode === 229) return;
  // A held Esc that just emptied the field must not go on to close the page.
  if (query === "" && !event.repeat) return;
  event.preventDefault();
  event.stopPropagation();
  if (query !== "") clear();
}
