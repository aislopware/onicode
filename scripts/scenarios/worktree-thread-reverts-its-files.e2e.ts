import { expect } from "e2e";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";

import { ONICODE_HOME } from "../e2e.config.ts";
import { chooseReplay } from "./composer.ts";
import { runPromise, test, WEB } from "./onicode.ts";
import { run } from "./shell.ts";

test(
  "A worktree thread edits its own checkout, shows the diff, and reverts the files with the chat",
  WEB,
  async ({ onicode, app, screen }) => {
    const project = await onicode.project("demo", { "README.md": "# demo\n" });
    const git = (...args: ReadonlyArray<string>) =>
      runPromise(run("git", args, { cwd: project })).then(({ stdout }) => stdout);
    const fs = <A, E>(use: (fs: FileSystem.FileSystem) => Effect.Effect<A, E>) =>
      runPromise(Effect.flatMap(FileSystem.FileSystem, use));
    await onicode.open();
    await chooseReplay(screen, "approval", "Supervised");
    await screen.getByRole("combobox", "Workspace").tap();
    await screen.getByRole("option", "New worktree").tap();
    const composer = screen.getByTestId("composer-editor");
    await composer.fill("Edit the notes");
    await screen.getByRole("button", "Submit message").tap();

    // Checking out the worktree and starting the agent in it takes longer than a default wait.
    await expect(screen.getByRole("group", "Command approval")).toBeVisible({ timeout: 60_000 });
    const worktree = (await git("worktree", "list", "--porcelain"))
      .split("\n")
      .map((line) => line.replace(/^worktree /, ""))
      .find((line) => line.startsWith(`${ONICODE_HOME}/worktrees/`));
    expect(worktree).toBeDefined();

    // The replay agent edits nothing, so the scenario makes the turn's edits while it waits.
    await fs((fs) =>
      fs.writeFileString(`${worktree}/README.md`, "# demo\n\nA line the turn added.\n"),
    );
    await fs((fs) => fs.writeFileString(`${worktree}/added.ts`, "export const added = 1;\n"));
    await screen.getByRole("button", "Approve").tap();

    const chat = screen.getByRole("region", "Conversation");
    await expect(chat.getByText("Created the requested probe file.")).toBeVisible();
    await expect(chat.getByRole("group", "3 additions, 0 deletions")).toBeVisible();
    expect(await git("status", "--porcelain")).toBe("");

    await chat.getByRole("button", "Open diff").tap();
    const diff = screen.getByRole("region", "Workspace panel: Diff");
    // A file opens expanded or collapsed by its size, so either label is the file being listed.
    await expect(diff.getByRole("button", /^(Expand|Collapse) added\.ts$/)).toBeVisible();
    await expect(diff.getByRole("button", /^(Expand|Collapse) README\.md$/)).toBeVisible();
    await app.screenshot("diff");
    await diff.getByRole("button", "Close Diff").tap();

    await chat.getByRole("button", "Edit from here").tap();
    await screen
      .getByRole("alertdialog", "Edit from here?")
      .getByRole("button", "Revert files too")
      .tap();
    await expect(composer).toHaveValue("Edit the notes");
    await expect(chat.getByRole("heading", "You")).toBeHidden();
    await expect.poll(() => fs((fs) => fs.exists(`${worktree}/added.ts`))).toBe(false);
    expect(await fs((fs) => fs.readFileString(`${worktree}/README.md`))).toBe("# demo\n");
    await app.screenshot("reverted");
  },
);
