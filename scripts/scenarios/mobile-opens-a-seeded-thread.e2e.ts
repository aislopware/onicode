import { expect } from "e2e";

import { IOS, test } from "./oni-mobile.ts";

test(
  "The iOS app lists a seeded thread and opens its history",
  IOS,
  async ({ onicode, app, screen }) => {
    const project = await onicode.project("demo");
    await onicode.thread({ project, title: "Earlier work", messages: ["first", "second"] });
    await onicode.open();

    const thread = screen.getByText("Earlier work", { exact: false }).first();
    await expect(thread).toBeVisible({ timeout: 120_000 });
    await app.screenshot("home");

    await thread.tap();
    await expect(screen.getByText("second fixture turn complete", { exact: false })).toBeVisible();
    await app.screenshot("thread");
  },
);
