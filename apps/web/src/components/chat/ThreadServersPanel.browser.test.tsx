import { EnvironmentId, ThreadId, type DiscoveredLocalServer } from "@onicode/contracts";
import { expect, it, vi } from "vite-plus/test";
import { page } from "vite-plus/test/browser";

vi.mock(import("../../portDiscoveryState"), async (importOriginal) => ({
  ...(await importOriginal()),
  useThreadDiscoveredPorts: () =>
    Array.from(
      { length: 10 },
      (_, index) =>
        ({
          pid: index,
          port: 3000 + index,
          url: `http://localhost:${3000 + index}`,
          terminal: null,
          processName: "node",
        }) as DiscoveredLocalServer,
    ),
}));

vi.mock(import("../../state/portForwards"), async (importOriginal) => ({
  ...(await importOriginal()),
  usePortForwards: () => [
    { environmentId: EnvironmentId.make("env"), remotePort: 3000, localPort: 3000 },
  ],
}));

import { render } from "../../../test/render";
import { ThreadServersPanel } from "./ThreadServersPanel";

const panel = (threadId: string) => (
  <ThreadServersPanel
    environmentId={EnvironmentId.make("env")}
    threadId={ThreadId.make(threadId)}
  />
);

it("pages servers four at a time, collapses back, and resets on thread switch", async () => {
  await using screen = await render(panel("a"));
  const count = (selector: string) => screen.container.querySelectorAll(selector).length;
  const expectRows = (rows: number) => vi.waitFor(() => expect(count("li")).toBe(rows));

  await expectRows(4);
  await page.getByRole("button", { name: "Show 4 more" }).click();
  await expectRows(8);
  expect(count("[data-thread-details-server-overflow]")).toBe(4);
  await page.getByRole("button", { name: "Show 2 more" }).click();
  await expectRows(10);
  await page.getByRole("button", { name: "Show less" }).click();
  await expectRows(4);

  await page.getByRole("button", { name: "Show 4 more" }).click();
  await expectRows(8);
  await screen.rerender(panel("b"));
  await expectRows(4);
  await screen.rerender(panel("a"));
  await expectRows(4);
});

it("keeps a held forward stoppable where new ports cannot be forwarded", async () => {
  await using _screen = await render(panel("a"));

  await expect
    .element(page.getByRole("button", { name: "Stop forwarding localhost:3000" }))
    .toBeVisible();
  await expect
    .element(page.getByRole("button", { name: "Forward localhost:3001 to this machine" }))
    .not.toBeInTheDocument();
});
