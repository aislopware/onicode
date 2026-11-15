import { expect } from "e2e";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";

import { ONICODE_HOME } from "../e2e.config.ts";
import { runPromise, test, WEB } from "./onicode.ts";
import { run } from "./shell.ts";

/** A repository the app has not seen, for the user to add by its path. */
const makeRepository = Effect.gen(function* () {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  yield* fs.makeDirectory(path.join(ONICODE_HOME, "workspaces"), { recursive: true });
  const parent = yield* fs.makeTempDirectory({
    directory: path.join(ONICODE_HOME, "workspaces"),
    prefix: "w-",
  });
  const repository = path.join(parent, "orchard");
  yield* fs.makeDirectory(repository);
  yield* run("git", ["init", "--quiet", "--initial-branch=main"], { cwd: repository });
  return repository;
});

test(
  "The welcome page leads an empty app to its first project and its first replayed turn",
  WEB,
  async ({ app, screen }) => {
    const repository = await runPromise(makeRepository);
    // Whether a first launch still shows the page depends on what ran before; its route always does.
    await app.open("/welcome");
    const welcome = screen.getByRole("main", "Welcome");
    await expect(welcome.getByRole("heading", "Welcome to Onicode")).toBeVisible();
    await expect(welcome.getByText("Replay (dev)")).toBeVisible();
    await expect(welcome.getByRole("link", "Pair a phone")).toBeVisible();
    await app.screenshot("welcome");
    await welcome.getByRole("button", "Start").tap();

    const sidebar = screen.getByRole("navigation", "Threads");
    await expect(sidebar.getByText("No projects yet")).toBeVisible();
    await expect(sidebar.getByRole("button", "New thread")).toBeDisabled();

    await screen.getByRole("main", "Home").getByRole("button", "Add project").tap();
    const palette = screen.getByTestId("command-palette");
    await palette.getByRole("option", /^Local folder/).tap();
    await palette.getByRole("combobox", /^Enter path/).fill(repository);
    await palette.getByRole("button", /^Add/).tap();
    await expect(screen.getByRole("button", "New thread in orchard")).toBeVisible();
    await expect(sidebar.getByRole("button", "New thread")).toBeEnabled();

    const composer = screen.getByTestId("composer-editor");
    await composer.fill("Say hello");
    await composer.press("Enter");
    await expect(screen.getByRole("button", "Stop generation")).toBeHidden({ timeout: 60_000 });
    await expect(sidebar.getByTestId("sidebar-row-card")).toHaveCount(1);
    await app.screenshot("first-turn");

    await app.open("/settings/projects");
    const settings = screen.getByRole("dialog", "Settings");
    await settings.getByRole("button", /^orchard /).tap();
    await settings.getByRole("button", "Remove project").tap();
    await screen.getByRole("alertdialog").getByRole("button", "Confirm").tap();
    await expect(settings.getByRole("button", /^orchard /)).toBeHidden();
  },
);
