import { expect } from "e2e";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";

import { ONICODE_HOME } from "../e2e.config.ts";
import { runPromise, test, WEB } from "./onicode.ts";

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
  "A queued message shows its @-file mention by name, not as a raw link",
  WEB,
  async ({ onicode, app, screen, browser }) => {
    const settings = await runPromise(threadRuntimeSettings);
    try {
      // Supervised mode makes the approval recording wait for an answer, which holds its turn
      // open for as long as the message takes to queue.
      await runPromise(settings.supervise);
      await onicode.open("/settings/threads");
      await expect(screen.getByRole("combobox", "Default permissions")).toHaveText(/Supervised/);
      const project = await onicode.project("demo", { "src/foo.ts": "export {};\n" });
      const thread = await onicode.thread({
        project,
        title: "Long work",
        messages: ["Change the file"],
        model: "approval",
      });
      await onicode.open(thread);
      await screen.getByRole("button", "Approve", { exact: true }).waitFor();

      const composer = screen.getByRole("textbox");
      await composer.pressSequentially("Review @foo");
      await screen.getByRole("option", "foo.ts", { exact: false }).tap();
      await expect(composer).toContainText("foo.ts");
      // Enter steers by default; the alternate send queues.
      await browser.keyboard.press("ControlOrMeta+Enter");

      const queued = screen.getByRole("listitem", "Queued message 1 of 1, not sent");
      await expect(queued).toContainText("Review foo.ts");
      await app.screenshot("queued-row");
    } finally {
      await runPromise(settings.restore);
    }
  },
);
