import { describe, expect, it } from "vite-plus/test";

import { NOTEBOOK_OUTPUT_PRELOAD_CAP, selectMountedNotebookOutputs } from "./NotebookPreview.logic";

const range = (from: number, to: number) =>
  Array.from({ length: to - from }, (_, index) => from + index);

describe("selectMountedNotebookOutputs", () => {
  it("keeps an output leaving a crowded view mounted while it stays inside the margin", () => {
    const visible = range(1, NOTEBOOK_OUTPUT_PRELOAD_CAP + 2);
    const mounted = new Set(range(0, NOTEBOOK_OUTPUT_PRELOAD_CAP + 1));
    const nearby = new Set(range(0, NOTEBOOK_OUTPUT_PRELOAD_CAP + 3));

    const next = selectMountedNotebookOutputs(new Set(visible), nearby, mounted);

    expect(next.has(0)).toBe(true);
    expect([...next].toSorted((left, right) => left - right)).toEqual(
      range(0, NOTEBOOK_OUTPUT_PRELOAD_CAP + 2),
    );
    // Past the margin it goes.
    expect(selectMountedNotebookOutputs(new Set(visible), new Set(visible), next).has(0)).toBe(
      false,
    );
  });
});
