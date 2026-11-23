// @effect-diagnostics-next-line nodeBuiltinImport:off - one fixture page for the server's browser to load
import * as NodeHttp from "node:http";

import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

// One app link that fills the page. Its click renames the page a moment later, after the server
// has seen the link's request.
const PAGE = `<!doctype html>
<title>Note link</title>
<a href="obsidian://open?vault=demo&amp;file=Note" style="display:block;height:100vh"
  onclick="setTimeout(function () { document.title = 'Link clicked'; }, 500)">Open the note</a>`;

test(
  "An app link clicked in the Browser leaves the page shown and loaded",
  WEB,
  async ({ onicode, desktop, app, browser, screen }) => {
    const server = NodeHttp.createServer((_request, response) => {
      response.writeHead(200, { "content-type": "text/html" });
      response.end(PAGE);
    });
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
    try {
      const address = server.address();
      if (address === null || typeof address === "string") throw new Error("No page port.");
      const url = `http://127.0.0.1:${address.port}/`;
      const project = await onicode.project("demo");
      const thread = await onicode.thread({ project, title: "Note check", messages: ["first"] });
      await onicode.open(thread);

      await screen.getByRole("button", "Toggle right panel").tap();
      await screen.getByRole("button", "Browser B").tap();
      const addressBar = screen.getByRole("textbox", "Search or enter URL");
      await addressBar.fill(url);
      await browser.keyboard.press("Enter");
      await expect(screen.getByText("Note link")).toBeVisible();

      // The desktop app shows the page in a webview of its own, not on the server browser's canvas.
      await browser.locator("webview").tap();
      try {
        // The desktop asks before handing the link to another app; the stack answers its first
        // button. The page stays and goes on renaming itself.
        await expect
          .poll(() => desktop.takeMessageBoxes())
          .toEqual([expect.objectContaining({ message: "Open this obsidian link?" })]);
        await expect(screen.getByText("Link clicked")).toBeVisible({ timeout: 2_000 });
        await expect(addressBar).toHaveValue(url);
        await expect(screen.getByRole("button", "Refresh", { exact: true })).toBeVisible();
      } finally {
        await app.screenshot("after-click");
      }
    } finally {
      await new Promise((resolve) => server.close(resolve));
    }
  },
);
