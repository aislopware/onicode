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

    const history = screen.getByText("second fixture turn complete", { exact: false });
    await thread.tap();
    // A hosted runner's simulator sometimes drops the first tap after pairing: the row stays put
    // and nothing opens. The same build opens it every time on a developer's Mac.
    await history.waitFor({ state: "visible", timeout: 10_000 }).catch(() => thread.tap());
    await expect(history).toBeVisible();
    await app.screenshot("thread");
  },
);
