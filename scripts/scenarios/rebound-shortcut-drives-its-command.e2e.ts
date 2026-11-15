import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

test(
  "A rebound shortcut drives its command until the binding is reset",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    await onicode.project("demo");
    await onicode.open("/settings/keybindings");
    const settings = screen.getByRole("dialog", "Settings");
    const sidebarToggle = screen.getByRole("button", "Toggle main sidebar");
    await settings.getByRole("searchbox", "Search keybindings").fill("Sidebar: Toggle");
    await expect(settings.getByRole("heading", "Command Palette: Toggle")).toBeHidden();

    await settings.getByRole("button", "Edit shortcut for Sidebar: Toggle: ⌘B").tap();
    await expect(settings.getByRole("textbox", "Keybinding for Sidebar: Toggle")).toBeFocused();
    await browser.keyboard.press("Control+Alt+9");
    await settings.getByRole("button", "Save").tap();
    await expect(
      settings.getByRole("button", "Edit shortcut for Sidebar: Toggle: ⌃⌥9"),
    ).toBeVisible();
    await app.screenshot("rebound");

    await settings.getByRole("region", "Settings content").getByRole("button", "Close").tap();
    await expect(sidebarToggle).toHaveAttribute("aria-pressed", "true");
    await browser.keyboard.press("Control+Alt+9");
    await expect(sidebarToggle).toHaveAttribute("aria-pressed", "false");
    await browser.keyboard.press("ControlOrMeta+b");
    await expect(sidebarToggle).toHaveAttribute("aria-pressed", "false");
    await browser.keyboard.press("Control+Alt+9");
    await expect(sidebarToggle).toHaveAttribute("aria-pressed", "true");

    await onicode.open("/settings/keybindings");
    await settings.getByRole("button", "Actions for Sidebar: Toggle").tap();
    await screen.getByRole("menuitem", "Reset to default").tap();
    await expect(
      settings.getByRole("button", "Edit shortcut for Sidebar: Toggle: ⌘B"),
    ).toBeVisible();
  },
);
