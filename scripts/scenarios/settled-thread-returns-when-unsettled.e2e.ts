import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

test(
  "Settling a thread moves it to the Settled shelf, and un-settling brings it back",
  WEB,
  async ({ onicode, desktop, app, screen }) => {
    const project = await onicode.project("demo");
    await onicode.thread({ project, title: "Settle me", messages: ["first"] });
    await onicode.open();
    const sidebar = screen.getByRole("navigation", "Threads");
    const card = sidebar.getByTestId("sidebar-row-card").filter({ hasText: "Settle me" });
    const slim = sidebar.getByTestId("sidebar-row-slim").filter({ hasText: "Settle me" });

    // A row's controls show on hover, over its labels.
    await card.hover();
    await card.getByRole("button", "Settle thread").tap();
    await expect(card).toBeHidden();
    const shelf = sidebar.getByTestId("sidebar-settled-shelf-toggle");
    if ((await shelf.getAttribute("aria-expanded")) !== "true") await shelf.tap();
    await expect(slim).toBeVisible();
    await app.screenshot("settled");

    await desktop.chooseFromContextMenu(slim, "Un-settle thread");
    await expect(card).toBeVisible();
    await expect(slim).toBeHidden();
    await app.screenshot("unsettled");
  },
);
