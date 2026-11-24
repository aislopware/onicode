import { expect, it, vi } from "vite-plus/test";
import { page, userEvent } from "vite-plus/test/browser";

import { act, render, stubGlobal } from "../../../test/render";
import { AppRouteModal } from "../AppRouteModal";
import { OpenSourceLicensesPanel } from "./OpenSourceLicenses";

vi.mock("../chat/GlobalVoiceInputIndicator", () => ({ GlobalVoiceInputIndicator: () => null }));

it("Esc in the license search closes it without closing Settings", async () => {
  const entry = {
    bundles: ["web"],
    kind: "package",
    license: "MIT",
    name: "react",
    noticeText: "MIT License",
    sourceUrl: null,
    version: "19.0.0",
  };
  const realFetch = globalThis.fetch.bind(globalThis);
  using _fetch = stubGlobal("fetch", (input: RequestInfo | URL, init?: RequestInit) =>
    (input instanceof Request ? input.url : String(input)).endsWith("/third-party-licenses.json")
      ? Promise.resolve(Response.json({ schemaVersion: 1, entries: [entry] }))
      : realFetch(input, init),
  );
  await using _screen = await render(
    <AppRouteModal label="Settings">
      <OpenSourceLicensesPanel />
    </AppRouteModal>,
    { router: "/settings/open-source-licenses" },
  );
  const settings = page.getByRole("dialog", { name: "Settings" });
  const search = page.getByRole("searchbox", { name: "Search open-source licenses" });
  const openSearch = async () => {
    const button = page.getByRole("button", { name: "Search open-source licenses" });
    await expect.element(button).toBeVisible();
    (button.element() as HTMLElement).focus();
    await userEvent.keyboard("{Enter}");
    await expect.element(search).toHaveFocus();
  };
  await openSearch();
  await search.fill("react");

  // The Esc that cancels an IME composition leaves the search and Settings open.
  const composingEscape = new KeyboardEvent("keydown", {
    key: "Escape",
    keyCode: 229,
    isComposing: true,
    bubbles: true,
    cancelable: true,
  });
  await act(() => search.element().dispatchEvent(composingEscape));
  expect(composingEscape.defaultPrevented).toBe(false);
  await expect.element(search).toHaveValue("react");
  await expect.element(settings).toHaveAttribute("data-open");

  await userEvent.keyboard("{Escape}");

  await expect.element(settings).toHaveAttribute("data-open");
  await expect.element(search).not.toBeInTheDocument();
  await expect.element(settings).toHaveFocus();

  await openSearch();
  await expect.element(search).toHaveValue("");
  await userEvent.keyboard("{Escape}");

  await expect.element(settings).toHaveAttribute("data-open");
  await expect.element(search).not.toBeInTheDocument();
  await expect.element(settings).toHaveFocus();

  await userEvent.keyboard("{Escape}");

  await expect.element(settings).not.toBeInTheDocument();
});
