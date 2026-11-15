import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

test(
  "An archived thread leaves the sidebar and comes back when Settings unarchives it",
  WEB,
  async ({ onicode, desktop, app, screen }) => {
    const project = await onicode.project("demo");
    await onicode.thread({ project, title: "Archive me", messages: ["first"] });
    await onicode.open();
    const sidebar = screen.getByRole("navigation", "Threads");
    const row = sidebar.getByTestId("sidebar-row-card").filter({ hasText: "Archive me" });

    await desktop.chooseFromContextMenu(row, "Archive thread");
    await expect(row).toBeHidden();

    await sidebar.getByRole("button", "Settings").tap();
    const settings = screen.getByRole("dialog", "Settings");
    await settings.getByRole("button", "Archive").tap();
    await app.screenshot("archive-list");
    await settings.getByRole("button", "Unarchive Archive me").tap();
    await expect(settings.getByRole("button", "Unarchive Archive me")).toBeHidden();
    await settings.getByRole("region", "Settings content").getByRole("button", "Close").tap();

    await expect(row).toBeVisible();
    await row.tap();
    await expect(screen.getByText("first fixture turn complete")).toBeVisible();
  },
);
