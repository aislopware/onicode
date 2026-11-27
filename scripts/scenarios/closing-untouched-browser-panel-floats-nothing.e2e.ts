import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

test(
  "Closing the right panel on a Browser tab that never navigated floats nothing",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    const project = await onicode.project("demo");
    const thread = await onicode.thread({
      project,
      title: "Untouched browser",
      messages: ["first"],
    });
    await onicode.open(thread);

    await screen.getByRole("button", "Toggle right panel").tap();
    await screen.getByRole("button", "Browser B").tap();
    await screen.getByRole("textbox", "Search or enter URL").waitFor();
    await app.screenshot("browser-untouched-before-close");

    await screen.getByRole("button", "Toggle right panel").tap();
    await app.screenshot("after-close-untouched-browser");
    await expect(browser.locator('section[aria-label="Floating browser preview"]')).toBeHidden();
  },
);
