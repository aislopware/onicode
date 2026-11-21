// @effect-diagnostics-next-line nodeBuiltinImport:off - a fake Linear API for the server to call
import * as NodeHttp from "node:http";

import { IssueTrackerKind } from "@onicode/contracts";
import { expect } from "e2e";
import * as Effect from "effect/Effect";

import { ONICODE_HOME } from "../e2e.config.ts";
import { SCENARIO_LINEAR_PORT } from "./start-app.ts";
import { rpcClient, runPromise, test, WEB } from "./onicode.ts";

// Connecting reads only the viewer, the workspace and its teams.
const VIEWER = {
  data: {
    viewer: {
      id: "user-1",
      name: "Scenario User",
      displayName: "scenario",
      email: "user@example.com",
      avatarUrl: null,
    },
    organization: { urlKey: "acme" },
    teams: {
      nodes: [
        { id: "team-eng", key: "ENG", name: "Engineering" },
        { id: "team-ops", key: "OPS", name: "Operations" },
      ],
    },
  },
};

test(
  "Linear settings say agents can file new issues in another bound team",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    const linear = NodeHttp.createServer((_request, response) => {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify(VIEWER));
    });
    // A taken port fails this test instead of escaping as an unhandled error.
    await new Promise<void>((resolve, reject) => {
      linear.once("error", reject);
      linear.listen(SCENARIO_LINEAR_PORT, "127.0.0.1", () => {
        linear.off("error", reject);
        resolve();
      });
    });
    try {
      await onicode.project("demo");
      await onicode.open("/settings/source-control");

      await screen.getByRole("button", "Toggle Linear details").tap();
      await browser.locator('input[id^="issue-tracker-linear-"]').fill("lin_api_scenario");
      await browser.keyboard.press("Enter");
      await expect(
        screen.getByText("Pick one or more Linear teams for each project", { exact: false }),
      ).toBeVisible();
      await app.screenshot("linear-projects");

      await expect(
        screen.getByText("agents file new issues in the first one unless asked for another", {
          exact: false,
        }),
      ).toBeVisible();
    } finally {
      // The credential lives on the shared scenario server; later scenarios start without it.
      await runPromise(
        Effect.gen(function* () {
          const client = yield* rpcClient(ONICODE_HOME, app.baseUrl ?? "");
          yield* client["issueTrackers.connect"]({
            tracker: IssueTrackerKind.make("linear"),
            token: null,
          });
        }).pipe(Effect.scoped),
      );
      await new Promise((resolve) => linear.close(resolve));
    }
  },
);
