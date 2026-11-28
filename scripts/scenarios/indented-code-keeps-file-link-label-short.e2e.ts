import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

test(
  "Indented code does not qualify the label of a live file link",
  WEB,
  async ({ onicode, app, screen }) => {
    const project = await onicode.project("file links", {
      "src/a/index.ts": "export const a = 1;\n",
      "src/b/index.ts": "export const b = 2;\n",
    });
    const thread = await onicode.thread({
      project,
      title: "Module entry points",
      messages: [
        "Example link syntax:\n\n    [index.ts](src/a/index.ts)\n\nOpen [index.ts](src/b/index.ts).",
      ],
    });
    await onicode.open(thread);
    await screen.getByRole("button", /^Agent Example link syntax/).tap();
    const link = screen.getByRole("link", /^index\.ts/);
    await link.waitFor();
    await app.screenshot("file-link-after-indented-code");
    await expect(link).toHaveText("index.ts");
  },
);
