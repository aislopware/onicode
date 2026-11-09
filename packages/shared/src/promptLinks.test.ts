import { describe, expect, it } from "vite-plus/test";

import {
  buildNewThreadLink,
  parseNewThreadLink,
  parseThreadLinkPrompt,
  promptLinkAppPath,
  promptLinkDeepLink,
  sanitizeLinkedPrompt,
  withoutPromptLinkParams,
} from "./promptLinks.ts";

describe("sanitizeLinkedPrompt", () => {
  it("removes characters a reader cannot see and counts them", () => {
    const tagged = "rate me" + String.fromCodePoint(0xe0041, 0xe0042);
    expect(sanitizeLinkedPrompt(`a\u200Bb\u202Ec\uFEFF${tagged}`)).toEqual({
      text: "abcrate me",
      hiddenCharacters: 5,
    });
    expect(sanitizeLinkedPrompt("a\u2028b\u2029c")).toEqual({
      text: "a\nb\nc",
      hiddenCharacters: 0,
    });
  });

  it("keeps emoji sequences intact", () => {
    const family = "\u{1F468}\u200D\u{1F469}\u200D\u{1F467}";
    const heart = "\u2764\uFE0F";
    expect(sanitizeLinkedPrompt(`${family} ${heart}`)).toEqual({
      text: `${family} ${heart}`,
      hiddenCharacters: 0,
    });
    expect(sanitizeLinkedPrompt("a\u200Db").hiddenCharacters).toBe(1);
    expect(sanitizeLinkedPrompt("\u{1F468}\u200D").hiddenCharacters).toBe(1);
    expect(sanitizeLinkedPrompt("\u{20000}\u200D\u{20001}").hiddenCharacters).toBe(1);
  });
});

describe("parseNewThreadLink", () => {
  it("reads a shared prompt with its optional repository and model", () => {
    const result = parseNewThreadLink(
      "?prompt=Rate+me&repo=https://github.com/pingdotgg/t3code.git&provider=Codex&model=gpt-5",
    );
    expect(result).toEqual({
      ok: true,
      value: {
        kind: "new",
        prompt: { text: "Rate me", hiddenCharacters: 0 },
        repository: { owner: "pingdotgg", name: "t3code" },
        provider: "codex",
        model: "gpt-5",
      },
    });
  });

  it("reads a pull request link, whose prompt is optional", () => {
    for (const pr of [
      "pingdotgg/t3code/42",
      "pingdotgg/t3code%2342",
      "https://github.com/pingdotgg/t3code/pull/42/files",
    ]) {
      expect(parseNewThreadLink(`?pr=${pr}`)).toEqual({
        ok: true,
        value: {
          kind: "pull-request",
          pullRequest: { owner: "pingdotgg", name: "t3code", number: 42 },
          prompt: null,
        },
      });
    }
  });

  it("refuses links it cannot open safely", () => {
    expect(parseNewThreadLink("?repo=a/b").ok).toBe(false);
    expect(parseNewThreadLink("?prompt=%E2%80%8B").ok).toBe(false);
    expect(parseThreadLinkPrompt("?prompt=%E2%80%8B").ok).toBe(false);
    expect(parseNewThreadLink("?prompt=x&repo=../etc").ok).toBe(false);
    expect(parseNewThreadLink(`?prompt=${"x".repeat(8_001)}`).ok).toBe(false);
  });
});

describe("thread links", () => {
  it("reads the prompt and leaves other parameters in place", () => {
    expect(parseThreadLinkPrompt("?prompt=Fix+it")).toEqual({
      ok: true,
      value: { text: "Fix it", hiddenCharacters: 0 },
    });
    expect(parseThreadLinkPrompt("?panel=diff")).toEqual({ ok: true, value: null });
    expect(withoutPromptLinkParams("?prompt=x&panel=diff")).toBe("?panel=diff");
  });
});

describe("link round trips", () => {
  it("builds an app link that parses back to the same prompt", () => {
    const link = buildNewThreadLink({
      prompt: "Swear count & rating?",
      repository: { owner: "pingdotgg", name: "onicode" },
      provider: "claudeAgent",
      model: "opus",
    });
    expect(link.startsWith("onicode://new?")).toBe(true);
    const path = promptLinkAppPath(link);
    expect(path).toBe(`/new${new URL(link).search}`);
    const parsed = parseNewThreadLink(new URL(link).search);
    expect(parsed.ok && parsed.value.kind === "new" && parsed.value.prompt.text).toBe(
      "Swear count & rating?",
    );
  });

  it("builds a web link on the origin it is given", () => {
    const link = buildNewThreadLink({ prompt: "hi", origin: "http://box.tailnet.ts.net:3773" });
    expect(link).toBe("http://box.tailnet.ts.net:3773/new?prompt=hi");
    expect(promptLinkAppPath(link, "http://box.tailnet.ts.net:3773")).toBe("/new?prompt=hi");
    expect(promptLinkAppPath(link)).toBeNull();
  });

  it("round trips a thread path through the app scheme", () => {
    const deepLink = promptLinkDeepLink("/env-1/mcp%3A1");
    expect(deepLink).toBe("onicode://threads/env-1/mcp%3A1");
    expect(promptLinkAppPath(deepLink)).toBe("/env-1/mcp%3A1");
  });

  it("maps app scheme links and the given web app's links to in-app paths, and nothing else", () => {
    const web = "https://box.tailnet.ts.net";
    expect(promptLinkAppPath("onicode://new?prompt=hi")).toBe("/new?prompt=hi");
    expect(promptLinkAppPath("onicode-dev://threads/env-1/thread-1")).toBe("/env-1/thread-1");
    expect(promptLinkAppPath(`${web}/env-1/thread-1?prompt=go`, web)).toBe(
      "/env-1/thread-1?prompt=go",
    );
    expect(promptLinkAppPath(`${web}/threads/env-1/thread-1`, web)).toBe("/env-1/thread-1");
    // The web app's own thread route, reloaded, is no link.
    expect(promptLinkAppPath(`${web}/env-1/thread-1`, web)).toBeNull();
    expect(promptLinkAppPath(`${web}/threads/env-1`, web)).toBeNull();
    expect(promptLinkAppPath(`${web}/settings/general`, web)).toBeNull();
    expect(promptLinkAppPath("https://evil.test/new?prompt=hi", web)).toBeNull();
    expect(promptLinkAppPath("onicode://app/index.html")).toBeNull();
  });
});
