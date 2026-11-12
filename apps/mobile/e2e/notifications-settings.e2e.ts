import { test } from "@e2e-dev/mobile";
import { expect } from "e2e";

import { launchApp, loadBundle } from "./launchApp.ts";

// Registration and delivery need a real push service; pushRegistration.test.ts covers that logic.
test("a notification preference survives a relaunch", async ({ app, device, platform, screen }) => {
  await launchApp({ app, device, platform, screen });
  await device.openLink("onicode-dev://settings");
  await screen.getByRole("button", "Notifications").tap();
  const completion = screen.getByRole("switch", "Completion");
  await expect(completion).toBeChecked();
  await completion.tap();
  await expect(completion).toBeChecked({ checked: false });

  await app.restart();
  await loadBundle({ app, device, platform, screen });
  await device.openLink("onicode-dev://settings/notifications");
  await expect(screen.getByRole("switch", "Completion")).toBeChecked({ checked: false });
  await expect(screen.getByRole("switch", "Failure")).toBeChecked();
});
