import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

test(
  "The command palette opens from the keyboard, runs a command, and jumps to a thread",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    const project = await onicode.project("demo");
    await onicode.thread({ project, title: "Palette target", messages: ["first"] });
    await onicode.open();
    const palette = screen.getByTestId("command-palette");
    const search = palette.getByRole("combobox");
    const settings = screen.getByRole("dialog", "Settings");

    await browser.keyboard.press("ControlOrMeta+k");
    await expect(search).toBeFocused();
    await browser.keyboard.press("ControlOrMeta+k");
    await expect(palette).toBeHidden();

    await browser.keyboard.press("ControlOrMeta+k");
    await search.fill("Open settings");
    await app.screenshot("palette");
    await palette.getByRole("option", "Open settings").tap();
    await expect(palette).toBeHidden();
    await expect(settings).toBeVisible();
    await settings.getByRole("region", "Settings content").getByRole("button", "Close").tap();
    // Closing returns to the page Settings opened over; a jump made before that lands is undone.
    await expect(settings).toBeHidden();

    await browser.keyboard.press("ControlOrMeta+k");
    await search.fill("Palette target");
    await palette.getByRole("option", /Palette target/).tap();
    await expect(screen.getByRole("heading", "Palette target")).toBeVisible();
    await expect(screen.getByText("first fixture turn complete")).toBeVisible();
  },
);
