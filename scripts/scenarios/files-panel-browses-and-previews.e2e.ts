import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

test(
  "The files panel lists the project, filters it, and previews a file",
  WEB,
  async ({ onicode, app, screen }) => {
    const project = await onicode.project("demo", {
      "README.md": "# demo\n\nAn orchard of fixtures.\n",
      "src/greeting.ts": 'export const greeting = "hello";\n',
    });
    const thread = await onicode.thread({ project, title: "Files home", messages: ["first"] });
    await onicode.open(thread);

    await screen.getByRole("button", "Toggle right panel").tap();
    await screen.getByRole("button", "Files F").tap();
    const panel = screen.getByRole("region", "Workspace panel: Files");
    await expect(panel.getByRole("treeitem", "README.md")).toBeVisible();
    await expect(panel.getByRole("treeitem", "src")).toBeVisible();

    await panel.getByRole("searchbox", "Search demo files").fill("greeting");
    // The search opens the folder that holds the match. Opening a file renames the panel after it.
    await screen.getByRole("treeitem", "greeting.ts").tap();
    await expect(screen.getByRole("textbox", "src/greeting.ts")).toHaveValue(
      /export const greeting/,
    );
    await app.screenshot("preview");
  },
);
