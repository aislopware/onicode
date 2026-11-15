import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

test(
  "A seeded thread opens by its link and shows its history",
  WEB,
  async ({ onicode, app, screen }) => {
    const project = await onicode.project("demo");
    const thread = await onicode.thread({
      project,
      title: "Earlier work",
      messages: ["first", "second"],
    });
    await onicode.open(thread);

    await expect(screen.getByText("second fixture turn complete", { exact: false })).toBeVisible();
    await app.screenshot("thread");
  },
);
