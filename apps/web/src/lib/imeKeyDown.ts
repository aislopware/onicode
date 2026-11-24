/** Safari ends a composition just before its confirming keydown, which reports only keyCode 229. */
export function isImeKeyDown(event: Pick<KeyboardEvent, "isComposing" | "keyCode">): boolean {
  return event.isComposing || event.keyCode === 229;
}
