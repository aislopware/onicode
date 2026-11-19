import { ThreadId } from "@onicode/contracts";
import { expect } from "e2e";
import * as Effect from "effect/Effect";

import { ONICODE_HOME } from "../e2e.config.ts";
import { rpcClient, runPromise, test, WEB } from "./onicode.ts";

test(
  "The right panel removes a terminal tab closed by another client",
  WEB,
  async ({ onicode, app, screen }) => {
    const cwd = await onicode.project("demo");
    const thread = await onicode.thread({
      project: cwd,
      title: "Shared panel terminals",
      messages: ["first"],
    });
    await onicode.open(thread);

    await screen.getByRole("button", "Toggle right panel").tap();
    await screen.getByRole("button", "Terminal T").tap();
    await screen.getByRole("button", "Add panel surface").tap();
    await screen.getByRole("menuitem", "Terminal T").tap();
    const closedTab = screen.getByText("Terminal 2", { exact: true });
    await closedTab.waitFor();
    await screen.getByText("Terminal 1", { exact: true }).tap();

    await runPromise(
      Effect.gen(function* () {
        const client = yield* rpcClient(ONICODE_HOME, app.baseUrl ?? "");
        yield* client["terminal.close"]({
          threadId: ThreadId.make(thread.threadId),
          terminalId: "term-2",
          deleteHistory: true,
        });
      }).pipe(Effect.scoped),
    );

    await app.screenshot("after-close-from-another-client");
    await expect(closedTab).toBeHidden();
  },
);
