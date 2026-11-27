/// <reference lib="dom" />
// @effect-diagnostics-next-line nodeBuiltinImport:off -- The fixture holds Git's HTTP request open.
import * as NodeHttp from "node:http";
import { expect } from "e2e";

import { ONICODE_HOME } from "../e2e.config.ts";
import { startSecondEnvironment } from "./second-environment.ts";
import { test, WEB } from "./onicode.ts";
import { ENVIRONMENT_LABEL } from "./start-app.ts";

test(
  "A dismissed clone start leaves the next Add to another machine dialog open",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    const remote = NodeHttp.createServer();
    await new Promise<void>((resolve) => remote.listen(0, "127.0.0.1", resolve));
    const second = await startSecondEnvironment(`${ONICODE_HOME}-second-environment`);
    const moreActions = screen.getByRole("button", `More actions for ${ENVIRONMENT_LABEL}`);
    let paired = false;
    try {
      const address = remote.address();
      if (!address || typeof address === "string") throw new Error("No Git fixture port");
      const seedProject = (name: string) =>
        onicode.project(name, {
          ".git/config": `[remote "origin"]\nurl = http://127.0.0.1:${address.port}/scenario/${name}.git\n`,
        });
      const first = await seedProject("first-repository");
      await seedProject("second-repository");

      // Deliver the real reply after the second dialog is open.
      await browser.addInitScript(() => {
        const startRequests = new WeakMap<WebSocket, string>();
        const send = WebSocket.prototype.send;
        WebSocket.prototype.send = function (data) {
          const request =
            typeof data === "string" && data.includes("projectClone.start")
              ? JSON.parse(data)
              : null;
          if (request?._tag === "Request" && request.tag === "projectClone.start") {
            startRequests.set(this, String(request.id));
          }
          return send.call(this, data);
        };
        const listen = WebSocket.prototype.addEventListener;
        WebSocket.prototype.addEventListener = function (
          this: WebSocket,
          type: string,
          listener: EventListenerOrEventListenerObject,
          options?: boolean | AddEventListenerOptions,
        ) {
          if (type !== "message" || typeof listener !== "function") {
            return listen.call(this, type, listener, options);
          }
          return listen.call(
            this,
            type,
            (event: Event) => {
              const { data } = event as MessageEvent;
              const wire = typeof data === "string" ? [JSON.parse(data)].flat() : [];
              if (
                wire.some(
                  (message) =>
                    message?._tag === "Exit" &&
                    message.exit?._tag === "Success" &&
                    String(message.requestId) === startRequests.get(this),
                )
              ) {
                document.documentElement.dataset.scenarioCloneReply = "ready";
                document.addEventListener(
                  "release-machine-clone-start",
                  () => listener.call(this, event),
                  {
                    once: true,
                  },
                );
              } else {
                listener.call(this, event);
              }
            },
            options,
          );
        } as typeof WebSocket.prototype.addEventListener;
      });

      const { host, token } = second;
      await browser.setViewport({ width: 1920, height: 1000 });
      await onicode.open("/settings/connections");
      await screen.getByRole("button", "Add environment").tap();
      const pairing = screen.getByRole("dialog");
      await pairing.getByPlaceholder("backend.example.com").fill(host);
      await pairing.getByPlaceholder("PAIRCODE").fill(token);
      await pairing.getByRole("button", "Add environment").tap();
      // The map is the default view; its rows carry no actions menu.
      await screen.getByRole("button", "List", { exact: true }).tap();
      await expect(moreActions).toBeVisible({ timeout: 60_000 });
      paired = true;

      await app.open("/");
      const projectPicker = browser.locator("[data-draft-project-trigger]");
      const dialog = screen.getByRole("dialog", "Add to another machine");
      const openFor = async (name: string) => {
        await projectPicker.tap();
        await screen.getByRole("option", name, { exact: true }).tap();
        await projectPicker.getByText(name, { exact: true }).waitFor();
        // The machine menu opens from the label of the machine the draft runs on.
        await screen.getByText(ENVIRONMENT_LABEL).last().tap();
        await screen.getByRole("menuitem", "Add to another machine").tap();
        await screen.getByRole("menuitem", ENVIRONMENT_LABEL, { exact: true }).last().tap();
        await dialog
          .getByText(`Clone scenario/${name} and bring the local files git does not track.`, {
            exact: true,
          })
          .waitFor();
      };
      await openFor("first-repository");
      await dialog.getByRole("textbox", /^Folder on/).fill(`${first}/../first-copy`);
      await dialog.getByRole("button", /^Clone to/).tap();
      await dialog.getByRole("button", "Starting…").waitFor();
      await dialog.getByRole("button", "Cancel", { exact: true }).tap();
      await openFor("second-repository");

      await browser.locator("html[data-scenario-clone-reply=ready]").waitFor();
      await browser.evaluate(async () => {
        document.dispatchEvent(new Event("release-machine-clone-start"));
        await new Promise<void>((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
        );
        return null;
      });
      await app.screenshot("dialog-after-dismissed-clone-start-returns");
      await expect(dialog).toBeVisible();
    } finally {
      remote.close();
      remote.closeAllConnections();
      // The desktop app keeps paired environments between scenarios, so a later one pairing
      // the same label would find two.
      try {
        if (paired) {
          await browser.keyboard.press("Escape");
          await app.open("/settings/connections");
          await screen.getByRole("button", "List", { exact: true }).tap();
          await moreActions.tap();
          await screen.getByRole("menuitem", "Remove from this device…").tap();
          await screen.getByRole("button", "Confirm").tap();
          await expect(moreActions).toBeHidden({ timeout: 30_000 });
        }
      } finally {
        await second.stop();
      }
    }
  },
);
