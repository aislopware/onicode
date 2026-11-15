import { expect } from "e2e";

import { ONICODE_HOME } from "../e2e.config.ts";
import { test, WEB } from "./onicode.ts";
import { startSecondEnvironment } from "./second-environment.ts";

test(
  "A paired environment lists its threads beside the local ones and runs a turn sent from here",
  WEB,
  async ({ onicode, app, screen }) => {
    const local = await onicode.project("demo");
    await onicode.thread({ project: local, title: "Local work", messages: ["first"] });
    const remote = await startSecondEnvironment(`${ONICODE_HOME}-remote-environment`);
    try {
      const project = await remote.state.project("remote-demo");
      await remote.state.thread({ project, title: "Remote work", messages: ["first"] });

      await onicode.open("/settings/connections");
      await screen.getByRole("button", "Add environment").tap();
      const dialog = screen.getByRole("dialog", "Add Environment");
      await dialog.getByRole("textbox", "Host").fill(remote.host);
      await dialog.getByRole("textbox", "Pairing code").fill(remote.token);
      await dialog.getByRole("button", "Add environment").tap();
      const moreActions = screen.getByRole("button", `More actions for ${remote.label}`);
      await expect(moreActions).toBeVisible({ timeout: 60_000 });
      await screen.getByRole("region", "Settings content").getByRole("button", "Close").tap();
      // Closing returns to the page Settings opened over; a jump made before that lands is undone.
      await expect(screen.getByRole("dialog", "Settings")).toBeHidden();

      const sidebar = screen.getByRole("navigation", "Threads");
      await expect(sidebar.getByRole("button", /^Local work, /)).toBeVisible();
      const remoteRow = sidebar.getByRole("button", /^Remote work, /);
      await expect(remoteRow).toBeVisible({ timeout: 60_000 });
      await remoteRow.tap();
      const chat = screen.getByRole("region", "Conversation");
      await expect(chat.getByText("first fixture turn complete")).toBeVisible();

      const composer = screen.getByTestId("composer-editor");
      await composer.fill("second");
      await composer.press("Enter");
      await expect(chat.getByText("second fixture turn complete")).toBeVisible({ timeout: 60_000 });
      await app.screenshot("remote-turn");

      await app.open("/settings/connections");
      await moreActions.tap();
      await screen.getByRole("menuitem", "Remove from this device…").tap();
      await screen.getByRole("button", "Confirm").tap();
      await expect(moreActions).toBeHidden({ timeout: 30_000 });
      await expect(sidebar.getByRole("button", /^Remote work, /)).toBeHidden();
    } finally {
      await remote.stop();
    }
  },
);
