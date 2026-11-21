import { expect } from "e2e";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";

import { ONICODE_HOME } from "../e2e.config.ts";
import { makeState, runPromise, test, WEB } from "./onicode.ts";

const SettingsJson = Schema.fromJsonString(Schema.Record(Schema.String, Schema.Unknown));
const decodeSettings = Schema.decodeEffect(SettingsJson);
const encodeSettings = Schema.encodeEffect(SettingsJson);

const threadRuntimeSettings = Effect.gen(function* () {
  const fs = yield* FileSystem.FileSystem;
  const settingsPath = (yield* Path.Path).join(ONICODE_HOME, "userdata/settings.json");
  const original = yield* fs.readFileString(settingsPath);
  const settings = yield* decodeSettings(original);
  return {
    supervise: fs.writeFileString(
      settingsPath,
      yield* encodeSettings({ ...settings, defaultRuntimeMode: "approval-required" }),
    ),
    restore: fs.writeFileString(settingsPath, original),
  };
});

test(
  "/compact sent while the agent works waits for the turn",
  WEB,
  async ({ onicode, app, screen }) => {
    const project = await onicode.project("demo");
    // The showcase recording streams for several seconds, long enough to send into its turn.
    const thread = await onicode.thread({
      project,
      title: "Long work",
      messages: ["Show me around"],
      model: "showcase",
    });
    await onicode.open(thread);

    const composer = screen.getByRole("textbox");
    await composer.tap();
    await composer.pressSequentially("Show me again");
    await composer.press("Enter");
    await expect(screen.getByRole("button", "Stop generation")).toBeVisible();

    // The trailing space closes the slash menu so Enter sends instead of picking a command.
    await composer.pressSequentially("/compact ");
    await composer.press("Enter");
    await app.screenshot("compact-sent");
    await expect(screen.getByRole("button", "Edit queued message")).toBeVisible();

    const settings = await runPromise(threadRuntimeSettings);
    try {
      // An approval holds this turn open, so the queued command stays put while Send now is hovered.
      await runPromise(settings.supervise);
      // The server takes the edit from a file watcher; Settings shows once it is supervised.
      await onicode.open("/settings/threads");
      await expect(screen.getByRole("combobox", "Default permissions")).toHaveText(/Supervised/);
      const waiting = await onicode.thread({
        project,
        title: "Waiting work",
        messages: ["Change the file"],
        model: "approval",
      });
      // Send now hides on a command already first in the queue, so a follow-up waits ahead of it.
      const state = makeState(ONICODE_HOME);
      await runPromise(state.cli(["thread", "send", waiting.threadId, "Then tidy up"]));
      await runPromise(state.cli(["thread", "send", waiting.threadId, "/compact"]));
      await onicode.open(waiting);
      await screen.getByRole("button", "Send now").last().hover();
      await expect(
        screen.getByText("Move this command to the front of the queue", { exact: false }),
      ).toBeVisible();
      await app.screenshot("send-now-tooltip");
    } finally {
      await runPromise(settings.restore);
    }
  },
);
