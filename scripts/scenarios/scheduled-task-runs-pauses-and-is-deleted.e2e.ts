import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

test(
  "A scheduled task is created in Settings, pauses, runs on demand, and is deleted",
  WEB,
  async ({ onicode, app, screen }) => {
    await onicode.project("demo");
    await onicode.open("/settings/scheduled-tasks");
    const settings = screen.getByRole("dialog", "Settings");

    await settings.getByRole("button", "New task").tap();
    const form = screen.getByRole("dialog", "New task");
    await form.getByRole("textbox", "Name").fill("Morning sweep");
    await form.getByRole("textbox", "Prompt").fill("Sweep the notes");
    await form.getByRole("button", "Create task").tap();
    await expect(form).toBeHidden();

    const enabled = settings.getByRole("switch", "Enable Morning sweep");
    await expect(enabled).toBeChecked();
    await expect(settings.getByText("Next run", { exact: false })).toBeVisible();
    await enabled.tap();
    await expect(settings.getByText("Paused", { exact: false })).toBeVisible();
    await enabled.tap();
    await expect(settings.getByText("Next run", { exact: false })).toBeVisible();

    const actions = settings.getByRole("button", "Actions for Morning sweep");
    await actions.tap();
    await screen.getByRole("menuitem", "Run now").tap();
    // The run is a whole replayed turn in a new thread.
    await expect(settings.getByText("Succeeded")).toBeVisible({ timeout: 60_000 });
    await app.screenshot("after-run");

    await actions.tap();
    await screen.getByRole("menuitem", "Delete").tap();
    await screen.getByRole("alertdialog").getByRole("button", "Confirm").tap();
    await expect(enabled).toBeHidden();
  },
);
