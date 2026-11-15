// `test` for scenarios about the desktop shell: `onicode.ts`'s test plus `desktop`, which reaches
// what the window's web page cannot show: native menus, native dialogs and the window itself.
import * as NodeServices from "@effect/platform-node/NodeServices";
import { expect, type Locator } from "e2e";
import * as Effect from "effect/Effect";

import { readDesktopStack } from "../e2e.config.ts";
import { mainProcess } from "./desktop-main.ts";
import { test as base } from "./onicode.ts";

export { WEB } from "./onicode.ts";

export interface Desktop extends ReturnType<typeof mainProcess> {
  /** Opens an element's native context menu and clicks one item, by its label path. */
  readonly chooseFromContextMenu: (
    target: Locator,
    ...path: ReadonlyArray<string>
  ) => Promise<void>;
}

export const test = base.extend<{ desktop: Desktop }>({
  // oxlint-disable-next-line no-empty-pattern -- e2e reads a fixture's dependencies from this pattern
  desktop: async ({}, use) => {
    const { inspectPort } = await Effect.runPromise(
      readDesktopStack.pipe(Effect.provide(NodeServices.layer)),
    );
    const main = mainProcess(inspectPort);
    // What an earlier scenario left waiting would answer this one's first step.
    await main.dismissContextMenus();
    await main.takeMessageBoxes();
    await main.forgetFolderPickerAnswers();
    await use({
      ...main,
      chooseFromContextMenu: async (target, ...path) => {
        await target.secondaryTap();
        await expect
          .poll(() => main.contextMenu(), { message: "no context menu opened" })
          .not.toBeNull();
        await main.chooseContextMenuItem(...path);
      },
    });
  },
});
