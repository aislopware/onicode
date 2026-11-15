import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

test(
  "A follow-up message runs a second turn in the same thread",
  WEB,
  async ({ onicode, app, screen }) => {
    const project = await onicode.project("demo");
    const thread = await onicode.thread({ project, title: "Two turns", messages: ["first"] });
    await onicode.open(thread);
    await expect(screen.getByText("first fixture turn complete")).toBeVisible();

    const composer = screen.getByTestId("composer-editor");
    await composer.fill("And the second?");
    await composer.press("Enter");
    await expect(screen.getByText("second fixture turn complete")).toBeVisible();
    await expect(screen.getByText("first fixture turn complete")).toBeVisible();
    await expect(composer).toHaveValue("");
    await app.screenshot("second-turn");
  },
);
