/// <reference lib="dom" />
import { expect } from "e2e";

import { environmentId, runPromise, test, WEB } from "./onicode.ts";

test(
  "A thread link pasted after a list code example becomes a chip",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    const project = await onicode.project("demo");
    const source = await onicode.thread({
      project,
      title: "Release checklist",
      messages: ["Checklist"],
    });
    const target = await onicode.thread({ project, title: "Release notes", messages: ["Notes"] });
    const environment = await runPromise(environmentId(app.baseUrl ?? ""));
    const href = `oni-thread://v2/${environment}/${encodeURIComponent(source.threadId)}`;
    await onicode.open(target);
    await screen.getByRole("textbox").tap();

    await browser.evaluate((text) => {
      const editor = document.activeElement;
      if (!(editor instanceof HTMLElement) || !editor.isContentEditable) {
        throw new Error("The composer is not focused");
      }
      const clipboardData = new DataTransfer();
      clipboardData.setData("text/plain", text);
      editor.dispatchEvent(
        new ClipboardEvent("paste", { clipboardData, bubbles: true, cancelable: true }),
      );
      return null;
    }, `- \`\`\`text\n  example\n\nSee [Release checklist](${href})`);

    await app.screenshot("pasted-thread-link");
    await expect(browser.locator('[role="textbox"] [data-slot="context-chip-label"]')).toHaveText(
      "Release checklist",
    );
  },
);
