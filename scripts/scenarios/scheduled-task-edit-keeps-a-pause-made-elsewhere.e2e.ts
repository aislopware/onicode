import { expect } from "e2e";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Stream from "effect/Stream";

import { ONICODE_HOME } from "../e2e.config.ts";
import { rpcClient, runPromise, test, WEB } from "./onicode.ts";

test(
  "Saving a scheduled task in Settings keeps a pause made elsewhere while the editor was open",
  WEB,
  async ({ onicode, app, screen }) => {
    await onicode.project("demo");
    await onicode.open("/settings/scheduled-tasks");
    const settings = screen.getByRole("dialog", "Settings");

    await settings.getByRole("button", "New task").tap();
    const created = screen.getByRole("dialog", "New task");
    await created.getByRole("textbox", "Name").fill("Morning sweep");
    await created.getByRole("textbox", "Prompt").fill("Sweep the notes");
    await created.getByRole("button", "Create task").tap();
    await expect(created).toBeHidden();
    const enabled = settings.getByRole("switch", "Enable Morning sweep");
    await expect(enabled).toBeChecked();

    await settings.getByRole("button", "Actions for Morning sweep").tap();
    await screen.getByRole("menuitem", "Edit").tap();
    const form = screen.getByRole("dialog", "Edit task");
    await form.getByRole("textbox", "Prompt").fill("Sweep the notes twice");

    // Another client pauses the task while the editor is open.
    await runPromise(
      Effect.gen(function* () {
        const client = yield* rpcClient(ONICODE_HOME, app.baseUrl ?? "");
        const list = yield* Stream.runHead(client["scheduledTasks.subscribe"]({}));
        const task = Option.getOrThrow(list).tasks.find((entry) => entry.title === "Morning sweep");
        yield* client["scheduledTasks.setEnabled"]({ id: task!.id, enabled: false });
      }).pipe(Effect.scoped),
    );

    await form.getByRole("button", "Save task").tap();
    await expect(
      screen.getByText("The task changed while saving", { exact: false }).first(),
    ).toBeVisible();
    await app.screenshot("refused-save");

    // The editor took the pause in and kept the prompt edit, so saving again goes through.
    await form.getByRole("button", "Save task").tap();
    await expect(form).toBeHidden();
    await expect(enabled).not.toBeChecked();
    await app.screenshot("after-save");

    await settings.getByRole("button", "Actions for Morning sweep").tap();
    await screen.getByRole("menuitem", "Delete").tap();
    await screen.getByRole("alertdialog").getByRole("button", "Confirm").tap();
    await expect(enabled).toBeHidden();
  },
);
