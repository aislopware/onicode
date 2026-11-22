import { expect } from "e2e";

import { ONICODE_HOME } from "../e2e.config.ts";
import { makeState, runPromise, test, WEB } from "./onicode.ts";

test(
  "A queued message shows its @-file mention by name, not as a raw link",
  WEB,
  async ({ onicode, app, screen, browser }) => {
    const project = await onicode.project("demo", { "src/foo.ts": "export {};\n" });
    const thread = await onicode.thread({
      project,
      title: "Long work",
      messages: ["Show me around"],
      model: "showcase",
    });
    await onicode.open(thread);

    const composer = screen.getByRole("textbox");
    await composer.pressSequentially("Review @foo");
    await screen.getByRole("option", "foo.ts", { exact: false }).tap();
    await expect(composer).toContainText("foo.ts");

    // The showcase recording streams for several seconds, long enough to queue behind its turn.
    await runPromise(
      makeState(ONICODE_HOME).cli(["thread", "send", thread.threadId, "Show me again"]),
    );
    await screen.getByText("Working for", { exact: false }).waitFor();
    // Enter steers by default; the alternate send queues.
    await browser.keyboard.press("ControlOrMeta+Enter");

    const queued = screen.getByRole("listitem", "Queued message 1 of 1, not sent");
    await queued.waitFor();
    // Read the row once: the queued message sends, and its row goes, when the turn ends.
    const text = await queued.textContent();
    await app.screenshot("queued-row");
    expect(text).toContain("Review foo.ts");
  },
);
