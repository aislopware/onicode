import { expect } from "e2e";
import * as Schema from "effect/Schema";

import { ONICODE_HOME } from "../e2e.config.ts";
import { startSecondEnvironment } from "./second-environment.ts";
import { runPromise, test, WEB } from "./onicode.ts";
import { ENVIRONMENT_LABEL } from "./start-app.ts";

const decodePairing = Schema.decodeEffect(
  Schema.fromJsonString(Schema.Struct({ credential: Schema.String })),
);

test(
  "Removing a connection clears its permission update notice",
  WEB,
  async ({ onicode, app, screen }) => {
    await onicode.project("demo");
    const second = await startSecondEnvironment(`${ONICODE_HOME}-second-environment`);
    try {
      // A read-only pairing predates scoped permissions, so the notice asks to re-pair it.
      const issued = await runPromise(
        second.cli(["auth", "pairing", "create", "--scope", "orchestration:read", "--json"]),
      );
      const { credential } = await runPromise(decodePairing(issued.stdout));

      await onicode.open("/settings/connections");
      await screen.getByRole("button", "Add environment").tap();
      const dialog = screen.getByRole("dialog");
      await dialog.getByPlaceholder("backend.example.com").fill(second.host);
      await dialog.getByPlaceholder("PAIRCODE").fill(credential);
      await dialog.getByRole("button", "Add environment").tap();

      const notice = screen.getByText(`Permissions have changed for ${ENVIRONMENT_LABEL}`, {
        exact: true,
      });
      await notice.waitFor({ timeout: 60_000 });
      const actions = screen.getByRole("button", `More actions for ${ENVIRONMENT_LABEL}`);
      await actions.press("Enter");
      await screen.getByRole("menuitem", "Remove from this device…").press("Enter");
      await screen.getByRole("button", "Confirm").press("Enter");
      await actions.waitFor({ state: "hidden" });

      await app.screenshot("after-removing-connection");
      await expect(notice).toBeHidden();
    } finally {
      await second.stop();
    }
  },
);
