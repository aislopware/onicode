import {
  BearerConnectionProfile,
  BearerConnectionTarget,
  type ConnectionCatalogEntry,
  type ConnectionRoute,
  mergeLearnedRoutes,
} from "@onicode/client-runtime/connection";
import { EnvironmentId } from "@onicode/contracts";
import * as Option from "effect/Option";
import { expect, it, vi } from "vite-plus/test";
import { page, userEvent } from "vite-plus/test/browser";

vi.mock(import("~/state/session"), async (importOriginal) => ({
  ...(await importOriginal()),
  usePreparedConnection: (() => Option.none()) as never,
}));

vi.mock(import("~/state/use-atom-command"), async (importOriginal) => ({
  ...(await importOriginal()),
  useAtomCommand: (() => async () => ({ _tag: "Success" })) as never,
}));

vi.mock("../chat/GlobalVoiceInputIndicator", () => ({ GlobalVoiceInputIndicator: () => null }));

import { render } from "../../../test/render";
import type { EnvironmentPresentation } from "~/state/environments";
import { AppRouteModal } from "../AppRouteModal";
import { EnvironmentRoutesList } from "./EnvironmentRoutesList";

const ENVIRONMENT_ID = EnvironmentId.make("environment-1");

function direct(id: string, httpBaseUrl: string): ConnectionRoute {
  return {
    target: new BearerConnectionTarget({
      environmentId: ENVIRONMENT_ID,
      label: "Desk",
      connectionId: id,
    }),
    profile: Option.some(
      new BearerConnectionProfile({
        connectionId: id,
        environmentId: ENVIRONMENT_ID,
        label: "Desk",
        httpBaseUrl,
        wsBaseUrl: httpBaseUrl.replace(/^http/, "ws"),
      }),
    ),
  };
}

const LAN = direct("lan", "http://192.168.1.10:3773/");
const TAILNET = direct("tailnet", "https://desk.tail1234.ts.net/");
const PUBLIC = direct("public", "https://desk.example.com/");

function routesList(routes: ReadonlyArray<ConnectionRoute>) {
  const [first, ...rest] = routes;
  const entry: ConnectionCatalogEntry = {
    target: first!.target,
    profile: first!.profile,
    alternateRoutes: rest,
    enabled: true,
  };
  const environment: EnvironmentPresentation = {
    environmentId: ENVIRONMENT_ID,
    label: "Desk",
    displayUrl: null,
    entry,
    connection: { phase: "available", error: null, traceId: null },
    serverConfig: null,
    refusedRouteIds: [],
  };
  return <EnvironmentRoutesList environment={environment} onAddRoute={() => undefined} />;
}

it("shows a route added after a reorder was saved, once another route is removed", async () => {
  await using screen = await render(routesList([LAN, TAILNET]));
  await userEvent.click(screen.getByRole("button", { name: "Reorder LAN, position 1" }));
  await userEvent.keyboard(" ");
  await userEvent.keyboard("{ArrowDown}");
  await userEvent.keyboard(" ");
  await expect
    .element(screen.getByRole("button", { name: "Reorder Tailscale HTTPS, position 1" }))
    .toBeVisible();

  await screen.rerender(routesList([TAILNET, LAN]));
  await screen.rerender(routesList([TAILNET, LAN, PUBLIC]));
  await screen.rerender(routesList([TAILNET, PUBLIC]));

  await expect
    .element(screen.getByRole("button", { name: "Reorder desk.example.com, position 2" }))
    .toBeVisible();
  expect(screen.getByRole("listitem").elements()).toHaveLength(2);
  await expect
    .element(screen.getByRole("button", { name: "Remove desk.example.com route" }))
    .toBeVisible();
});

it("Esc cancels a keyboard reorder, and only the next one closes Settings", async () => {
  await using _screen = await render(
    <AppRouteModal label="Settings">{routesList([LAN, TAILNET])}</AppRouteModal>,
    { router: "/settings/connections" },
  );
  const settings = page.getByRole("dialog", { name: "Settings" });
  const handle = page.getByRole("button", { name: "Reorder LAN, position 1" });
  // The modal's inert layer takes pointer presses in this harness; the reorder is a keyboard one.
  await expect.element(handle).toBeVisible();
  (handle.element() as HTMLElement).focus();
  await userEvent.keyboard(" ");
  await expect.element(handle).toHaveAttribute("aria-pressed", "true");
  await userEvent.keyboard("{ArrowDown}");

  await userEvent.keyboard("{Escape}");
  await expect.element(handle).not.toHaveAttribute("aria-pressed");
  await expect.element(settings).toHaveAttribute("data-open");

  await userEvent.keyboard("{Escape}");
  await expect.element(settings).not.toBeInTheDocument();
});

it("does not offer to remove the only route a found-automatically route depends on", async () => {
  const paired: ConnectionCatalogEntry = {
    target: LAN.target,
    profile: LAN.profile,
    alternateRoutes: [],
    enabled: true,
  };
  const learned = mergeLearnedRoutes({
    entry: paired,
    activeRoute: LAN,
    reported: [{ httpBaseUrl: "https://desk.tail1234.ts.net/", kind: "tailnet" }],
    allowInsecure: true,
  })!;
  await using screen = await render(routesList(learned));
  await expect.element(screen.getByText(/found automatically/)).toBeVisible();

  expect(screen.getByRole("button", { name: "Remove LAN route" }).elements()).toHaveLength(0);
});
