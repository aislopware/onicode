import { expect } from "e2e";
import * as Effect from "effect/Effect";

import { ONICODE_HOME } from "../e2e.config.ts";
import { startSecondEnvironment } from "./second-environment.ts";
import { run } from "./shell.ts";
import { rpcClient, runPromise, test, WEB } from "./onicode.ts";
import { ENVIRONMENT_LABEL } from "./start-app.ts";

test(
  "Automatic machine selection uses the chosen project's workspace default",
  WEB,
  async ({ onicode, app, screen, browser }) => {
    const repository = {
      ".git/config": '[remote "origin"]\n\turl = https://github.com/scenario/demo.git\n',
    };
    await onicode.project("demo", repository);
    const secondHome = `${ONICODE_HOME}-second-environment`;
    const second = await startSecondEnvironment(secondHome);
    const moreActions = screen.getByRole("button", `More actions for ${ENVIRONMENT_LABEL}`);
    let paired = false;
    try {
      const { host, token } = second;
      const remoteProject = await second.state.project("demo", repository);
      await runPromise(run("git", ["branch", "-m", "remote-base"], { cwd: remoteProject }));
      await runPromise(
        Effect.scoped(
          Effect.gen(function* () {
            const client = yield* rpcClient(secondHome, host);
            yield* client["server.updateSettings"]({ patch: { defaultThreadEnvMode: "worktree" } });
          }),
        ),
      );

      await onicode.open();
      const workspace = screen.getByRole("combobox", "Workspace", { exact: true });
      await workspace.getByText("Current checkout").waitFor({ timeout: 60_000 });
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

      await screen.getByRole("switch", "Automatically balance load").tap();
      // Onicode folds the per-machine preferences under the section title.
      await screen.getByRole("button", "Load balancing", { exact: true }).tap();
      await screen.getByRole("combobox", `${ENVIRONMENT_LABEL} load preference`).first().tap();
      await screen.getByRole("option", "Manual only", { exact: true }).tap();

      await app.open(draftPath);
      // Onicode's compact composer toolbar collapses its context labels to zero width.
      await screen
        .getByText("Auto balance", { exact: true })
        .first()
        .waitFor({ state: "attached", timeout: 60_000 });
      await screen
        .getByRole("combobox", /^Branch: (From )?remote-base/)
        .first()
        .waitFor({ timeout: 60_000 });
      await app.screenshot("workspace-after-auto-machine-selection");
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
