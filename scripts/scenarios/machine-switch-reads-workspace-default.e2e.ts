import { expect } from "e2e";

import { ONICODE_HOME } from "../e2e.config.ts";
import { startSecondEnvironment } from "./second-environment.ts";
import { run } from "./shell.ts";
import { runPromise, test, WEB } from "./onicode.ts";
import { ENVIRONMENT_LABEL } from "./start-app.ts";

test(
  "The first switch to another machine uses that checkout's project file workspace default",
  WEB,
  async ({ onicode, app, screen, browser }) => {
    const repository = {
      ".git/config": '[remote "origin"]\n\turl = https://github.com/scenario/demo.git\n',
    };
    await onicode.project("demo", repository);
    const second = await startSecondEnvironment(`${ONICODE_HOME}-second-environment`);
    const moreActions = screen.getByRole("button", `More actions for ${ENVIRONMENT_LABEL}`);
    let paired = false;
    try {
      const { host, token } = second;
      const remoteProject = await second.state.project("demo", {
        ...repository,
        "onicode.json": JSON.stringify({ defaultThreadEnvMode: "worktree" }),
      });
      await runPromise(run("git", ["branch", "-m", "remote-base"], { cwd: remoteProject }));

      await onicode.open();
      const workspace = screen.getByRole("combobox", "Workspace", { exact: true });
      await workspace.getByText("Current checkout").waitFor();
      await screen.getByRole("combobox", "Branch: main", { exact: true }).first().waitFor();
      await screen.getByRole("textbox").fill("Plan the next change");
      const draftPath = new URL(await browser.url()).hash.slice(1) || "/";

      await app.open("/settings/connections");
      await screen.getByRole("button", "Add environment").tap();
      const dialog = screen.getByRole("dialog");
      await dialog.getByPlaceholder("backend.example.com").fill(host);
      await dialog.getByPlaceholder("PAIRCODE").fill(token);
      await dialog.getByRole("button", "Add environment").tap();
      // The map is the default view; its rows carry no actions menu.
      await screen.getByRole("button", "List", { exact: true }).tap();
      await expect(moreActions).toBeVisible({ timeout: 60_000 });
      paired = true;
      await app.open(draftPath);

      // Both machines carry the scenario label; pick the one the draft is not on.
      const machine = (index: number) => browser.locator('[role="menuitemradio"]').nth(index);
      await screen.getByText(ENVIRONMENT_LABEL).last().tap();
      const target = (await machine(0).getAttribute("aria-checked")) === "true" ? 1 : 0;
      await machine(target).click();
      await screen
        .getByRole("combobox", /^Branch: (From )?remote-base/)
        .first()
        .waitFor();

      await app.screenshot("workspace-after-first-machine-switch");
      await expect(workspace).toHaveText("New worktree");
    } finally {
      // The desktop app keeps paired environments between scenarios, so a later one pairing
      // the same label would find two.
      try {
        if (paired) {
          await app.open("/settings/connections");
          await screen.getByRole("button", "List", { exact: true }).tap();
          await moreActions.tap();
          await screen.getByRole("menuitem", "Remove from this device…").tap();
          await screen.getByRole("button", "Confirm").tap();
          await expect(moreActions).toBeHidden({ timeout: 30_000 });
        }
      } finally {
        await second.stop();
      }
    }
  },
);
