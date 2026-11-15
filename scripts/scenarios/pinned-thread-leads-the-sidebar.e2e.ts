import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

test(
  "A pinned thread leads the sidebar until it is unpinned",
  WEB,
  async ({ onicode, desktop, app, screen }) => {
    const project = await onicode.project("demo");
    await onicode.thread({ project, title: "Older work", messages: ["first"] });
    await onicode.thread({ project, title: "Newer work", messages: ["first"] });
    await onicode.open();
    const sidebar = screen.getByRole("navigation", "Threads");
    const rows = sidebar.getByTestId("sidebar-row-card");
    const older = rows.filter({ hasText: "Older work" });
    await expect(rows.first()).toContainText("Newer work");

    await desktop.chooseFromContextMenu(older, "Pin thread");
    await expect(older.getByRole("button", "Unpin thread")).toBeVisible();
    await expect(rows.first()).toContainText("Older work");
    await app.screenshot("pinned");

    await desktop.chooseFromContextMenu(older, "Unpin thread");
    await expect(rows.first()).toContainText("Newer work");
    await expect(older.getByRole("button", "Unpin thread")).toBeHidden();
  },
);
