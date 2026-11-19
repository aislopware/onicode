import { shouldRefocusComposerOnWindowFocus } from "../lib/editableFocus";

/**
 * Dictation hands focus back to its editor unless the user moved it somewhere deliberate since
 * the recording started; focus that never moved, as with the mic or the shortcut, comes back.
 */
export function mayRefocusAfterDictation(
  editor: Element | null | undefined,
  startFocus: Element | null,
): boolean {
  const active = document.activeElement;
  let focused = active;
  while (focused?.shadowRoot?.activeElement) focused = focused.shadowRoot.activeElement;
  return (
    (active !== null && (active === startFocus || (editor?.contains(active) ?? false))) ||
    shouldRefocusComposerOnWindowFocus(focused, document.getSelection())
  );
}
