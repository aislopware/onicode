import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

test(
  "Sidebar search finds a thread by its title and opens it",
  WEB,
  async ({ onicode, app, screen }) => {
    const project = await onicode.project("demo");
    await onicode.thread({ project, title: "Apple harvest", messages: ["first"] });
    await onicode.thread({ project, title: "Pear harvest", messages: ["first"] });
    await onicode.open();

    await screen.getByRole("combobox", "Search threads").fill("Pear");
    const results = screen.getByRole("listbox", "Thread search results");
    await expect(results).toContainText("Pear harvest");
    await expect(results).not.toContainText("Apple harvest");
    await app.screenshot("results");

    await results.getByRole("option", /Pear harvest/).tap();
    await expect(screen.getByRole("heading", "Pear harvest")).toBeVisible();
    await expect(screen.getByText("first fixture turn complete")).toBeVisible();
  },
);
