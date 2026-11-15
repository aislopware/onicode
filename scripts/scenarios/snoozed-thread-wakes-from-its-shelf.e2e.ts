import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

test(
  "Snoozing a thread until tomorrow shelves it, and waking brings it back",
  WEB,
  async ({ onicode, desktop, app, screen }) => {
    const project = await onicode.project("demo");
    await onicode.thread({ project, title: "Snooze me", messages: ["first"] });
    await onicode.open();
    const sidebar = screen.getByRole("navigation", "Threads");
    const card = sidebar.getByTestId("sidebar-row-card").filter({ hasText: "Snooze me" });
    const slim = sidebar.getByTestId("sidebar-row-slim").filter({ hasText: "Snooze me" });

    await desktop.chooseFromContextMenu(card, "Snooze", "Tomorrow (9:00 AM)");
    await expect(screen.getByText(/^Snoozed 1 thread/)).toBeVisible();
    await expect(card).toBeHidden();
    const shelf = sidebar.getByTestId("sidebar-snoozed-shelf-toggle");
    if ((await shelf.getAttribute("aria-expanded")) !== "true") await shelf.tap();
    await expect(slim).toBeVisible();
    await app.screenshot("snoozed");

    await desktop.chooseFromContextMenu(slim, "Wake thread");
    await expect(card).toBeVisible();
    await expect(slim).toBeHidden();
  },
);
