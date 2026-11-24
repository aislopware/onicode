import { i18n, type DiffsMessageKey } from "@pierre/diffs";
import { msg } from "gt-react";

import { getActiveLocale, template } from "./messages";

// Pierre compiles a subset of ICU: `#` in plural branches and no arguments inside them.
export const DIFFS_MESSAGES = {
  unmodifiedLines: msg("{count, plural, one {# unmodified line} other {# unmodified lines}}"),
  unknownUnmodifiedLines: msg("More unchanged context may be available"),
  expandAll: msg("Expand all"),
  expandLine: msg("Show full line"),
  noNewlineAtEndOfFile: msg("No newline at end of file"),
  acceptCurrentChange: msg("Accept current change"),
  acceptIncomingChange: msg("Accept incoming change"),
  acceptBoth: msg("Accept both"),
  searchPlaceholder: msg("Search", { $context: "text search field placeholder" }),
  replacePlaceholder: msg("Replace", { $context: "replacement text field placeholder" }),
  searching: msg("Searching…"),
  searchResultCount: msg("{count, plural, one {# result} other {# results}}"),
  matchCase: msg("Match Case"),
  wholeWord: msg("Whole Word"),
  regularExpression: msg("Regexp", { $context: "regular expression search toggle" }),
  replaceMatch: msg("Replace", { $context: "button that replaces one search match" }),
  replaceAllMatches: msg("Replace All"),
  previousMatch: msg("Previous", { $context: "previous search match" }),
  nextMatch: msg("Next", { $context: "next search match" }),
  closeSearch: msg("Close", { $context: "closes the search panel" }),
  toggleReplace: msg("Toggle Replace"),
} satisfies Partial<Record<DiffsMessageKey, string>>;

/** Pierre keeps its strings in a module-level singleton, which every diff and code view reads. */
export function applyDiffsMessages(): void {
  const locale = getActiveLocale();
  const messages = Object.fromEntries(
    Object.entries(DIFFS_MESSAGES).map(([key, message]) => [key, template(message)]),
  );
  try {
    i18n.set({ locale, messages });
  } catch (cause) {
    // A translation Pierre cannot compile must not take the diff views down with it.
    console.error("Diff view translations rejected", cause);
    i18n.set({ locale });
  }
}
