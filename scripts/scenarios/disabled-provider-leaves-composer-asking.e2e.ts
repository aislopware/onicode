import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

test(
  "Turning the only provider off leaves the composer asking for one until it is back on",
  WEB,
  async ({ onicode, app, screen }) => {
    await onicode.project("demo");
    await onicode.open("/settings/providers");
    const settings = screen.getByRole("dialog", "Settings");
    const close = settings.getByRole("region", "Settings content").getByRole("button", "Close");
    const replay = settings.getByRole("switch", "Enable Replay (dev)");
    const model = screen.getByRole("button", "Showcase");
    await expect(replay).toBeChecked();

    await replay.tap();
    await expect(replay).not.toBeChecked();
    try {
      await close.tap();
      await expect(
        screen.getByText("Enable a provider in Settings to send a message"),
      ).toBeVisible();
      await expect(model).toBeHidden();
      await app.screenshot("no-provider");
      await screen.getByRole("button", "Open provider settings").tap();
    } finally {
      // Every later scenario replays through this provider.
      if (!(await replay.isVisible())) await onicode.open("/settings/providers");
      if (!(await replay.isChecked())) await replay.tap();
    }
    await expect(replay).toBeChecked();
    await close.tap();
    await expect(model).toBeVisible();
  },
);
