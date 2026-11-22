import { describe, expect, it } from "vite-plus/test";

import { lineCutMarkdown } from "./shownText";

const more = (count: number) => `${count} more`;

describe("lineCutMarkdown", () => {
  it.each([
    // The cut drops the comment's close, so the line closes it and the heading still renders.
    ["a comment closed past the cut", `<!-- ${"x".repeat(12_000)} -->`, " -->… "],
    // A comment that only closes on a later line stays open, as in the file.
    ["a comment closed on a later line", `<!-- ${"x".repeat(12_000)}\n-->`, "… "],
  ])("keeps the document after %s", (_name, line, joint) => {
    const shown = lineCutMarkdown(`${line}\n\n# After`, more);
    expect(shown).toContain(`${"x".repeat(9_995)}${joint}`);
    expect(shown.endsWith("\n\n# After")).toBe(true);
  });
});
