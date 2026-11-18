import type { MenuAction } from "@react-native-menu/menu";
import { describe, expect, it } from "vite-plus/test";
import { currentMenuPath } from "./AndroidAnchoredMenu.logic";

const helper = (choices: MenuAction[]): MenuAction => ({
  id: "explain",
  title: "Explain",
  subactions: choices,
});
const settled = (choices: MenuAction[]): MenuAction => ({
  id: "explain:settled",
  title: "Settled threads",
  subactions: choices,
});

describe("currentMenuPath", () => {
  it("shows an open submenu's refreshed choices", () => {
    const opened = [helper([{ id: "explain:one", title: "One" }])];
    const busy = helper([{ id: "explain:one", title: "One", attributes: { disabled: true } }]);
    expect(currentMenuPath([busy], opened)).toEqual([busy]);
  });

  it("closes back to the deepest submenu that still has choices", () => {
    const opened = [
      helper([settled([{ id: "explain:old", title: "Old" }])]),
      settled([{ id: "explain:old", title: "Old" }]),
    ];
    const refreshed = helper([{ id: "explain:new", title: "New" }]);
    expect(currentMenuPath([refreshed], opened)).toEqual([refreshed]);
    expect(currentMenuPath([{ id: "explain", title: "Explain" }], opened)).toEqual([]);
  });
});
