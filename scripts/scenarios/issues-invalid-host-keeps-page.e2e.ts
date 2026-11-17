import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

test(
  "An invalid host filter keeps the Issues page open",
  WEB,
  async ({ onicode, app, browser }) => {
    await onicode.project("demo");
    await onicode.open("/issues?host=true");

    await app.screenshot("issues-invalid-host");
    await expect(browser.locator("h1")).toHaveText("Issues");
  },
);
