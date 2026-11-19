/** Dictation hands focus back to its editor only while the user has not moved it elsewhere. */
export function mayRefocusAfterDictation(editor: Element | null | undefined): boolean {
  const active = document.activeElement;
  return active === null || active === document.body || (editor?.contains(active) ?? false);
}
