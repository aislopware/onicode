import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

test(
  "Renaming a thread from its context menu changes its title everywhere",
  WEB,
  async ({ onicode, desktop, app, screen }) => {
    const project = await onicode.project("demo");
    const thread = await onicode.thread({ project, title: "Old name", messages: ["first"] });
    await onicode.open(thread);
    const sidebar = screen.getByRole("navigation", "Threads");

    await desktop.chooseFromContextMenu(
      sidebar.getByTestId("sidebar-row-card").filter({ hasText: "Old name" }),
      "Rename thread",
    );
    const field = sidebar.getByRole("textbox", "Thread title");
    await expect(field).toHaveValue("Old name");
    await field.fill("New name");
    await field.press("Enter");

    await expect(sidebar.getByRole("button", /^New name, /)).toBeVisible();
    await expect(screen.getByRole("heading", "New name")).toBeVisible();
    await app.screenshot("renamed");
    await onicode.open(thread);
    await expect(screen.getByRole("heading", "New name")).toBeVisible();
  },
);
