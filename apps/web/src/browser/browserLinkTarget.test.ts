import type { BrowserLinkTarget } from "@onicode/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

import { resolveLinkTarget } from "./browserLinkTarget";
import { canonicalLinkUrl } from "./webUrl";

const settings = vi.hoisted(() => ({ browserLinkTarget: "system" as BrowserLinkTarget }));

vi.mock("~/hooks/useSettings", () => ({
  ensureClientSettingsHydrated: vi.fn(async () => undefined),
  getClientSettings: () => settings,
}));

const click = { metaKey: false, ctrlKey: false };

describe("canonicalLinkUrl", () => {
  it("spells a loose web link the way the system browser reads it", () => {
    // The preview's own normalizer would read `http:/localhost:5173` as a host named `http`.
    for (const url of ["http:/localhost:5173", "http:localhost:5173", " http://localhost:5173"]) {
      expect(canonicalLinkUrl(url)).toBe("http://localhost:5173/");
    }
    expect(canonicalLinkUrl("magnet:?xt=urn:btih:abc")).toBe("magnet:?xt=urn:btih:abc");
  });
});

describe("resolveLinkTarget", () => {
  it("leaves non-web schemes to the shell", () => {
    for (const url of ["mailto:someone@example.com", "vscode://file/x", "not a url"]) {
      expect(resolveLinkTarget({ url, event: click, preference: "app", canOpenInApp: true })).toBe(
        "system",
      );
    }
  });
});
