import { describe, expect, it } from "vite-plus/test";

import { nextArchivedChunkView } from "./ArchivedThreadsPanel.logic.ts";

describe("nextArchivedChunkView", () => {
  const mounted = { near: true, rowHeight: 75 };

  it.each([
    ["the same rows report again", mounted, true, 750, 10, mounted],
    ["a hidden chunk reports no height", mounted, true, 0, 10, mounted],
    ["a placeholder has no rows to measure", { near: false, rowHeight: 80 }, false, 800, 0, null],
  ] as const)("keeps the same view when %s", (_name, previous, near, height, rows, expected) => {
    expect(nextArchivedChunkView(previous, near, height, rows)).toBe(expected ?? previous);
  });

  it("measures the rows it mounted, not the rows it was given", () => {
    expect(nextArchivedChunkView(mounted, false, 600, 8)).toEqual({ near: false, rowHeight: 75 });
    expect(nextArchivedChunkView(mounted, true, 640, 8)).toEqual({ near: true, rowHeight: 80 });
  });
});
