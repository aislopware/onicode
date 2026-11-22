// Attributes that describe layout and state but never content. Every other attribute, such as
// `data-markdown-copy` or a file key, can hold prompts, source or paths, so replays mask it.
const LAYOUT_ATTRIBUTES = new Set([
  "class",
  "style",
  "id",
  "role",
  "type",
  "tabindex",
  "dir",
  "hidden",
  "disabled",
  "checked",
  "selected",
  "open",
  "width",
  "height",
  "viewBox",
  "d",
  "fill",
  "stroke",
]);

const STATE_ATTRIBUTE =
  /^(?:aria-(?:hidden|expanded|selected|checked|pressed|disabled|current|busy|invalid|orientation|modal|haspopup)|data-(?:state|slot|side|align|orientation|variant|size|disabled|highlighted|selected|active|open|placeholder))$/u;

/** Keeps layout and state attributes, and replaces every other value with asterisks. */
export function maskReplayAttribute(name: string, value: string): string {
  return LAYOUT_ATTRIBUTES.has(name) || STATE_ATTRIBUTE.test(name)
    ? value
    : value.replace(/\S/gu, "*");
}

/** A replay must not record a page whose address carries a query: links put prompts there. */
export const addressHasQuery = (hash: string, search: string): boolean =>
  search.length > 0 || hash.includes("?");
