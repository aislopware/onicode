import { ThreadId } from "@onicode/contracts";
import { Atom } from "effect/reactivity";
import { describe, expect, it, vi } from "vite-plus/test";

import { decodeFullFileText, fullFileTextResource } from "./useFullFileText";

vi.mock("../../connection/runtime", async () => ({
  connectionAtomRuntime: Atom.runtime((await import("effect/http")).FetchHttpClient.layer),
}));
vi.mock("../../state/assets", () => ({ useAssetUrlState: vi.fn() }));

const threadId = ThreadId.make("thread-1");
const path = "/repo/logs/big.log";

describe("fullFileTextResource", () => {
  it.each([
    {
      name: "a thread file downloads as a thread attachment",
      input: { threadId, draftCwd: null, absolutePath: path },
      expected: { _tag: "media-file", threadId, path, disposition: "attachment" },
    },
    {
      name: "a draft file downloads from the draft's workspace",
      input: { threadId: null, draftCwd: "/repo", absolutePath: path },
      expected: { _tag: "draft-workspace-file", cwd: "/repo", path, disposition: "attachment" },
    },
    {
      name: "no workspace keeps the preview",
      input: { threadId: null, draftCwd: null, absolutePath: path },
      expected: null,
    },
    {
      name: "a disabled download mints nothing",
      input: { threadId, draftCwd: null, absolutePath: null },
      expected: null,
    },
  ])("$name", ({ input, expected }) => {
    expect(fullFileTextResource(input)).toEqual(expected);
  });
});

describe("decodeFullFileText", () => {
  const encode = (text: string) => new TextEncoder().encode(text);
  it.each([
    { name: "UTF-8 text", bytes: encode("héllo\n"), expected: "héllo\n" },
    {
      name: "a leading BOM is dropped and a later U+FEFF kept",
      bytes: Uint8Array.of(0xef, 0xbb, 0xbf, ...encode("text\uFEFFmore")),
      expected: "text\uFEFFmore",
    },
    {
      name: "an invalid trailing byte keeps the preview",
      bytes: Uint8Array.of(...encode("text"), 0xff),
      expected: null,
    },
    {
      name: "a NUL byte is binary",
      bytes: Uint8Array.of(...encode("a"), 0, ...encode("b")),
      expected: null,
    },
  ])("$name", ({ bytes, expected }) => {
    expect(decodeFullFileText(bytes)).toBe(expected);
  });
});
