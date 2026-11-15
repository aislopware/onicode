import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

test(
  "A running turn shows as Working in the sidebar and names the window when it is done",
  WEB,
  async ({ onicode, desktop, app, browser, screen }) => {
    await onicode.project("demo", { "src/math.ts": "export const sum = 1 + 1;\n" });
    await onicode.open();

    const composer = screen.getByTestId("composer-editor");
    await composer.fill("Tour the math module");
    await screen.getByRole("button", "Submit message").tap();
    const sidebar = screen.getByRole("navigation", "Threads");
    await expect(sidebar.getByRole("button", /, Working, demo$/)).toBeVisible();
    await expect(screen.getByText(/Tasks \d\/3/)).toBeVisible();
    await app.screenshot("working");

    await expect(screen.getByText("This turn is a replay.", { exact: false })).toBeVisible();
    await expect(screen.getByRole("button", "Stop generation")).toBeHidden();
    // An open thread is read as it finishes, so its row drops the status and never shows Done.
    await expect(sidebar.getByRole("button", /, Working, demo$/)).toBeHidden();
    await expect(sidebar.getByRole("button", /, demo$/)).toBeVisible();
    await expect(browser).toHaveTitle(/^demo \/ /);
    expect((await desktop.window()).title).toMatch(/^demo \/ /);
    await app.screenshot("done");
  },
);
