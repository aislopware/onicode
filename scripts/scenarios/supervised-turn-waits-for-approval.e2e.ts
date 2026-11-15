import { expect } from "e2e";

import { chooseReplay } from "./composer.ts";
import { test, WEB } from "./onicode.ts";

test(
  "A supervised turn waits for approval, says so in the sidebar, and finishes once approved",
  WEB,
  async ({ onicode, app, screen }) => {
    await onicode.project("demo");
    await onicode.open();
    await chooseReplay(screen, "approval", "Supervised");

    await screen.getByTestId("composer-editor").fill("Write the probe file");
    await screen.getByRole("button", "Submit message").tap();
    const approval = screen.getByRole("group", "Command approval");
    await expect(approval).toBeVisible();
    await expect(
      screen.getByRole("navigation", "Threads").getByRole("button", /, Approval, demo$/),
    ).toBeVisible();
    await app.screenshot("waiting");

    await approval.getByRole("button", "Approve").tap();
    await expect(screen.getByText("Created the requested probe file.")).toBeVisible();
    await expect(approval).toBeHidden();
    await app.screenshot("approved");
  },
);
