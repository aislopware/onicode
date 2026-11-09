import { describe, expect, it } from "vite-plus/test";

import { normalizeDesktopUpdateReleaseNotes } from "./releaseNotes.ts";

describe("normalizeDesktopUpdateReleaseNotes", () => {
  it("shows the newest changes and counts all real changes", () => {
    const result = normalizeDesktopUpdateReleaseNotes(
      [
        "- feat: first change",
        "- fix: second change",
        "- fix: third change",
        "- fix: fourth change",
        "- fix: fifth change",
        "- fix: sixth change",
        "- fix: seventh change",
        "- fix: eighth change",
        "- fix(web): keep long task drawers usable on small screens by @human in #8313",
        "- fix(opencode): handle child approvals, stops, and model catalogs by @human in #8480",
        "## New Contributors",
        "- @human made their first contribution in #8435",
        "**Full Changelog**: https://github.com/aislopware/onicode/compare/old...new",
      ].join("\n"),
      "0.0.36",
    );

    expect(result).toEqual({
      releaseNotes: [
        {
          version: "0.0.36",
          items: [
            "fix(opencode): handle child approvals, stops, and model catalogs by @human in #8480",
            "fix(web): keep long task drawers usable on small screens by @human in #8313",
            "fix: eighth change",
            "fix: seventh change",
            "fix: sixth change",
            "fix: fifth change",
            "fix: fourth change",
            "fix: third change",
          ],
          totalItems: 10,
        },
      ],
      omittedReleaseCount: 0,
    });
  });

  it("excludes a GitHub HTML contributor section", () => {
    const result = normalizeDesktopUpdateReleaseNotes(
      "<h2>What's Changed</h2><ul><li>Older fix</li><li>Newer fix</li></ul>" +
        "<h2>New Contributors</h2><ul><li>@human made their first contribution</li></ul>" +
        "<h2>Full Changelog</h2>",
      "1.2.3",
    );

    expect(result).toEqual({
      releaseNotes: [{ version: "1.2.3", items: ["Newer fix", "Older fix"], totalItems: 2 }],
      omittedReleaseCount: 0,
    });
  });

  it("reads a generated summary as a note and skips the folded full list", () => {
    const fixes = Array.from({ length: 9 }, (_, index) => `- Fix ${index + 1} (#${index + 1})`);
    const result = normalizeDesktopUpdateReleaseNotes(
      [
        "**v1.2.4** · 12 changes from 3 contributors",
        "## Highlights",
        "Link machines (beta).",
        "## Fixes",
        ...fixes,
        "<details>",
        "<summary>Full changelog</summary>",
        "* fix: raw entry by @human in https://github.com/aislopware/onicode/pull/1",
        "</details>",
      ].join("\n"),
      "1.2.4",
      "2026-10-29T10:00:00.000Z",
    );

    expect(result.releaseNotes).toEqual([
      {
        version: "1.2.4",
        releasedAt: "2026-10-29T10:00:00.000Z",
        stats: "12 changes from 3 contributors",
        intro: "Link machines (beta).",
        items: fixes.map((_, index) => `Fix ${index + 1}`),
        totalItems: 10,
      },
    ]);
  });

  it.each([
    "**v1.2.4** · 3 changes from 2 contributors\n- **One-time tasks.** Pick a date.\n\n### Plus 2 more bug fixes and performance improvements.",
    "<p><strong>v1.2.4</strong> · 3 changes from 2 contributors</p><ul><li><strong>One-time tasks.</strong> Pick a date.</li></ul><h3>Plus 2 more bug fixes and performance improvements.</h3>",
  ])("keeps highlight labels and the remaining change count from the feed", (summary) => {
    const { releaseNotes } = normalizeDesktopUpdateReleaseNotes(
      summary + "\n<details><summary>Full changelog</summary>raw list</details>",
      "1.2.4",
    );
    expect(releaseNotes).toEqual([
      {
        version: "1.2.4",
        stats: "3 changes from 2 contributors",
        items: ["**One-time tasks.** Pick a date."],
        totalItems: 1,
        footer: "Plus 2 more bug fixes and performance improvements.",
      },
    ]);
  });

  it("does not count Markdown or HTML section headings as changes", () => {
    const changes = Array.from({ length: 8 }, (_, index) => `Change ${index + 1}`);
    const result = normalizeDesktopUpdateReleaseNotes(
      [
        { version: "1.2.4", note: ["### Features", ...changes].join("\n- ") },
        {
          version: "1.2.3",
          note: `<h3>Fixes</h3><ul>${changes.map((change) => `<li>${change}</li>`).join("")}</ul>`,
        },
      ],
      "1.2.4",
    );

    expect(result.releaseNotes).toEqual([
      { version: "1.2.4", items: changes.toReversed(), totalItems: 8 },
      { version: "1.2.3", items: changes.toReversed(), totalItems: 8 },
    ]);
  });

  it("reads handwritten notes with their own <details> as a list", () => {
    const result = normalizeDesktopUpdateReleaseNotes(
      "- Older fix\n- Newer fix\n<details><summary>Logs</summary>\n- hidden\n</details>",
      "1.2.3",
    );

    expect(result.releaseNotes).toEqual([
      { version: "1.2.3", items: ["hidden", "Logs", "Newer fix", "Older fix"], totalItems: 4 },
    ]);
  });

  it("dates only the offered release, from the feed", () => {
    const result = normalizeDesktopUpdateReleaseNotes(
      [
        { version: "0.0.42", note: "- Change 2" },
        { version: "0.0.41", note: "- Change 1" },
      ],
      "0.0.42",
      "2026-09-14T18:30:00.000Z",
    );

    expect(result.releaseNotes.map(({ releasedAt }) => releasedAt)).toEqual([
      "2026-09-14T18:30:00.000Z",
      undefined,
    ]);
  });

  it("counts valid groups before applying the six-release limit", () => {
    const releaseNotes = [
      { version: "1.3.9", note: "- Change 9" },
      { version: "1.3.8", note: "Full changelog: https://example.com/compare/x...y" },
      { version: "1.3.7", note: "- Change 7" },
      { version: "1.3.6", note: "- Change 6" },
      { version: "1.3.5", note: "- Change 5" },
      { version: "1.3.4", note: "- Change 4" },
      { version: "1.3.3", note: "- Change 3" },
      { version: "1.3.2", note: "- Change 2" },
    ];

    const result = normalizeDesktopUpdateReleaseNotes(releaseNotes, "1.3.9");

    expect(result.releaseNotes.map(({ version }) => version)).toEqual([
      "1.3.9",
      "1.3.7",
      "1.3.6",
      "1.3.5",
      "1.3.4",
      "1.3.3",
    ]);
    expect(result.omittedReleaseCount).toBe(1);
  });

  it("ignores malformed and empty entries instead of throwing", () => {
    const result = normalizeDesktopUpdateReleaseNotes(
      [
        { version: "1.2.3", note: "- Valid change" },
        { version: "1.2.2", note: "" },
        { version: 42, note: "- Bad version type" },
        { version: "1.2.1", note: { html: "<p>object note</p>" } },
        "not an object",
        null,
      ],
      "1.2.3",
    );

    expect(result).toEqual({
      releaseNotes: [{ version: "1.2.3", items: ["Valid change"], totalItems: 1 }],
      omittedReleaseCount: 0,
    });
  });

  it("does not throw on out-of-range numeric entities and keeps the literal", () => {
    const result = normalizeDesktopUpdateReleaseNotes("- Broken entity &#9999999999;", "1.0.0");
    expect(result).toEqual({
      releaseNotes: [{ version: "1.0.0", items: ["Broken entity &#9999999999;"], totalItems: 1 }],
      omittedReleaseCount: 0,
    });
  });

  it("ignores the Full Changelog paragraph that ends GitHub's generated notes", () => {
    const fullChangelog = (from: string, to: string) =>
      `<p><strong>Full Changelog</strong>: <a class="commit-link" href="https://github.com/aislopware/onicode/compare/v${from}...v${to}"><tt>v${from}...v${to}</tt></a></p>`;
    const result = normalizeDesktopUpdateReleaseNotes(
      [
        {
          version: "0.1.4",
          note: fullChangelog("0.1.3", "0.1.4"),
        },
        {
          version: "0.1.3",
          note:
            "<h2>What's Changed</h2>\n<ul>\n" +
            '<li>fix(web): A by <a href="https://github.com/a">@a</a> in <a href="https://github.com/aislopware/onicode/pull/1">#1</a></li>\n' +
            '<li>fix(web): B by <a href="https://github.com/b">@b</a> in <a href="https://github.com/aislopware/onicode/pull/2">#2</a></li>\n' +
            "<li>Full Changelog: links open in the browser in #3</li>\n" +
            "</ul>\n" +
            fullChangelog("0.1.2", "0.1.3"),
        },
      ],
      "0.1.4",
    );

    expect(result).toEqual({
      releaseNotes: [
        {
          version: "0.1.3",
          items: [
            "Full Changelog: links open in the browser in #3",
            "fix(web): B by @b in #2",
            "fix(web): A by @a in #1",
          ],
          totalItems: 3,
        },
      ],
      omittedReleaseCount: 0,
    });
  });
});
