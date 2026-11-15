import { expect } from "e2e";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";

import { ONICODE_HOME } from "../e2e.config.ts";
import { test, WEB } from "./desktop.ts";
import { runPromise } from "./onicode.ts";

const KEYBINDINGS = `${ONICODE_HOME}/userdata/keybindings.json`;

// Shortcut labels are symbols on macOS and words elsewhere.
const DEFAULT_BINDING = /^Edit shortcut for Sidebar: Toggle: (?:⌘B|Ctrl\+B)$/;
const REBOUND = /^Edit shortcut for Sidebar: Toggle: (?:⌃⌥9|Ctrl\+Alt\+9)$/;

test(
  "A rebound shortcut drives its command until the binding is reset",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    await onicode.project("demo");
    const original = await runPromise(
      Effect.flatMap(FileSystem.FileSystem, (fs) => fs.readFileString(KEYBINDINGS)),
    );
    try {
      await onicode.open("/settings/keybindings");
      const settings = screen.getByRole("dialog", "Settings");
      const sidebarToggle = screen.getByRole("button", "Toggle main sidebar");
      await settings.getByRole("searchbox", "Search keybindings").fill("Sidebar: Toggle");
      await expect(settings.getByRole("heading", "Command Palette: Toggle")).toBeHidden();

      await settings.getByRole("button", DEFAULT_BINDING).tap();
      await expect(settings.getByRole("textbox", "Keybinding for Sidebar: Toggle")).toBeFocused();
      await browser.keyboard.press("Control+Alt+9");
      await settings.getByRole("button", "Save").tap();
      await expect(settings.getByRole("button", REBOUND)).toBeVisible();
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
      await expect(settings.getByRole("button", DEFAULT_BINDING)).toBeVisible();
    } finally {
      // A run that stopped before the reset would leave every later scenario without ⌘B.
      await runPromise(
        Effect.flatMap(FileSystem.FileSystem, (fs) => fs.writeFileString(KEYBINDINGS, original)),
      );
    }
  },
);
