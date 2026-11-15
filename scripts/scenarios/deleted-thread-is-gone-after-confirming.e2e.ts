import { expect } from "e2e";

import { ONICODE_HOME } from "../e2e.config.ts";
import { test, WEB } from "./desktop.ts";
import { makeState, runPromise } from "./onicode.ts";

test(
  "Deleting a thread asks first, keeps it on Cancel, and removes it on Confirm",
  WEB,
  async ({ onicode, desktop, app, screen }) => {
    const project = await onicode.project("demo");
    const thread = await onicode.thread({ project, title: "Delete me", messages: ["first"] });
    await onicode.open();
    const row = screen
      .getByRole("navigation", "Threads")
      .getByTestId("sidebar-row-card")
      .filter({ hasText: "Delete me" });
    const confirm = screen.getByRole("alertdialog", 'Delete thread "Delete me"?');

    await desktop.chooseFromContextMenu(row, "Delete");
    await app.screenshot("confirm");
    await confirm.getByRole("button", "Cancel").tap();
    await expect(confirm).toBeHidden();
    await expect(row).toBeVisible();

    await desktop.chooseFromContextMenu(row, "Delete");
    await confirm.getByRole("button", "Confirm").tap();
    await expect(row).toBeHidden();
    const { cli } = makeState(ONICODE_HOME);
    await expect
      .poll(
        async () =>
          (await runPromise(cli(["thread", "list", "--project", project, "--json"]))).stdout,
      )
      .not.toContain(thread.threadId);
  },
);
