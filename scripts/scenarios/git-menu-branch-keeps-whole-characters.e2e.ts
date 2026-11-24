import { expect } from "e2e";

import { run } from "./shell.ts";
import { runPromise } from "./onicode.ts";
import { IOS, test } from "./oni-mobile.ts";

// The Git menu shows 12 units, "…" and the last 11; each emoji straddles one cut.
const BRANCH = "feature/abc😀-long-branch-name-😀-for-menus";

test(
  "The Git menu's branch title keeps whole characters at both cuts",
  IOS,
  async ({ onicode, app, screen }) => {
    const project = await onicode.project("demo");
    await runPromise(run("git", ["checkout", "--quiet", "-b", BRANCH], { cwd: project }));
    await onicode.thread({ project, title: "Branch work", messages: ["first"] });
    // A local simulator keeps the last run's pairing.
    await app.clearState().catch(() => {});
    await onicode.open();

    await screen.getByText("Branch work", { exact: false }).first().tap({ timeout: 120_000 });
    // Terminal joins the header once the session's scopes load and moves the other buttons.
    await screen.getByRole("button", "Terminal").waitFor({ timeout: 60_000 });
    await screen.getByRole("button", "Git controls").tap({ timeout: 60_000 });
    const title = screen.getByText("feature/abc", { exact: false }).first();
    await title.waitFor({ timeout: 30_000 });
    await app.screenshot("git-menu");
    await expect(title).toHaveText("feature/abc…-for-menus, Clean");
  },
);
