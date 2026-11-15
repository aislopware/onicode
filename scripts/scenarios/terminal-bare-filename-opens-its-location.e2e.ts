/// <reference lib="dom" />
// The DOM lib types the function this scenario runs in the page.
import { expect } from "e2e";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";

import { ONICODE_HOME } from "../e2e.config.ts";
import { runPromise, test, WEB } from "./onicode.ts";

// gcc prints a file compiled from its own folder by bare name.
const DIAGNOSTIC = "main.c:1:25: error: 'missing' undeclared";
const EDITOR_ARGS = `${ONICODE_HOME}/editor-args.txt`;

/** The file's lines, or none while it does not exist. */
const readLines = (file: string) =>
  runPromise(
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      if (!(yield* fs.exists(file))) return [];
      return (yield* fs.readFileString(file)).trim().split("\n");
    }),
  );

test(
  "Cmd/Ctrl+click on a bare compiler filename in the terminal opens that file at its line and column",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    await runPromise(
      Effect.flatMap(FileSystem.FileSystem, (fs) => fs.remove(EDITOR_ARGS, { force: true })),
    );
    const project = await onicode.project("demo", {
      "main.c": "int main(void) { return missing; }\n",
      // Enough rows that the middle of the terminal shows the diagnostic, whatever its height.
      "build.log": `${DIAGNOSTIC}\n`.repeat(200),
    });
    const thread = await onicode.thread({ project, title: "Build", messages: ["Build it"] });
    await onicode.open(thread);

    await screen.getByRole("button", "Toggle terminal drawer").tap();
    const input = screen.getByRole("textbox", "Terminal input");
    // The input mounts before fonts and WASM finish loading and its handlers are installed.
    await expect(input).toHaveAttribute("data-workspace-focus-default", "");
    await input.focus();
    await browser.keyboard.type("cat build.log; touch cat.done\n");
    await expect.poll(() => readLines(`${project}/cat.done`), { timeout: 30_000 }).toEqual([""]);
    await app.screenshot("diagnostic-printed");

    // The terminal draws on a canvas, so point at the first cells of its middle row.
    const canvas = await browser.evaluate(() => {
      const box = document
        .querySelector('[aria-label="Terminal input"]')
        ?.parentElement?.querySelector("canvas")
        ?.getBoundingClientRect();
      return box ? { x: box.x, y: box.y, width: box.width, height: box.height } : null;
    });
    if (!canvas) throw new Error("The terminal canvas is not on the page.");
    // A link opens on Cmd+click on macOS and Ctrl+click elsewhere.
    const linkModifier = HostProcess.Platform.defaultValue() === "darwin" ? "Meta" : "Control";
    await browser.mouse.move(canvas.x + 16, canvas.y + canvas.height / 2);
    await browser.keyboard.down(linkModifier);
    await browser.mouse.down();
    await browser.mouse.up();
    await browser.keyboard.up(linkModifier);
    await app.screenshot("after-ctrl-click");

    await expect
      .poll(() => readLines(EDITOR_ARGS), { timeout: 15_000 })
      .toEqual(["--classic", "--goto", `${project}/main.c:1:25`]);
  },
);
