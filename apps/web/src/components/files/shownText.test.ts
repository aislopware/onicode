import { describe, expect, it } from "vite-plus/test";

import { lineCutMarkdown } from "./shownText";

const more = (count: number) => `${count} more`;

describe("lineCutMarkdown", () => {
  it.each([
    // The cut drops the comment's close, so the line closes it and the heading still renders.
    ["a comment closed past the cut", `<!-- ${"x".repeat(12_000)} -->`, " -->… "],
    // A comment that only closes on a later line stays open, as in the file.
    ["a comment closed on a later line", `<!-- ${"x".repeat(12_000)}\n-->`, "… "],
    // The cut falls inside the close itself.
    ["a close cut halfway", `<!-- ${"x".repeat(9_993)}-->${"y".repeat(2_000)}`, "-- -->… "],
    // In a fenced code block the delimiters are text, so nothing is added.
    ["a fenced code line", `\`\`\`\n<!-- ${"x".repeat(12_000)} -->\n\`\`\``, "… "],
    // A fence closes only on its own marker, and a fence inside a comment opens nothing.
    [
      "a fence holding a `~~~` line",
      `\`\`\`\n~~~\n\`\`\`\n<!-- ${"x".repeat(12_000)} -->`,
      " -->… ",
    ],
    ["a fence in a comment", `<!--\n\`\`\`\n-->\n<!-- ${"x".repeat(12_000)} -->`, " -->… "],
    // An opener inside a paragraph, here in a code span, hides no fence after it.
    [
      "a fence after an inline opener",
      `Write \`<!--\`\n\`\`\`\n<!-- ${"x".repeat(12_000)} -->\n\`\`\``,
      "… ",
    ],
  ])("keeps the document after %s", (_name, line, joint) => {
    const shown = lineCutMarkdown(`${line}\n\n# After`, more);
    expect(shown).toContain(`${"x".repeat(100)}${joint}`);
    expect(shown.endsWith("\n\n# After")).toBe(true);
  });
});
