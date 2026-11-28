/// <reference lib="dom" />
import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

test(
  "Editing a repository name supersedes its pending lookup",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    const project = await onicode.project("demo");
    const thread = await onicode.thread({ project, title: "Current work", messages: ["Start"] });

    await onicode.open(thread);
    await screen.getByText("Start", { exact: true }).waitFor({ timeout: 30_000 });
    await browser.evaluate(() => {
      const send = WebSocket.prototype.send;
      WebSocket.prototype.send = function (data) {
        const request = typeof data === "string" ? JSON.parse(data) : null;
        if (request?._tag !== "Request") return send.call(this, data);
        const reply = (value: unknown) =>
          this.dispatchEvent(
            new MessageEvent("message", {
              data: JSON.stringify({
                _tag: "Exit",
                requestId: request.id,
                exit: { _tag: "Success", value },
              }),
            }),
          );

        if (request.tag === "server.discoverSourceControl") {
          const none = { _tag: "None" };
          queueMicrotask(() =>
            reply({
              versionControlSystems: [],
              sourceControlProviders: [
                {
                  kind: "github",
                  label: "GitHub",
                  status: "available",
                  version: none,
                  installHint: "Install GitHub CLI",
                  detail: none,
                  auth: { status: "authenticated", account: none, host: none, detail: none },
                },
              ],
            }),
          );
          return;
        }

        if (request.tag === "sourceControl.lookupRepository") {
          document.documentElement.dataset.scenarioLookup = "pending";
          document.addEventListener(
            "release-repository-lookup",
            () =>
              reply({
                provider: "github",
                nameWithOwner: "example/slow",
                url: "https://github.com/example/slow",
                sshUrl: "git@github.com:example/slow.git",
              }),
            { once: true },
          );
          return;
        }
        return send.call(this, data);
      };
      return null;
    });

    await browser.keyboard.press("ControlOrMeta+k");
    const palette = screen.getByRole("dialog", "Command palette");
    await palette.getByRole("combobox").fill("> Add project");
    await palette.getByText("Add project", { exact: true }).tap();
    await palette.getByRole("option", /^GitHub repository/).tap();
    await palette.getByRole("combobox").fill("example/slow");
    await screen.getByRole("button", "Lookup (Enter)").tap();
    await browser.locator("html[data-scenario-lookup=pending]").waitFor();
    const repository = palette.getByRole("combobox");
    await repository.fill("example/corrected");

    await browser.evaluate(async () => {
      // The desktop window's onicode-dev:// scheme records no resource timing, so the app's React
      // module URL is rebuilt from the hash Vite stamps on every prebundled dependency.
      // @effect-diagnostics-next-line globalFetch:off - runs in the window, outside any Effect
      const metadata = await fetch("/node_modules/.vite/deps/_metadata.json");
      const { browserHash } = (await metadata.json()) as { browserHash?: string };
      if (!browserHash) throw new Error("The app's React module has not loaded");
      const { default: react } = await import(`/node_modules/.vite/deps/react.js?v=${browserHash}`);
      const { appAtomRegistry } = await import(String("/src/rpc/atomRegistry.ts"));
      const { sourceControlEnvironment } = await import(String("/src/state/sourceControl.ts"));
      const environmentId = location.hash.split("/")[1];
      const input = { provider: "github", repository: "example/slow" };
      const atom = sourceControlEnvironment.repository({ environmentId, input });
      await react.act(async () => {
        const settled = Promise.withResolvers<void>();
        const unsubscribe = appAtomRegistry.subscribe(atom, settled.resolve);
        document.dispatchEvent(new Event("release-repository-lookup"));
        await settled.promise.finally(unsubscribe);
      });
      return null;
    });
    await app.screenshot("after-old-lookup-returns");
    expect(await repository.getAttribute("value")).toBe("example/corrected");
  },
);
