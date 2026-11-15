import { expect } from "e2e";

import { chooseReplay } from "./composer.ts";
import { test, WEB } from "./onicode.ts";

const QUESTION = "Should this fixture prefer strict schemas or UI flexibility?";

test(
  "A turn that asks a question continues with the option the user picked",
  WEB,
  async ({ onicode, app, screen }) => {
    await onicode.project("demo");
    await onicode.open();
    await chooseReplay(screen, "question");

    await screen.getByTestId("composer-editor").fill("Plan the fixture");
    await screen.getByRole("button", "Submit message").tap();
    await expect(screen.getByText(QUESTION)).toBeVisible();
    await app.screenshot("asked");

    await screen.getByText("UI flexibility").tap();
    await screen.getByRole("button", "Submit").tap();
    await expect(screen.getByText("plan questions fixture complete")).toBeVisible();
    await expect(screen.getByRole("button", `${QUESTION}: UI flexibility`)).toBeVisible();
    await app.screenshot("answered");
  },
);
