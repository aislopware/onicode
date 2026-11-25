import { i18n } from "@pierre/diffs";
import { decodeOptions } from "gt-react";
import { describe, expect, it } from "vite-plus/test";

import { DIFFS_MESSAGES } from "./diffsMessages";
import { SUPPORTED_LOCALES } from "./locales";

const catalogs = import.meta.glob<Record<string, string>>("./translations/*.json", {
  eager: true,
  import: "default",
});
const english = catalogs["./translations/en.json"]!;

describe("diff view messages", () => {
  // Pierre throws on ICU it cannot compile, which would drop the whole locale back to English.
  it.each(SUPPORTED_LOCALES)("compile in Pierre for %s", (locale) => {
    const catalog = catalogs[`./translations/${locale}.json`]!;
    for (const [key, message] of Object.entries(DIFFS_MESSAGES)) {
      const decoded = decodeOptions(message);
      const source = typeof decoded?.$_source === "string" ? decoded.$_source : message;
      const hashes = Object.keys(english).filter((hash) => english[hash] === source);
      expect(hashes, key).not.toHaveLength(0);
      for (const hash of hashes) {
        expect(() => i18n.set({ locale, messages: { [key]: catalog[hash] } }), key).not.toThrow();
        expect(i18n.t(key as keyof typeof DIFFS_MESSAGES, { count: 2 })).not.toBe("");
      }
    }
    i18n.set();
  });
});
