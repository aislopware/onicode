import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

test(
  "Stop interrupts a streaming turn, and the thread takes the next message",
  WEB,
  async ({ onicode, app, screen }) => {
    await onicode.project("demo", { "src/math.ts": "export const sum = 1 + 1;\n" });
    await onicode.open();

    const composer = screen.getByTestId("composer-editor");
    await composer.fill("Tour the math module");
    await screen.getByRole("button", "Submit message").tap();
    await expect(screen.getByText(/Tasks \d\/3/)).toBeVisible();
    const stop = screen.getByRole("button", "Stop generation");
    await stop.tap();

    const chat = screen.getByRole("region", "Conversation");
    await expect(chat.getByText("Run interrupted")).toBeVisible();
    await expect(stop).toBeHidden();
    await expect(screen.getByText("This turn is a replay.", { exact: false })).toBeHidden();
    await app.screenshot("stopped");

    // The button that replaces Stop ignores clicks for half a second, so a tap meant for Stop
    // cannot send; Enter is how a user sends at once.
    await composer.fill("Carry on");
    await composer.press("Enter");
    await expect(chat.getByRole("heading", "You")).toHaveCount(2);
    await expect(stop).toBeVisible();
    await expect(screen.getByText("This turn is a replay.", { exact: false })).toBeVisible();
  },
);
