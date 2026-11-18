import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

test(
  "Esc from Settings opened in the command palette returns focus to the control it opened from",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    const project = await onicode.project("demo");
    const thread = await onicode.thread({ project, title: "Focus target", messages: ["first"] });
    await onicode.open(thread);
    const row = screen
      .getByRole("navigation", "Threads")
      .getByRole("button", "Focus target", { exact: false });
    const settings = screen.getByRole("dialog", "Settings");

    await row.tap();
    await expect(row).toBeFocused();
    await browser.keyboard.press("ControlOrMeta+k");
    await screen.getByTestId("command-palette").getByRole("combobox").fill("Open settings");
    await browser.keyboard.press("Enter");
    await expect(settings).toBeVisible();
    await browser.keyboard.press("Escape");
    await expect(settings).toBeHidden();

    await app.screenshot("after-esc");
    await expect(row).toBeFocused();
  },
);
