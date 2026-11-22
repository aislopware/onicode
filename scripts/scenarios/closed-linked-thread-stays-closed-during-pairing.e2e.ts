/// <reference lib="dom" />
import type { EnvironmentId } from "@onicode/contracts";
import { expect } from "e2e";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";

import { ONICODE_HOME } from "../e2e.config.ts";
import { startSecondEnvironment } from "./second-environment.ts";
import { environmentId, makeState, rpcClient, runPromise, test, WEB } from "./onicode.ts";
import { ENVIRONMENT_LABEL } from "./start-app.ts";

const SECOND_HOME = `${ONICODE_HOME}-second-environment`;

const decodePairing = Schema.decodeEffect(
  Schema.fromJsonString(Schema.Struct({ credential: Schema.String })),
);

const linkEnvironments = Effect.fn("scenario.linkEnvironments")(function* (
  primaryOrigin: string,
  secondOrigin: string,
) {
  const primary = yield* rpcClient(ONICODE_HOME, primaryOrigin);
  const second = yield* rpcClient(SECOND_HOME, secondOrigin);
  for (const client of [primary, second]) {
    yield* client["server.updateSettings"]({ patch: { linkedEnvironmentsEnabled: true } });
  }
  const pairing = yield* makeState(SECOND_HOME).cli(["auth", "pairing", "create", "--json"]);
  const { credential } = yield* decodePairing(pairing.stdout);
  const link = yield* primary["peerLinks.link"]({
    url: secondOrigin,
    pairingCode: credential,
    access: "full-access",
  });
  return link.environmentId;
}, Effect.scoped);

/** The primary server outlives the test, so the link and the beta go back to how they were. */
const unlinkEnvironment = Effect.fn("scenario.unlinkEnvironment")(function* (
  primaryOrigin: string,
  environmentId: EnvironmentId | undefined,
) {
  const primary = yield* rpcClient(ONICODE_HOME, primaryOrigin);
  yield* (
    environmentId === undefined ? Effect.void : primary["peerLinks.unlink"]({ environmentId })
  ).pipe(
    Effect.ensuring(
      primary["server.updateSettings"]({ patch: { linkedEnvironmentsEnabled: false } }).pipe(
        Effect.ignore,
      ),
    ),
  );
}, Effect.scoped);

test(
  "A closed linked-thread tab does not take over the conversation when pairing finishes",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    const project = await onicode.project("primary");
    const parent = await onicode.thread({
      project,
      title: "Current work",
      messages: ["Keep this conversation open"],
    });
    const primary = app.baseUrl ?? "";
    const second = await startSecondEnvironment(SECOND_HOME);
    const moreActions = screen.getByRole("button", `More actions for ${ENVIRONMENT_LABEL}`);
    let linked: EnvironmentId | undefined;
    try {
      const { host } = second;
      linked = await runPromise(linkEnvironments(primary, host));

      const linkedProject = await second.state.project("linked");
      const linkedTitle = "Remote design notes";
      const linkedPrompt = "Read the remote design notes";
      const remoteThread = await second.state.thread({
        project: linkedProject,
        title: linkedTitle,
        messages: [linkedPrompt],
      });
      const via = await runPromise(environmentId(primary));
      await browser.setViewport({ width: 1440, height: 900 });

      // Restore the same saved tab that opening the delegated thread in the panel would leave.
      // The desktop renderer routes by hash.
      await browser.addInitScript(
        ({ via, linked, parentId, childId, title }) => {
          if (location.hash.split("?")[0] !== `#/${via}/${parentId}`) return;
          const id = `linked-thread:${encodeURIComponent(via)}:${encodeURIComponent(linked)}:${encodeURIComponent(childId)}`;
          localStorage.setItem(
            "onicode:right-panel-state:v2",
            JSON.stringify({
              version: 16,
              state: {
                byThreadKey: {
                  [`${via}:${parentId}`]: {
                    isOpen: true,
                    activeSurfaceId: id,
                    surfaces: [
                      {
                        kind: "linked-thread",
                        id,
                        viaEnvironmentId: via,
                        environmentId: linked,
                        threadId: childId,
                        label: "Scenario",
                        title,
                      },
                    ],
                  },
                },
                threadPanelVisibilityByThreadKey: {},
              },
            }),
          );
        },
        {
          via,
          linked,
          parentId: parent.threadId,
          childId: remoteThread.threadId,
          title: linkedTitle,
        },
      );

      const tokenUrl = `${host}/oauth/token`;
      const requested = Promise.withResolvers<void>();
      const release = Promise.withResolvers<void>();
      await browser.route(tokenUrl, async (route) => {
        if (route.request.method !== "POST") return route.fallback();
        requested.resolve();
        await release.promise;
        await route.continue();
      });
      try {
        await onicode.open(parent);
        await screen.getByText(linkedPrompt, { exact: true }).waitFor();
        const originalUrl = await browser.url();
        await screen.getByRole("button", "Connect to interact", { exact: true }).tap();
        await requested.promise;
        await screen.getByRole("button", `Close ${linkedTitle}`, { exact: true }).tap();

        release.resolve();
        await screen
          .getByText("This device is paired with it and can use its threads.", { exact: true })
          .waitFor();

        await app.screenshot("after-pairing-finishes");
        await expect(browser).toHaveURL(originalUrl);
      } finally {
        release.resolve();
        await browser.unroute(tokenUrl);
      }
    } finally {
      // The desktop app keeps paired environments between scenarios, so a later one pairing
      // the same label would find two.
      try {
        await onicode.open("/settings/connections");
        // The map is the default view; its rows carry no actions menu.
        const list = screen.getByRole("button", "List", { exact: true });
        const appears = (locator: typeof list) =>
          locator.waitFor({ timeout: 15_000 }).then(
            () => true,
            () => false,
          );
        if (await appears(list)) await list.tap();
        if (await appears(moreActions)) {
          await moreActions.tap();
          await screen.getByRole("menuitem", "Remove from this device…").tap();
          await screen.getByRole("button", "Confirm").tap();
          await expect(moreActions).toBeHidden({ timeout: 30_000 });
        }
      } finally {
        try {
          await runPromise(unlinkEnvironment(primary, linked));
        } finally {
          await second.stop();
        }
      }
    }
  },
);
