// @effect-diagnostics nodeBuiltinImport:off -- The scenario starts a local ACP fixture.
import * as NodePath from "node:path";

import { ProviderDriverKind, ProviderInstanceId } from "@onicode/contracts";
import { expect } from "e2e";
import * as Effect from "effect/Effect";

import { ONICODE_HOME } from "../e2e.config.ts";
import {
  expectHeldImageCompressions,
  finishImageCompression,
  holdImageCompression,
  pasteWideScreenshot,
} from "./image-compression-hold.ts";
import { makeState, rpcClient, runPromise, test, WEB } from "./onicode.ts";

const ACP = `
import { createInterface } from "node:readline";

const sessionId = "image-question-session";
const send = (message) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...message }) + "\\n");
let promptId;
for await (const line of createInterface({ input: process.stdin })) {
  const { id, method } = JSON.parse(line);
  if (method === "session/cancel" && promptId !== undefined) {
    send({ id: promptId, result: { stopReason: "cancelled" } });
    promptId = undefined;
  }
  if (id === "ask-image-question" && method === undefined) {
    const answer = JSON.stringify(JSON.parse(line).result?.content ?? null);
    send({ method: "session/update", params: { sessionId, update: { sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "Answer received: " + answer } } } });
    send({ id: promptId, result: { stopReason: "end_turn" } });
    promptId = undefined;
    continue;
  }
  if (id === undefined || method === undefined) continue;
  if (method === "initialize") {
    send({ id, result: { protocolVersion: 1, agentCapabilities: { promptCapabilities: { image: true } }, authMethods: [],
      agentInfo: { name: "Question fixture", version: "1.0.0" } } });
  } else if (method === "session/new") {
    send({ id, result: { sessionId } });
  } else if (method === "session/prompt") {
    promptId = id;
    send({
      id: "ask-image-question",
      method: "elicitation/create",
      params: {
        sessionId,
        mode: "form",
        message: "What should we look at?",
        requestedSchema: {
          type: "object",
          properties: { answer: { type: "string", title: "Screenshot question" } },
          required: ["answer"],
        },
      },
    });
  } else {
    send({ id, error: { code: -32601, message: "Method not found" } });
  }
}
`;

test(
  "An answer sent while its screenshot prepares still goes after the user switches threads",
  WEB,
  async ({ onicode, app, screen, browser }) => {
    await holdImageCompression(browser);
    const project = await onicode.project("demo", { "question-agent.mjs": ACP });
    await onicode.thread({ project, title: "Earlier work", messages: ["first"] });
    const instanceId = ProviderInstanceId.make("scenario-question-agent");
    const settings = (mutation: object) =>
      runPromise(
        Effect.gen(function* () {
          const client = yield* rpcClient(ONICODE_HOME, app.baseUrl ?? "");
          yield* client["server.updateSettings"]({
            patch: {},
            providerInstanceMutation: { instanceId, ...mutation } as never,
          });
        }).pipe(Effect.scoped),
      );
    await settings({
      operation: "upsert",
      instance: {
        driver: ProviderDriverKind.make("acpRegistry"),
        displayName: "Question fixture",
        enabled: true,
        config: {
          source: "local",
          commandPath: process.execPath,
          commandArgs: [NodePath.join(project, "question-agent.mjs")],
        },
      },
    });
    try {
      const { cli } = makeState(ONICODE_HOME);
      const started = await runPromise(
        cli(
          [
            "thread",
            "start",
            project,
            "Ask for a screenshot.",
            "--title",
            "Screenshot question thread",
            "--provider",
            instanceId,
            "--model",
            "default",
            "--wait",
            "--json",
          ],
          [2],
        ),
      );
      const { threadId } = JSON.parse(started.stdout) as { threadId: string };
      await onicode.open({ threadId });

      const composer = browser.locator('[data-chat-composer-form="true"]');
      const question = browser.locator(
        '[data-chat-composer-form="true"] [data-pending-user-input-toggle]',
      );
      await question.waitFor({ timeout: 60_000 });
      await composer.getByRole("textbox").fill("Here is the screenshot.");
      await pasteWideScreenshot(browser);
      await expectHeldImageCompressions(browser, 1);
      // Submit waits on preparing images; Enter is how a held answer gets sent.
      await composer.getByRole("textbox").press("Enter");
      const row = (title: string) =>
        screen.getByTestId("sidebar-row-card").filter({ hasText: title });
      await row("Earlier work").tap();
      await question.waitFor({ state: "hidden" });

      await finishImageCompression(browser);
      // The agent took the answer once its thread no longer waits on input.
      await expect(row("Screenshot question thread")).not.toContainText("Input");
      await onicode.open({ threadId });
      await app.screenshot("question-thread-after-returning");
      await expect(screen.getByText(/Answer received: .*Here is the screenshot\./)).toBeVisible();
    } finally {
      // The server outlives the test; a second enabled provider breaks later scenarios.
      await settings({ operation: "remove" }).catch(() => {});
    }
  },
);
