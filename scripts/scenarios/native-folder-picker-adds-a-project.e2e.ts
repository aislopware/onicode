import { expect } from "e2e";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";

import { ONICODE_HOME } from "../e2e.config.ts";
import { test, WEB } from "./desktop.ts";
import { runPromise } from "./onicode.ts";
import { run } from "./shell.ts";

/** A repository the app has not seen, for the picker to return. */
const makeRepository = Effect.gen(function* () {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  yield* fs.makeDirectory(path.join(ONICODE_HOME, "workspaces"), { recursive: true });
  const parent = yield* fs.makeTempDirectory({
    directory: path.join(ONICODE_HOME, "workspaces"),
    prefix: "w-",
  });
  const repository = path.join(parent, "harbor");
  yield* fs.makeDirectory(repository);
  yield* run("git", ["init", "--quiet", "--initial-branch=main"], { cwd: repository });
  return repository;
});

test(
  "The native folder picker adds the project the user picked, and Settings removes it",
  WEB,
  async ({ onicode, desktop, app, browser, screen }) => {
    await onicode.project("demo");
    const repository = await runPromise(makeRepository);
    await onicode.open();

    await desktop.answerFolderPicker(repository);
    await browser.keyboard.press("ControlOrMeta+k");
    const palette = screen.getByTestId("command-palette");
    await palette.getByRole("option", "Add project").tap();
    await palette.getByRole("option", /^Local folder/).tap();
    await palette.getByRole("button", /^Open in /).tap();
    await expect(screen.getByRole("button", "New thread in harbor")).toBeVisible();
    await app.screenshot("added");

    await onicode.open("/settings/projects");
    const settings = screen.getByRole("dialog", "Settings");
    await settings.getByRole("button", /^harbor /).tap();
    await settings.getByRole("button", "Remove project").tap();
    await screen.getByRole("alertdialog").getByRole("button", "Confirm").tap();
    await expect(settings.getByRole("button", /^harbor /)).toBeHidden();
    await expect(settings.getByRole("button", /^demo /)).toBeVisible();
  },
);
