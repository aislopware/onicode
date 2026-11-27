import { describe, expect, it } from "vite-plus/test";

import { toHtml } from "hast-util-to-html";
import remarkGfm from "remark-gfm";
import remarkParse from "remark-parse";
import remarkRehype from "remark-rehype";
import { unified } from "unified";

import {
  cutSourceLines,
  extendMarkdownSplit,
  LARGE_MARKDOWN_CHUNK_MAX_CHARS,
  markdownHeadingPiece,
  splitMarkdown,
  withMarkdownDefinitions,
} from "./markdownChunks.ts";
import { maskMarkdownFrontmatter } from "./markdownFrontmatter.ts";
import { taskListMarkerOffset } from "./markdownTaskEdits.ts";

const splitLargeMarkdown = (text: string, targetChars?: number) =>
  splitMarkdown(text, targetChars).chunks;

const starts = (text: string, complete?: boolean) =>
  splitMarkdown(text, 1, complete).chunks.map((chunk) => chunk.text.split("\n")[0]);

describe("splitMarkdown", () => {
  it("cuts only where a new top-level block starts, and keeps every byte", () => {
    const text = [
      "# Title",
      "Intro paragraph",
      "",
      "- item one",
      "",
      "  continued item",
      "- item two",
      "",
      "2. ordered",
      "",
      "```md",
      "# not a heading",
      "",
      "Not a paragraph",
      "```",
      "",
      "> quote",
      "",
      "Closing paragraph",
      "## Next",
      "---",
      "",
      "| a | b |",
      "",
    ].join("\n");
    const chunks = splitLargeMarkdown(text, 1);
    expect(chunks.map((chunk) => chunk.text).join("\n")).toBe(text);
    expect(chunks.reduce((sum, chunk) => sum + chunk.lines, 0)).toBe(text.split("\n").length);
    expect(starts(text)).toEqual([
      "# Title",
      "```md",
      "> quote",
      "Closing paragraph",
      "## Next",
      "| a | b |",
    ]);
  });

  it("keeps a fence open until a matching closer", () => {
    const text = ["````", "```", "", "# inside", "````", "", "after", ""].join("\n");
    expect(starts(text)).toEqual(["````", "after"]);
  });

  it("treats nonbreaking spaces as content rather than fence suffixes or blank lines", () => {
    const fence = ["```md", "x".repeat(4100), "```\u00a0", "# literal", "```"].join("\n");
    expect(splitLargeMarkdown(fence)).toHaveLength(1);

    const prose = ["*" + "x".repeat(4100), "\u00a0", "more*"].join("\n");
    expect(splitLargeMarkdown(prose)).toHaveLength(1);

    const htmlBlock = ["<div>", "x".repeat(4100), "\u00a0", "# literal", "</div>"].join("\n");
    expect(splitLargeMarkdown(htmlBlock)).toHaveLength(1);
  });

  it("still closes fences and recognizes blank lines with ASCII spaces, tabs, and CRLF", () => {
    // A trailing line break, so the last line is whole and may be a cut.
    const fence = ["```md", "x".repeat(4100), "``` \t", "# heading", ""].join("\r\n");
    expect(splitLargeMarkdown(fence).map((chunk) => chunk.text.split("\r\n")[0])).toEqual([
      "```md",
      "# heading",
    ]);

    const prose = ["a".repeat(4100), " \t", "next paragraph", ""].join("\r\n");
    expect(splitLargeMarkdown(prose).map((chunk) => chunk.text.split("\r\n")[0])).toEqual([
      "a".repeat(4100),
      "next paragraph",
    ]);

    const htmlBlock = ["<div>", "x".repeat(4100), " \t", "# heading", ""].join("\r\n");
    expect(splitLargeMarkdown(htmlBlock).map((chunk) => chunk.text.split("\r\n")[0])).toEqual([
      "<div>",
      "# heading",
    ]);
  });

  it("keeps HTML blocks and display math open across blank lines", () => {
    const text = [
      "<!--",
      "",
      "# hidden",
      "-->",
      "",
      "$$",
      "",
      "Inside math",
      "$$",
      "",
      "<pre>",
      "",
      "Preformatted",
      "</pre>",
      "",
      "<!-- one line -->",
      "",
      "<div>",
      "# literal",
      "</div>",
      "",
      "<details>",
      "<summary>Answer</summary>",
      "",
      "Inside details",
      "",
      "</details>",
      "",
      "<table><tr><td>",
      "",
      "```ts",
      "inside a cell",
      "```",
      "",
      "</td></tr></table>",
      "",
      '<ol start="10">',
      "",
      "<li>one</li>",
      "",
      "<li>two</li>",
      "",
      "</ol>",
      "",
      "```html",
      "<details>",
      "```",
      "",
      "after",
      "",
    ].join("\n");
    expect(starts(text)).toEqual(["<!--", "<details>", "<table><tr><td>", "```html", "after"]);
    const multiline = [
      "<details",
      '  class="faq"><!-- </details> -->',
      "",
      "Inside details",
      "",
      "</details>",
      "",
      "after",
      "",
    ].join("\n");
    expect(starts(multiline)).toEqual(["<details", "after"]);
    const unclosed = ["<details>", "", "x".repeat(32 * 1024), "", "after", ""].join("\n");
    expect(starts(unclosed)).toEqual(["<details>", "after"]);
    const afterBigFence = [
      "```",
      "x".repeat(34 * 1024),
      "```",
      "",
      "<details>",
      "<summary>Small</summary>",
      "",
      "Inside",
      "",
      "</details>",
      "",
      "after",
      "",
    ].join("\n");
    expect(starts(afterBigFence)).toEqual(["```", "<details>", "after"]);
    const closedInParagraph = [
      "<details>",
      "<summary>Plan</summary>",
      "",
      "Done </details>",
      "",
      "next",
      "",
      "last",
      "",
    ].join("\n");
    expect(starts(closedInParagraph)).toEqual(["<details>", "next", "last"]);
    const afterComment = [
      "<!-- note --> <details>",
      "",
      "Inside",
      "",
      "</details>",
      "",
      "after",
      "",
    ];
    expect(starts(afterComment.join("\n"))).toEqual(["<!-- note --> <details>", "after"]);
    const closerInComment = [
      "<details> <!-- </details>",
      "still a comment -->",
      "",
      "Inside",
      "",
      "</details>",
      "",
      "after",
      "",
    ].join("\n");
    expect(starts(closerInComment)).toEqual(["<details> <!-- </details>", "after"]);
    const literalTags = [
      "<details>",
      "",
      "<pre><details></pre>",
      "",
      "Use `</details>` to close it.",
      "",
      "    </details>",
      "",
      "<https://example.com> <details> in prose",
      "",
      "Inside",
      "",
      "</details>",
      "",
      "after",
      "",
    ];
    expect(starts(literalTags.join("\n"))).toEqual(["<details>", "after"]);
    const oneLineScript = ['<script>let s = "<details>";</script>', "", "next", ""];
    expect(starts(oneLineScript.join("\n"))).toEqual([
      '<script>let s = "<details>";</script>',
      "next",
    ]);
    const commentsAcrossLines = [
      "<details> <!-- start",
      "</details> -->",
      "",
      "Inside",
      "",
      "<!--",
      "</details>",
      "--> </details>",
      "",
      "after",
      "",
    ];
    expect(starts(commentsAcrossLines.join("\n"))).toEqual(["<details> <!-- start", "after"]);
    const closesInWrapper = (line: string) => ["<details>", "", line, "", "next", "", "last", ""];
    for (const line of [
      "<https://example.com> closes it </details>",
      "Write `<!--` then </details>",
      "Close ``</details>` here",
    ]) {
      expect(starts(closesInWrapper(line).join("\n"))).toEqual(["<details>", "next", "last"]);
    }
    // An HTML block ends the paragraph, so its unclosed comment was literal text.
    const literalComment = ["<details>", "", "Intro <!-- start", "</details> -->", "", "next", ""];
    expect(starts(literalComment.join("\n"))).toEqual(["<details>", "next"]);
    const staysOpen = (lines: string[]) => [
      ...lines,
      "",
      "Inside",
      "",
      "</details>",
      "",
      "after",
      "",
    ];
    for (const lines of [
      ["<details>", "", "$$ \\text{</details>} $$"],
      ["<details>", "", "Intro <!-- start", "more </details> -->"],
      ["<!-- note --> <details> <!-- start", "</details> -->"],
    ]) {
      expect(starts(staysOpen(lines).join("\n"))).toEqual([lines[0], "after"]);
    }
    const shortComment = ["<!--> <details>", "", "Inside", "", "</details>", "", "after", ""];
    expect(starts(shortComment.join("\n"))).toEqual(["<!--> <details>", "after"]);
    // A wrapper after a big fence, with no blank line to cut at before it.
    const wrapperAfterBigFence = [
      "```",
      "x".repeat(34 * 1024),
      "```",
      " <details>",
      "",
      "Inside",
      "",
      "</details>",
      "",
      "after",
      "",
    ];
    expect(starts(wrapperAfterBigFence.join("\n"))).toEqual(["```", "after"]);
    const closerInFence = [
      "<details>",
      "",
      "```",
      "</details>",
      "```",
      "",
      "Inside",
      "",
      "</details>",
      "",
      "after",
      "",
    ];
    expect(starts(closerInFence.join("\n"))).toEqual(["<details>", "after"]);
    // A piece held open past the render limit goes back to the held cut once its line ends.
    const head = ["<details>", "", "a".repeat(8 * 1024), "", ""].join("\n");
    const longLine = "x".repeat(LARGE_MARKDOWN_CHUNK_MAX_CHARS - head.length);
    expect(starts(head + longLine)).toEqual(["<details>"]);
    expect(starts(`${head}${longLine}x`)).toEqual(["<details>"]);
    expect(starts(`${head}${longLine}x`, true)).toEqual(["<details>", `${longLine}x`]);
    expect(starts(`${head}${longLine}x\n`)).toEqual(["<details>", `${longLine}x`]);
    expect(starts(`${head}\`\`\`json\n${longLine}\n\`\`\`\n\n</details>`)).toEqual([
      "<details>",
      "```json",
    ]);
    const closedThenLong = ["<details>", "", "intro", "", "para", "", "</details>", "", "- "];
    expect(
      starts(
        `${closedThenLong.join("\n")}${"x".repeat(LARGE_MARKDOWN_CHUNK_MAX_CHARS)}\n\nafter\n`,
      ),
    ).toEqual(["<details>", "after"]);
    const reopened = ["<details>", "", "intro", "", "para", "", "</details><details>", ""];
    expect(
      starts(
        `${reopened.join("\n")}${"x".repeat(LARGE_MARKDOWN_CHUNK_MAX_CHARS)}\n\n</details>\n\nafter\n`,
      ),
    ).toEqual(["<details>", "after"]);
    // A wrapper that opens and closes on one line, after an unclosed one stopped counting, closes
    // nothing that holds a cut.
    const stopped = [
      "<details>",
      "",
      "intro",
      "",
      "y".repeat(33 * 1024),
      "",
      "<span><table></table></span>",
      "",
    ];
    expect(
      starts(`${stopped.join("\n")}1. ${"z".repeat(LARGE_MARKDOWN_CHUNK_MAX_CHARS)}\n\nafter\n`),
    ).toEqual(["<details>", "y".repeat(33 * 1024), "after"]);
  });

  it("treats an autolink line as a paragraph, so a following fence still opens", () => {
    const text = ["<https://example.com>", "```md", "", "# literal", "```", "", "after", ""].join(
      "\n",
    );
    expect(starts(text)).toEqual(["<https://example.com>", "after"]);
  });

  it("waits for the target size before cutting", () => {
    const text = ["# a", "", "b", "", "# c", "", "d"].join("\n");
    expect(splitLargeMarkdown(text, 6).map((chunk) => chunk.text)).toEqual([
      "# a\n\nb\n",
      "# c\n\nd",
    ]);
  });

  it("never cuts at the last line, which may still be growing", () => {
    expect(starts("# a\n# b")).toEqual(["# a"]);
    expect(starts("# a\n# b\n")).toEqual(["# a", "# b"]);
    expect(starts("# a\n# b", true)).toEqual(["# a", "# b"]);
  });

  it("keeps leading frontmatter in the first piece", () => {
    const text = ["---", "title: x", "", "# not a heading", "---", "", "# Heading", ""].join("\n");
    expect(starts(text)).toEqual(["---", "# Heading"]);
  });

  it("cuts after a leading horizontal rule with no frontmatter closer", () => {
    const md =
      "---\n\n" +
      Array.from({ length: 300 }, (_, i) => `## Part ${i}\n${"a".repeat(450)}`).join("\n\n");
    const { chunks } = splitMarkdown(md);
    expect(chunks.length).toBeGreaterThan(1);
    expect(chunks.every((chunk) => chunk.text.length <= LARGE_MARKDOWN_CHUNK_MAX_CHARS)).toBe(true);
    expect(chunks.map((chunk) => chunk.text).join("\n")).toBe(md);
  });

  it.each([
    ["one space", " # References"],
    ["two spaces", "  ### References"],
    ["three spaces", "   ###### References"],
  ])("collects definitions after headings indented by %s without cutting there", (_, heading) => {
    const intro = "[Release notes][release]\n";
    const definition = "[release]: /releases";
    const appendix = ["## Appendix", heading, definition].join("\n");
    const result = splitMarkdown(`${intro}\n${appendix}`, 1);

    expect(result.definitions).toBe(definition);
    expect(result.chunks.map((chunk) => chunk.text)).toEqual([intro, appendix]);
  });

  it("agrees with the frontmatter mask on where frontmatter ends", () => {
    const withCloserAfter = (lines: number) =>
      [
        "---",
        "# inside",
        ...Array.from({ length: lines - 2 }, () => "a: 1"),
        "---",
        "# after",
        "",
      ].join("\n");
    for (const [closerLine, expected] of [
      [1000, ["---", "# after"]],
      [1001, ["---", "# inside", "# after"]],
    ] as const) {
      const text = withCloserAfter(closerLine);
      expect(starts(text)).toEqual(expected);
      // Desktop masks each piece, mobile the whole file.
      const [first] = splitLargeMarkdown(text, 1);
      expect(maskMarkdownFrontmatter(first!.text)).toBe(
        maskMarkdownFrontmatter(text).slice(0, first!.text.length),
      );
    }
  });

  it("keeps a file that defines a footnote whole until it is too large to parse at once", () => {
    const text = [
      "# One",
      "",
      "See [^note].",
      "",
      "# Two",
      "",
      "[^note]: In another piece.",
      "",
    ].join("\n");
    expect(splitMarkdown(text, 1)).toEqual({ chunks: [{ text, lines: 8 }], definitions: "" });
    const filler = "# Filler\n\nparagraph\n\n";
    const large = text + filler.repeat(LARGE_MARKDOWN_CHUNK_MAX_CHARS / filler.length);
    expect(splitLargeMarkdown(large, 1).length).toBeGreaterThan(1);
  });

  it.each(["", " "])("collects link definitions outside code with fence indent %j", (indent) => {
    const text = [
      "[![badge][b]][site]",
      "",
      "```md",
      "[fake]: /not-a-definition",
      "```",
      "",
      "Paragraph",
      "[inline]: /continues-the-paragraph",
      "",
      "[site]: https://example.com",
      "[b]: https://example.com/badge.svg 'Badge'",
      "[^note]: a footnote",
      "",
      '[x]: /url "title" is paragraph text',
      "",
      "# Heading",
      "[h]: /after-heading",
      "",
      "---",
      "[t]: /after-break",
      "",
      "===",
      "[p]: /paragraph-text",
      "",
      `${indent}\`\`\`sh`,
      "npm install",
      `${indent}\`\`\``,
      "[fence]: /after-fence",
      "~~~",
      "~~~",
      "Text after a fence",
      "[q]: /continues-the-paragraph",
      "",
      "$$",
      "x",
      "$$ trailing",
      "[math]: /inside-math",
      "",
      "<!-->",
      "```",
      "-->",
      "```",
      "[comment]: /after-a-closed-comment",
      "```",
      "[code]: /inside-a-fence-after-a-comment",
      "```",
      "",
      "- ```md",
      "  ~~~",
      "  ~~~",
      "  [list]: /inside-a-list-fence",
      "  ```",
      "",
      // Past the whole-file limit, so the footnote does not keep the file one piece.
      "filler ".repeat(LARGE_MARKDOWN_CHUNK_MAX_CHARS / 7 + 1),
    ].join("\n");
    expect(splitMarkdown(text).definitions).toBe(
      "[site]: https://example.com\n[b]: https://example.com/badge.svg 'Badge'\n[h]: /after-heading\n[t]: /after-break\n[fence]: /after-fence\n[comment]: /after-a-closed-comment",
    );
    const frontmatter = ["---", "title: x", "...", "[fm]: /after-frontmatter", "", "# A"];
    expect(splitMarkdown(frontmatter.join("\n")).definitions).toBe("[fm]: /after-frontmatter");
    const unclosed = ["---", ...Array<string>(1000).fill("text"), "[u]: /paragraph-text"];
    expect(splitMarkdown(unclosed.join("\n")).definitions).toBe("");
    expect(splitMarkdown("---\n[rule]: /after-leading-rule\n\n# A").definitions).toBe(
      "[rule]: /after-leading-rule",
    );
    const unbalanced = [
      "[x]: /docs/example)",
      "[after]: /continues-paragraph",
      "",
      "[open]: /docs/example(",
      "",
      "[bs]: /docs/example\\\\)",
      "",
      "[balanced]: /docs/example(a(b))",
      "[escaped]: /docs/example\\)",
      "[angle]: </docs/example)>",
    ];
    expect(splitMarkdown(unbalanced.join("\n")).definitions).toBe(
      "[balanced]: /docs/example(a(b))\n[escaped]: /docs/example\\)\n[angle]: </docs/example)>",
    );
  });
});

const footnoteFiller = "# Section\n\nparagraph text\n\n";
const largeFootnoteText =
  "[^n]: note\n\n" +
  footnoteFiller.repeat(Math.ceil(LARGE_MARKDOWN_CHUNK_MAX_CHARS / footnoteFiller.length) + 2);

describe("extendMarkdownSplit", () => {
  it("splits again for a shorter text, another piece size, or no line break at the resume point", () => {
    const previous = splitMarkdown("# A\n\nfirst text\n\n# B\n\nmore\n", 8);
    for (const text of ["# A\n\nfirst", "# Other\n\nsecond text\n\n# B\n\nmore and more\n"]) {
      expect(extendMarkdownSplit(previous, text, 8)).toEqual(splitMarkdown(text, 8));
    }
    // At 4 the text is cut before `# B`; at 16 it is not, so the earlier piece cannot be reused.
    const small = splitMarkdown("# A\n\nab\n\n# B\n\ncd\n", 4);
    expect(small.chunks.length).toBeGreaterThan(1);
    const longer = "# A\n\nab\n\n# B\n\ncd\n\n# C\n";
    expect(extendMarkdownSplit(small, longer, 16)).toEqual(splitMarkdown(longer, 16));
  });

  // Cuts before `reusesFrom` change earlier pieces: the tail closes the frontmatter or makes the
  // text one piece.
  const frontmatter = "---\ntitle: x\n\n# not\n---\n\n# Heading\n\nbody\n\n# Next\n";
  const footnote = "# A\n\nsee [^n]\n\n# B\n\ntext\n\n[^n]: note\n";
  const lines = (count: number) => "x\n".repeat(count);
  // The heading inside is cut before the closer arrives. A closer on the 1,000th line after the
  // opener makes the block frontmatter; one line later it is a thematic break.
  const lateFrontmatter = `---\na: 12345\n# inside\n${lines(997)}---\n\n# H\n\npara\n\n# I\n`;
  const noFrontmatter = `---\na: 12345\n# inside\n${lines(997)}# late\n---\n\n# H\n\npara\n\n# I\n`;
  // The first definition fits under the shared cap, so the cap is nearly spent past the cut: the
  // longer one after it no longer fits, the short one does.
  const cappedDefinitions = `# A\n\n[a]: /${"a".repeat(16_000)}\n\n# B\n\n[b]: /${"b".repeat(400)}\n\n[c]: /c\n\n# C\n`;
  const heldPastLimit = `<details>\n\nintro\n\npara ${"x".repeat(LARGE_MARKDOWN_CHUNK_MAX_CHARS)}\n\n</details>\n\n# B\n`;
  const finishedPastLimit = `<details>\n\nintro\n\npara ${"x".repeat(LARGE_MARKDOWN_CHUNK_MAX_CHARS)}`;
  const near = (text: string, marker: string, offsets: readonly number[]) =>
    offsets.map((offset) => text.indexOf(marker) + offset);
  it.each([
    {
      name: "headings",
      text: "# A\n\npara one\n\n# B\n\npara two\n\n## C\ntext\n\n- list\n\n# D\n",
    },
    {
      name: "a fence spanning the cut",
      text: "# A\n\nintro\n\n```md\n# not\n\nnot a paragraph\n```\n\nafter\n\n# B\n",
    },
    { name: "comments and math", text: "<!--\n\n# hidden\n-->\n\n$$\n\nx\n$$\n\n# After\n\nend" },
    {
      name: "a details block spanning blank lines",
      text: "# A\n\nintro\n\n<details>\n<summary>S</summary>\n\nInside\n\n# kept\n\n</details>\n\nafter\n\n# B\n",
    },
    {
      name: "definitions in the last piece",
      text: "# A\n\n[a]: /one\n\nuse [a] and [b]\n\n# B\n\ntext\n\n[b]: /two 'Two'\n[c]: /three\n",
    },
    { name: "frontmatter", text: frontmatter, reusesFrom: frontmatter.indexOf("\n\n# Heading") },
    { name: "CRLF line ends", text: "# A\r\n\r\ntext\r\n\r\n# B\r\n\r\nmore\r\n" },
    { name: "a footnote in the last piece", text: footnote, reusesFrom: footnote.length + 1 },
    {
      name: "a frontmatter closer at its last allowed line",
      text: lateFrontmatter,
      reusesFrom: lateFrontmatter.indexOf("\n\n# H"),
    },
    { name: "a frontmatter closer past its last allowed line", text: noFrontmatter },
    {
      name: "definitions past the shared cap",
      text: cappedDefinitions,
      cuts: [
        ...near(cappedDefinitions, "# B", [0, 2]),
        ...near(cappedDefinitions, "[b]", [10, 410]),
        ...near(cappedDefinitions, "[c]", [3, 9]),
        ...near(cappedDefinitions, "# C", [0, 3]),
      ],
    },
    {
      name: "a footnote file crossing the whole-file limit",
      text: largeFootnoteText,
      cuts: [
        LARGE_MARKDOWN_CHUNK_MAX_CHARS - 7,
        LARGE_MARKDOWN_CHUNK_MAX_CHARS,
        LARGE_MARKDOWN_CHUNK_MAX_CHARS + 5,
        largeFootnoteText.length - 3,
      ],
    },
    {
      name: "a details piece held past the render limit",
      text: heldPastLimit,
      cuts: [
        ...near(heldPastLimit, "para", [0, 3, LARGE_MARKDOWN_CHUNK_MAX_CHARS - 20]),
        ...near(heldPastLimit, "\n\n</details>", [0, 1, 2, 3]),
        heldPastLimit.length - 2,
      ],
    },
    {
      name: "a finished text whose last line passes the render limit",
      text: finishedPastLimit,
      complete: true,
      cuts: [
        ...near(finishedPastLimit, "para", [0, 3, LARGE_MARKDOWN_CHUNK_MAX_CHARS - 20]),
        finishedPastLimit.length,
      ],
    },
  ])("splits $name as if split whole", ({ text, cuts, reusesFrom = 0, complete }) => {
    const whole = splitMarkdown(text, 8, complete);
    let streamed = splitMarkdown("", 8);
    for (const cut of cuts ?? Array.from({ length: text.length + 1 }, (_, index) => index)) {
      const previous = splitMarkdown(text.slice(0, cut), 8);
      const extended = extendMarkdownSplit(previous, text, 8, complete);
      expect(extended).toEqual(whole);
      if (cut >= reusesFrom) {
        for (const [index, chunk] of previous.chunks.slice(0, -1).entries()) {
          expect(extended.chunks[index]).toBe(chunk);
        }
      }
      streamed = extendMarkdownSplit(streamed, text.slice(0, cut), 8);
      expect(streamed).toEqual(previous);
    }
  });
});

describe("cutSourceLines", () => {
  it("cuts at line breaks, and inside a line only when it is too long", () => {
    expect(cutSourceLines("ab\ncd\nef", 5)).toEqual(["ab\ncd", "ef"]);
    expect(cutSourceLines("abcdefg\nh", 3)).toEqual(["abc", "def", "g\nh"]);
    expect(cutSourceLines("a\u{1F600}b", 2)).toEqual(["a", "\u{1F600}", "b"]);
    // A run too short for a surrogate pair still moves on.
    expect(cutSourceLines("\u{1F600}", 1).join("")).toBe("\u{1F600}");
  });
});

describe("markdownHeadingPiece", () => {
  // regression: a link to a heading in a piece the list had not mounted did not scroll
  it("finds the piece of a linked heading, numbering duplicates across the whole file", () => {
    const chunks = [
      "---\ntitle: Intro\n---\n# Intro\n\nSee [setup](#setup-1).",
      "## Setup\n\n```md\n# Setup\nSetup\n---\n```",
      "# Intro\n\nSetup\n=====",
      "## **Bold** [link](https://example.com) `snake_case` _em_\n\nIntro\n---\n\n- Intro\n---",
    ].map((text) => ({ text, lines: text.split("\n").length }));
    expect(markdownHeadingPiece(chunks, "intro")).toEqual({ index: 0, id: "intro" });
    expect(markdownHeadingPiece(chunks, "setup-1")).toEqual({ index: 2, id: "setup" });
    expect(markdownHeadingPiece(chunks, "bold-link-snake_case-em")).toEqual({
      index: 3,
      id: "bold-link-snake_case-em",
    });
    // Each piece renders alone, so it numbers its own duplicates from the start.
    expect(markdownHeadingPiece(chunks, "intro-2")).toEqual({ index: 3, id: "intro" });
    expect(markdownHeadingPiece(chunks, "intro-3")).toBeNull();
    // Rendered text decodes character references; code spans keep them literal.
    const entities = [{ text: "# Caf&eacute; &#x2014; `&amp;`", lines: 1 }];
    expect(markdownHeadingPiece(entities, "café--amp")).toEqual({ index: 0, id: "café--amp" });
    // A decoded underscore is text, never emphasis.
    const underscores = [{ text: "# &#95;init&#x5f;", lines: 1 }];
    expect(markdownHeadingPiece(underscores, "_init_")).toEqual({ index: 0, id: "_init_" });
  });
});

describe("withMarkdownDefinitions", () => {
  const html = (text: string) => {
    const processor = unified().use(remarkParse).use(remarkRehype);
    return toHtml(processor.runSync(processor.parse(text)));
  };

  it("keeps the first document definition ahead of later chunk definitions", () => {
    const text = "[id]: /first\n\n" + "x".repeat(4100) + "\n\n# Next\n[id]: /later\n\n[id]";
    const { chunks, definitions } = splitMarkdown(text);
    expect(html(text)).toContain('href="/first"');
    expect(chunks).toHaveLength(2);
    expect(html(withMarkdownDefinitions(chunks[1]!.text, definitions))).toContain('href="/first"');
    expect(chunks.map((chunk) => chunk.text).join("\n")).toBe(text);
  });

  it("keeps frontmatter hidden and task offsets shifted by exactly the prefix", () => {
    const text = "\uFEFF---\r\ntitle: hidden\r\n---\r\n\r\n[ref]: /x\r\n\r\n- [ ] task";
    const { definitions } = splitMarkdown(text);
    const rendered = withMarkdownDefinitions(text, definitions);
    expect(html(rendered)).not.toContain("hidden");
    const list = unified()
      .use(remarkParse)
      .use(remarkGfm)
      .parse(rendered)
      .children.find((node) => node.type === "list");
    const marker = taskListMarkerOffset(rendered, list!.position!.start.offset!);
    expect(marker! - (definitions.length + 2)).toBe(text.indexOf("[ ]"));
  });

  it("keeps shared definitions out of a trailing unclosed code fence", () => {
    const text = "[ref]: /dst\n\n```text\nshown";
    const { definitions } = splitMarkdown(text);
    expect(html(withMarkdownDefinitions(text, definitions))).toBe(html(text));
  });
});
