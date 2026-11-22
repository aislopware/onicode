/// <reference lib="dom" />
import type { EnvironmentId } from "@onicode/contracts";
import { expect } from "e2e";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";

import { ONICODE_HOME } from "../e2e.config.ts";
import { startSecondEnvironment } from "./second-environment.ts";
import { environmentId, makeState, rpcClient, runPromise, test, WEB } from "./onicode.ts";

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
    access: "approval-required",
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
  "Try again immediately reads a linked thread once it is reachable",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    await onicode.project("primary");
    const origin = app.baseUrl ?? "";
    const second = await startSecondEnvironment(SECOND_HOME);
    let linkedId: EnvironmentId | undefined;
    try {
      const { host } = second;
      linkedId = await runPromise(linkEnvironments(origin, host));

      const project = await second.state.project("linked");
      const prompt = "Read the linked design notes";
      const thread = await second.state.thread({
        project,
        title: "Design notes",
        messages: [prompt],
      });
      const via = await runPromise(environmentId(origin));
      const linked = await runPromise(environmentId(host));

      // Drop only the first subscription, as if the linked machine were briefly unreachable.
      // Every later request goes to the real server and reads the seeded thread.
      await browser.addInitScript(() => {
        let failed = false;
        const send = WebSocket.prototype.send;
        WebSocket.prototype.send = function (data) {
          if (failed || typeof data !== "string") return send.call(this, data);
          const parsed = JSON.parse(data);
          const messages = Array.isArray(parsed) ? parsed : [parsed];
          const request = messages.find(
            (message) => message?._tag === "Request" && message.tag === "peerLinks.subscribeThread",
          );
          if (!request) return send.call(this, data);
          failed = true;
          const others = messages.filter((message) => message !== request);
          if (others.length > 0) send.call(this, JSON.stringify(others));
          // @effect-diagnostics-next-line globalTimers:off - This callback is injected into the browser.
          setTimeout(() => {
            this.dispatchEvent(
              new MessageEvent("message", {
                data: JSON.stringify({
                  _tag: "Exit",
                  requestId: request.id,
                  exit: {
                    _tag: "Failure",
                    cause: [
                      {
                        _tag: "Fail",
                        error: {
                          _tag: "PeerLinkThreadReadError",
                          reason: "unreachable",
                          label: "Scenario",
                          message: "The linked machine is temporarily unreachable.",
                        },
                      },
                    ],
                  },
                }),
              }),
            );
          }, 0);
        };
      });

      await onicode.open(`/linked/${via}/${linked}/${thread.threadId}`);
      await screen.getByRole("button", "Try again", { exact: true }).tap();
      await app.screenshot("after-trying-again");
      // The automatic retry waits 15 seconds. This checks the user's explicit retry.
      await expect(screen.getByText(prompt, { exact: true })).toBeVisible({ timeout: 4_000 });
    } finally {
      try {
        await runPromise(unlinkEnvironment(origin, linkedId));
      } finally {
        await second.stop();
      }
    }
  },
);
