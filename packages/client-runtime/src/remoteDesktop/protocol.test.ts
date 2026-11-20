import { describe, expect, it } from "vite-plus/test";

import { REMOTE_DESKTOP_KEYBOARD_SENTINEL as S, remoteDesktopKeyboardEdit } from "./protocol.ts";

describe("remoteDesktopKeyboardEdit", () => {
  it.each([
    ["typed text", `${S}a`, `${S}ab`, { backspaces: 0, lines: ["b"], reset: false }],
    ["a deleted character", `${S}ab`, `${S}a`, { backspaces: 1, lines: [""], reset: false }],
    // Telex turns "aa" into "â": the keyboard rewrites a letter it already typed.
    ["a rewritten letter", `${S}caa`, `${S}câ`, { backspaces: 2, lines: ["â"], reset: false }],
    ["a deleted emoji", `${S}😀`, S, { backspaces: 1, lines: [""], reset: false }],
    ["Backspace on an empty field", S, "", { backspaces: 1, lines: [""], reset: true }],
    ["Enter", `${S}a`, `${S}a\n`, { backspaces: 0, lines: ["", ""], reset: true }],
    [
      "a full field",
      `${S}${"a".repeat(63)}`,
      `${S}${"a".repeat(64)}`,
      { backspaces: 0, lines: ["a"], reset: true },
    ],
  ])("%s", (_name, before, after, edit) => {
    expect(remoteDesktopKeyboardEdit(before, after)).toEqual(edit);
  });
});
