import { describe, expect, it } from "vite-plus/test";
import { groupsAppBarTrailingTools } from "./AppBar.logic";
import type { NativeLayoutMetrics } from "../../lib/reserved-regions";

// Poses reported by the iOS 27.1 iPhone Duo simulator.
const duoOuter: NativeLayoutMetrics = {
  width: 466,
  height: 678,
  hasHinge: true,
  horizontalSizeClass: "compact",
  verticalBarEdge: "right",
  safeArea: { top: 0, bottom: 34, left: 0, right: 84 },
  reservedRegions: [],
};
const duoInnerPortrait: NativeLayoutMetrics = {
  width: 669,
  height: 951,
  hasHinge: true,
  horizontalSizeClass: "regular",
  verticalBarEdge: "none",
  safeArea: { top: 82, bottom: 34, left: 0, right: 0 },
  reservedRegions: [{ kind: "occlusion", x: 535, y: 0, width: 134, height: 82 }],
};
const duoInnerLandscape: NativeLayoutMetrics = {
  ...duoInnerPortrait,
  width: 951,
  height: 669,
  verticalBarEdge: "right",
};

const groups = (overrides: Partial<Parameters<typeof groupsAppBarTrailingTools>[0]>) =>
  groupsAppBarTrailingTools({
    nativeColumns: true,
    metrics: duoOuter,
    sidebar: false,
    closable: false,
    tools: 3,
    ...overrides,
  });

describe("groupsAppBarTrailingTools", () => {
  it("groups only the tools a vertical bar would draw", () => {
    expect(groups({})).toBe(true);
    expect(groups({ metrics: duoInnerLandscape })).toBe(true);
    // The unfolded portrait Duo and a landscape sidebar draw horizontal bars with room.
    expect(groups({ metrics: duoInnerPortrait })).toBe(false);
    expect(groups({ metrics: duoInnerLandscape, sidebar: true })).toBe(false);
    expect(groups({ metrics: null })).toBe(false);
    expect(groups({ nativeColumns: false })).toBe(false);
  });

  it("leaves a single tool and close bars alone", () => {
    expect(groups({ tools: 1 })).toBe(false);
    expect(groups({ closable: true })).toBe(false);
  });
});
