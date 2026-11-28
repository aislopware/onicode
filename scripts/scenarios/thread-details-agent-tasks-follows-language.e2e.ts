import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

test("Agent tasks follows the German language setting", WEB, async ({ onicode, app, screen }) => {
  await onicode.project("demo");
  await onicode.open("/settings/general");
  await screen.getByLabel("Language", { exact: true }).tap({ timeout: 120_000 });
  await screen.getByText("Deutsch", { exact: true }).tap();
  await screen.getByLabel("Sprache", { exact: true }).waitFor();

  await onicode.open("/settings/appearance");
  await screen.getByRole("button", /^Thread-Details: \d+ von \d+ an$/).tap({ timeout: 120_000 });
  await screen.getByText("In Thread-Details anzeigen", { exact: true }).waitFor();

  await app.screenshot("german-thread-details-options");
  await expect(screen.getByRole("menuitemcheckbox", "Agent tasks", { exact: true })).toHaveCount(0);
  await expect(screen.getByRole("menuitemcheckbox", "Agent-Aufgaben", { exact: true })).toHaveCount(
    1,
  );
});
