import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

test(
  "With no update feed the app says so, in the sidebar and from the menu",
  WEB,
  async ({ onicode, desktop, app, screen }) => {
    await onicode.project("demo");
    await onicode.open();
    await expect(
      screen.getByRole("button", /^Updates unavailable: .*no update feed is configured/),
    ).toBeDisabled();
    await app.screenshot("sidebar");

    await desktop.chooseApplicationMenuItem("Help", "Check for Updates...");
    await expect
      .poll(() => desktop.takeMessageBoxes())
      .toEqual([
        expect.objectContaining({
          title: "Updates unavailable",
          detail: expect.stringContaining("no update feed is configured"),
        }),
      ]);
    expect((await desktop.window()).windowCount).toBe(1);
  },
);
