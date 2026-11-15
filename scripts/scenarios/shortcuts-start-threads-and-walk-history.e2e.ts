import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

test(
  "Shortcuts start a thread, walk the history, and fold the sidebar",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    const project = await onicode.project("demo");
    const thread = await onicode.thread({ project, title: "Shortcut home", messages: ["first"] });
    await onicode.open(thread);
    const heading = screen.getByRole("heading", "Shortcut home");
    const draft = screen.getByRole("textbox", "Message New thread");
    const sidebarToggle = screen.getByRole("button", "Toggle main sidebar");
    await expect(heading).toBeVisible();

    await browser.keyboard.press("ControlOrMeta+n");
    await expect(draft).toBeFocused();
    await browser.keyboard.press("ControlOrMeta+[");
    await expect(heading).toBeVisible();
    await browser.keyboard.press("ControlOrMeta+]");
    await expect(draft).toBeVisible();
    await screen.getByRole("navigation", "Threads").getByRole("button", "Go back").tap();
    await expect(heading).toBeVisible();

    await browser.keyboard.press("ControlOrMeta+b");
    await expect(sidebarToggle).toHaveAttribute("aria-pressed", "false");
    await app.screenshot("sidebar-folded");
    await browser.keyboard.press("ControlOrMeta+b");
    await expect(sidebarToggle).toHaveAttribute("aria-pressed", "true");
  },
);
