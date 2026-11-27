/// <reference lib="dom" />
import { expect } from "e2e";

import { environmentId, runPromise, test, WEB } from "./onicode.ts";

test(
  "A pasted thread link after a carriage-return blank line becomes a chip",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    const project = await onicode.project("demo");
    const linked = await onicode.thread({
      project,
      title: "Release notes",
      messages: ["Draft the notes"],
    });
    const thread = await onicode.thread({
      project,
      title: "Follow-up",
      messages: ["Start a follow-up"],
    });
    await onicode.open(thread);
    const composer = screen.getByRole("textbox");
    await composer.tap();
    // The desktop renderer routes by hash, so the path carries no environment id.
    const environment = await runPromise(environmentId(app.baseUrl ?? ""));
    await browser.evaluate(
      ({ environmentId, threadId }: { environmentId: string; threadId: string }) => {
        const href = `oni-thread://v2/${environmentId}/${threadId}`;
        const clipboardData = new DataTransfer();
        clipboardData.setData("text/plain", `First paragraph \`\r\r<${href}>\rSecond paragraph \``);
        const editor = document.activeElement;
        if (!editor?.matches('[contenteditable="true"]'))
          throw new Error("Composer is not focused");
        return editor.dispatchEvent(
          new ClipboardEvent("paste", { clipboardData, bubbles: true, cancelable: true }),
        );
      },
      { environmentId: environment, threadId: linked.threadId },
    );
    await composer.getByText("First paragraph", { exact: false }).waitFor();
    await app.screenshot("pasted-thread-link");
    await expect(composer.getByText("Release notes", { exact: true })).toBeVisible();
  },
);
