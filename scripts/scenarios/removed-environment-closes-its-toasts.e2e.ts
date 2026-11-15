import { expect } from "e2e";

import { ONICODE_HOME } from "../e2e.config.ts";
import { makeState, runPromise, test, WEB } from "./onicode.ts";
import { startSecondEnvironment } from "./second-environment.ts";
import { ENVIRONMENT_LABEL } from "./start-app.ts";

test(
  "Removing an environment closes the in-app notifications it raised",
  WEB,
  async ({ onicode, app, screen, browser }) => {
    const project = await onicode.project("demo");
    const primaryThread = await onicode.thread({
      project,
      title: "Finished here",
      messages: ["first"],
    });
    const primary = makeState(ONICODE_HOME);
    const second = await startSecondEnvironment(`${ONICODE_HOME}-second-environment`);
    try {
      const { host, token } = second;
      const project = await second.state.project("remote-demo");
      const thread = await second.state.thread({
        project,
        title: "Finished elsewhere",
        messages: ["first"],
      });

      await onicode.open("/settings/general");
      await screen.getByRole("switch", "In-app notifications").check();
      await app.open("/settings/connections");
      await screen.getByRole("button", "Add environment").tap();
      const dialog = screen.getByRole("dialog");
      await dialog.getByPlaceholder("backend.example.com").fill(host);
      await dialog.getByPlaceholder("PAIRCODE").fill(token);
      await dialog.getByRole("button", "Add environment").tap();
      const moreActions = screen.getByRole("button", `More actions for ${ENVIRONMENT_LABEL}`);
      await expect(moreActions).toBeVisible({ timeout: 60_000 });

      // The client announces only a change it saw happen, so it must hold the thread first.
      await expect(screen.getByText("Finished elsewhere").first()).toBeVisible({ timeout: 60_000 });
      await runPromise(
        primary.cli(["thread", "send", primaryThread.threadId, "second", "--wait", "--json"]),
      );
      const primaryToast = browser.locator(
        '[data-slot="toast-description"]:has-text("Finished here")',
      );
      await expect(primaryToast).toBeVisible({ timeout: 60_000 });
      await primaryToast.hover();
      await runPromise(
        second.cli(["thread", "send", thread.threadId, "second", "--wait", "--json"]),
      );
      const toast = browser.locator(
        '[data-slot="toast-description"]:has-text("Finished elsewhere")',
      );
      await expect(toast).toBeVisible({ timeout: 60_000 });

      // A hovered toast does not time out. The removal goes by keyboard so the pointer stays.
      await toast.hover();
      await moreActions.press("Enter");
      await screen.getByRole("menuitem", "Remove from this device…").press("Enter");
      await expect(toast).toBeVisible();
      await screen.getByRole("button", "Confirm").press("Enter");
      try {
        await expect(toast).toBeHidden();
        // Both timers pause together; an older surviving toast rules out auto-dismissal.
        await expect(primaryToast).toBeVisible();
      } finally {
        await app.screenshot("after-removal");
      }
      await expect(moreActions).toBeHidden({ timeout: 30_000 });
    } finally {
      await second.stop();
    }
  },
);
