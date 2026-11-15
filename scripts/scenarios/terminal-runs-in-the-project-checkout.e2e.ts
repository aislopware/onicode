import { expect } from "e2e";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";

import { test, WEB } from "./desktop.ts";
import { runPromise } from "./onicode.ts";

test(
  "The terminal drawer runs commands in the project checkout",
  WEB,
  async ({ onicode, browser, screen }) => {
    const project = await onicode.project("demo");
    const thread = await onicode.thread({ project, title: "Terminal home", messages: ["first"] });
    await onicode.open(thread);
    const proof = () =>
      runPromise(
        Effect.flatMap(FileSystem.FileSystem, (fs) =>
          fs.readFileString(`${project}/proof.txt`).pipe(Effect.orElseSucceed(() => null)),
        ),
      );

    await browser.keyboard.press("ControlOrMeta+j");
    const terminal = screen.getByRole("region", "Terminal");
    const input = terminal.getByRole("textbox", "Terminal input");
    await expect(input).toBeFocused();
    await input.pressSequentially("pwd > proof.txt");
    await input.press("Enter");
    await expect.poll(async () => (await proof())?.trim()).toBe(project);
    // No screenshot: the terminal runs the developer's own shell, and its prompt is theirs.

    await terminal.getByRole("button", /^Close Terminal/).tap();
    await expect(terminal).toBeHidden();
  },
);
