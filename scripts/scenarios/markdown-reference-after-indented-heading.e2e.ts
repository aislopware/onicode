import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

test(
  "A long Markdown preview resolves a reference defined after an indented heading",
  WEB,
  async ({ onicode, app, screen }) => {
    const markdown =
      "[Release notes][release]\n\n" +
      "Background for this release. ".repeat(180) +
      "\n\n## Appendix\n\n  ### References\n[release]: https://example.com/releases\n";
    const project = await onicode.project("documentation", { "release.md": markdown });
    const thread = await onicode.thread({
      project,
      title: "Release",
      messages: ['See :codex-file-citation{path="release.md"}'],
    });
    await onicode.open(thread);

    await screen.getByRole("button", /^Agent See/).tap();
    await screen.getByRole("link", /^release\.md/).tap();
    await screen.getByRole("button", "Show rendered markdown").tap();
    await screen.getByText("Background for this release.", { exact: false }).waitFor();
    await app.screenshot("release-reference");
    await expect(screen.getByRole("link", "Release notes")).toBeVisible();
  },
);
