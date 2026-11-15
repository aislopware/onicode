import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

test(
  "A thread in the project checkout rewinds the chat but may not revert files",
  WEB,
  async ({ onicode, app, screen }) => {
    const project = await onicode.project("demo");
    const thread = await onicode.thread({ project, title: "Rewind me", messages: ["first"] });
    await onicode.open(thread);
    const chat = screen.getByRole("region", "Conversation");
    const confirm = screen.getByRole("alertdialog", "Edit from here?");
    await expect(chat.getByText("first fixture turn complete")).toBeVisible();

    // A message sent through the CLI shows collapsed under its sender.
    await chat.getByRole("button", /^Agent first/).tap();
    await chat.getByRole("button", "Edit from here").tap();
    await expect(confirm.getByRole("button", "Revert files too")).toBeDisabled();
    await app.screenshot("confirm");
    await confirm.getByRole("button", "Cancel").tap();
    await expect(chat.getByText("first fixture turn complete")).toBeVisible();

    await chat.getByRole("button", "Edit from here").tap();
    await confirm.getByRole("button", "Revert and keep changes").tap();
    await expect(screen.getByTestId("composer-editor")).toHaveValue("first");
    await expect(chat.getByText("first fixture turn complete")).toBeHidden();
  },
);
