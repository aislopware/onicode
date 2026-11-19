import { shouldRefocusComposerOnWindowFocus } from "../lib/editableFocus";

/** Dictation hands focus back to its editor unless the user moved it somewhere deliberate. */
export function mayRefocusAfterDictation(editor: Element | null | undefined): boolean {
  const active = document.activeElement;
  return (
    (active !== null && (editor?.contains(active) ?? false)) ||
    shouldRefocusComposerOnWindowFocus(active, document.getSelection())
  );
}
