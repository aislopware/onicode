import { EnvironmentId, ThreadId, type DiscoveredLocalServer } from "@onicode/contracts";
import { expect, it, vi } from "vite-plus/test";
import { page } from "vite-plus/test/browser";

const mocks = vi.hoisted(() => ({
  shell: { openExternal: vi.fn(async (_url: string) => undefined) },
  canForward: false,
  forwards: [] as Array<{ environmentId: string; remotePort: number; localPort: number }>,
}));
const setup = (options: {
  canForward: boolean;
  forwards: Array<{ environmentId: string; remotePort: number; localPort: number }>;
}) => {
  mocks.shell.openExternal.mockClear();
  mocks.canForward = options.canForward;
  mocks.forwards = options.forwards;
};

vi.mock("~/env", () => ({ isElectron: true }));
vi.mock(import("../../state/portForwards"), async (importOriginal) => ({
  ...(await importOriginal()),
  canForwardPorts: () => mocks.canForward,
  useForwardLocality: (environmentId) => (environmentId === null ? "unknown" : "remote"),
  usePortForwards: (() => mocks.forwards) as never,
  forwardPort: (async (environmentId: string, remotePort: number) => ({
    environmentId,
    remotePort,
    localPort: 45000,
  })) as never,
}));
vi.mock(import("../../state/session"), async (importOriginal) => ({
  ...(await importOriginal()),
  useEnvironmentScope: () => true,
}));
vi.mock(import("../../state/environments"), async (importOriginal) => ({
  ...(await importOriginal()),
  useEnvironment: (() => ({
    connection: { phase: "connected" },
    entry: { target: { _tag: "BearerConnectionTarget", connectionId: "saved:remote" } },
    serverConfig: null,
  })) as never,
}));
vi.mock(import("../../localApi"), async (importOriginal) => ({
  ...(await importOriginal()),
  readLocalApi: (() => ({ shell: mocks.shell })) as never,
}));

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

import { render } from "../../../test/render";
import { ThreadServersPanel } from "./ThreadServersPanel";

const panel = (threadId: string) => (
  <ThreadServersPanel
    environmentId={EnvironmentId.make("env")}
    threadId={ThreadId.make(threadId)}
  />
);

it("pages servers four at a time, collapses back, and resets on thread switch", async () => {
  setup({ canForward: false, forwards: [] });
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
  setup({
    canForward: false,
    forwards: [{ environmentId: "env", remotePort: 3000, localPort: 3000 }],
  });
  await using _screen = await render(panel("a"));

  await expect
    .element(page.getByRole("button", { name: "Stop forwarding localhost:3000" }))
    .toBeVisible();
  await expect
    .element(page.getByRole("button", { name: "Forward localhost:3001 to this machine" }))
    .not.toBeInTheDocument();
});

it("opens a remote server in the system browser through the forward a modified click starts", async () => {
  setup({ canForward: true, forwards: [] });
  await using _screen = await render(panel("a"));
  await page.getByText("localhost:3000", { exact: true }).click({ modifiers: ["ControlOrMeta"] });
  await vi.waitFor(() =>
    expect(mocks.shell.openExternal).toHaveBeenCalledWith("http://localhost:45000/"),
  );
});
