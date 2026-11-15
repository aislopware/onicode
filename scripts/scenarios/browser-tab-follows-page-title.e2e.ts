// @effect-diagnostics-next-line nodeBuiltinImport:off - one fixture page for the server's browser to load
import * as NodeHttp from "node:http";

import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

// A page that renames itself on a click, with no navigation.
const PAGE = `<!doctype html>
<title>Original title</title>
<body style="margin:0;height:100vh" onpointerdown="document.title = 'Updated title'">
  Click to rename
</body>`;

test(
  "A Browser tab's label follows a title change that makes no navigation",
  WEB,
  async ({ onicode, browser, screen }) => {
    const server = NodeHttp.createServer((_request, response) => {
      response.writeHead(200, { "content-type": "text/html" });
      response.end(PAGE);
    });
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
    try {
      const address = server.address();
      if (address === null || typeof address === "string") throw new Error("No page port.");
      const project = await onicode.project("demo");
      const thread = await onicode.thread({ project, title: "Rename check", messages: ["first"] });
      await onicode.open(thread);

      await screen.getByRole("button", "Toggle right panel").tap();
      await screen.getByRole("button", "Browser B").tap();
      await screen
        .getByRole("textbox", "Search or enter URL")
        .fill(`http://127.0.0.1:${address.port}/`);
      await browser.keyboard.press("Enter");
      await expect(screen.getByText("Original title")).toBeVisible();

      // The desktop app shows the page in a webview of its own, not on the server browser's canvas.
      await browser.locator("webview").tap();
      await expect(screen.getByText("Updated title")).toBeVisible();
      // No screenshot: capturing the window over CDP never returns while a webview shows a page.
    } finally {
      await new Promise((resolve) => server.close(resolve));
    }
  },
);
