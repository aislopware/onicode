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
  fallback: string,
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
    alternateUrls: [fallback],
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
  "Connect to interact opens the editable thread before a silent fallback times out",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    await onicode.project("primary");
    const primary = app.baseUrl ?? "";
    const second = await startSecondEnvironment(SECOND_HOME);
    const moreActions = screen.getByRole("button", `More actions for ${ENVIRONMENT_LABEL}`);
    let linked: EnvironmentId | undefined;
    try {
      const { host } = second;
      const alternate = new URL(host);
      alternate.hostname = "127.0.0.2";
      linked = await runPromise(linkEnvironments(primary, host, alternate.origin));

      const project = await second.state.project("linked");
      const thread = await second.state.thread({
        project,
        title: "Linked handoff",
        messages: ["first"],
      });
      const via = await runPromise(environmentId(primary));
      const target = await runPromise(environmentId(host));

      await browser.addInitScript((fallback) => {
        const fetchActual = globalThis.fetch;
        globalThis.fetch = (input, init) => {
          const url = new URL(input instanceof Request ? input.url : String(input), location.href);
          if (url.origin !== fallback || url.pathname !== "/.well-known/onicode/environment") {
            // @effect-diagnostics-next-line globalFetch:off - This interception runs inside the browser.
            return fetchActual(input, init);
          }
          // The network silently drops this route. The preferred address is still real.
          return new Promise((_, reject) => {
            const signal = init?.signal ?? (input instanceof Request ? input.signal : undefined);
            signal?.addEventListener("abort", () => reject(signal.reason), { once: true });
          });
        };
      }, alternate.origin);

      await onicode.open(`/linked/${via}/${target}/${thread.threadId}`);
      await screen.getByRole("heading", "Linked handoff").waitFor();
      await screen.getByRole("button", "Connect to interact").tap();

      try {
        // The desktop renderer routes by hash.
        await expect
          .poll(() => browser.evaluate(() => location.hash.split("?")[0] ?? ""), { timeout: 4_000 })
          .toBe(`#/${target}/${encodeURIComponent(thread.threadId)}`);
      } finally {
        await app.screenshot("linked-thread-after-connect");
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
        // A pairing still finishing lands here; its row may appear only after the page renders.
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
