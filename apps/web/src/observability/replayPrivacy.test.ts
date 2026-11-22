import { describe, expect, it } from "vite-plus/test";

import { addressHasQuery, maskReplayAttribute } from "./replayPrivacy";

describe("maskReplayAttribute", () => {
  it.each([
    ["class", "flex gap-2", "flex gap-2"],
    ["aria-expanded", "true", "true"],
    ["data-state", "open", "open"],
    ["data-markdown-copy", "graph TD; A-->B", "***** *** *****"],
    ["data-diff-file-key", "src/secret.ts", "*************"],
    ["aria-label", "Open ~/notes", "**** *******"],
  ])("%s", (name, value, expected) => {
    expect(maskReplayAttribute(name, value)).toBe(expected);
  });
});

describe("addressHasQuery", () => {
  it.each([
    ["#/threads/1", "", false],
    ["#/new?prompt=hi", "", true],
    ["", "?prompt=hi", true],
  ])("%s %s", (hash, search, expected) => {
    expect(addressHasQuery(hash, search)).toBe(expected);
  });
});
