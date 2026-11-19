const EDITABLE_SELECTOR = [
  "input",
  "textarea",
  "select",
  '[contenteditable=""]',
  '[contenteditable="true"]',
  '[contenteditable="plaintext-only"]',
  '[role="textbox"]',
].join(",");

/** Includes focused descendants of open shadow roots, whose key events target the host. */
export function isEditableFocused(target: EventTarget | null = document.activeElement): boolean {
  let element = target instanceof Element ? target : null;
  while (element?.shadowRoot?.activeElement) element = element.shadowRoot.activeElement;
  return element !== null && element.closest(EDITABLE_SELECTOR) !== null;
}

// Window return puts the caret in the composer unless focus or a selection is deliberate: another
// text field, a terminal, an open dialog, menu, or popup, a region, timeline, or sidebar row reached
// from the keyboard, a region with its own keys (aria-keyshortcuts), or selected text the composer
// would clear. A plain focused button or a clicked row yields to the composer.
export function shouldRefocusComposerOnWindowFocus(
  activeElement:
    | (Pick<Element, "tagName" | "closest" | "getAttribute"> & { isContentEditable?: boolean })
    | null,
  selection: Pick<Selection, "isCollapsed"> | null,
): boolean {
  if (selection !== null && !selection.isCollapsed) return false;
  if (activeElement === null || activeElement.tagName === "BODY") return true;
  if (
    activeElement.tagName === "INPUT" ||
    activeElement.tagName === "TEXTAREA" ||
    activeElement.tagName === "SELECT" ||
    activeElement.tagName === "IFRAME" ||
    activeElement.tagName === "WEBVIEW" ||
    activeElement.isContentEditable === true ||
    activeElement.getAttribute("role") === "textbox"
  ) {
    return false;
  }
  return (
    activeElement.closest(
      '[data-workspace-keyboard-focus], [data-sidebar-row]:focus-visible, [role="dialog"], [role="alertdialog"], [role="application"], [role="menu"], [role="listbox"], [data-slot$="-popup"], [data-terminal-owner], [aria-keyshortcuts]',
    ) === null
  );
}
