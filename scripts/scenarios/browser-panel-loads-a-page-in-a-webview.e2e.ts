// @effect-diagnostics-next-line nodeBuiltinImport:off - one fixture page for the browser panel to load
import * as NodeHttp from "node:http";

import * as NodeServices from "@effect/platform-node/NodeServices";
import { expect } from "e2e";
import * as Effect from "effect/Effect";

import { readDesktopStack } from "../e2e.config.ts";
import { test, WEB } from "./onicode.ts";

const PAGE = "<!doctype html><title>Scenario page</title><h1>Scenario page</h1>";

interface CdpTarget {
  readonly type: string;
  readonly title: string;
}

test(
  "The browser panel loads a page in a webview of its own and closes it with the tab",
  WEB,
  async ({ onicode, screen }) => {
    const { cdpPort } = await Effect.runPromise(
      readDesktopStack.pipe(Effect.provide(NodeServices.layer)),
    );
    const webviews = async () => {
      // @effect-diagnostics-next-line globalFetch:off - one read of the window's DevTools target list
      const targets = await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json();
      return (targets as ReadonlyArray<CdpTarget>)
        .filter((target) => target.type === "webview")
        .map((target) => target.title);
    };
    const server = NodeHttp.createServer((_request, response) => {
      response.writeHead(200, { "content-type": "text/html" });
      response.end(PAGE);
    });
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
    try {
      const address = server.address();
      if (address === null || typeof address === "string") throw new Error("No page port.");
      const project = await onicode.project("demo");
      const thread = await onicode.thread({ project, title: "Browser home", messages: ["first"] });
      await onicode.open(thread);

      await screen.getByRole("button", "Toggle right panel").tap();
      await screen.getByRole("button", "Browser B").tap();
      const panel = screen.getByRole("region", "Workspace panel: Browser");
      const url = panel.getByRole("textbox", "Search or enter URL");
      await url.fill(`http://127.0.0.1:${address.port}/`);
      await url.press("Enter");
      await expect.poll(webviews).toEqual(["Scenario page"]);
      // The panel takes the page's title, and a screenshot never returns while a webview shows one.
      await expect(screen.getByRole("region", "Workspace panel: Scenario page")).toBeVisible();

      await screen.getByRole("button", "Close Scenario page").tap();
      await expect.poll(webviews).toEqual([]);
    } finally {
      await new Promise((resolve) => server.close(resolve));
    }
  },
);
