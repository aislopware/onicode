import { describe, expect, it, vi } from "vite-plus/test";
import { page, userEvent } from "vite-plus/test/browser";

import "~/index.css";
import { render } from "../../../test/render";
import { AppRouteModal } from "../AppRouteModal";
import { EnvironmentsMap } from "./EnvironmentsMap";

vi.mock("../chat/GlobalVoiceInputIndicator", () => ({ GlobalVoiceInputIndicator: () => null }));

const renderSettingsMap = () =>
  render(
    <AppRouteModal label="Settings">
      <EnvironmentsMap onAddEnvironment={() => {}} renderSavedEnvironment={() => null} />
    </AppRouteModal>,
    { router: "/settings/connections" },
  );

describe("EnvironmentsMap", () => {
  it("clears the selection on Esc before Settings closes", async () => {
    await using _screen = await renderSettingsMap();
    const settings = page.getByRole("dialog", { name: "Settings" });
    const browser = page.getByRole("button", { name: "This device", exact: true });

    await expect.element(settings).toHaveFocus();
    await browser.click();
    await expect.element(browser).toHaveAttribute("aria-pressed", "true");
    await userEvent.keyboard("{Escape}");

    await expect.element(browser).toHaveAttribute("aria-pressed", "false");
    // The close would render in the same commit that cleared the selection.
    expect(settings.element()).toHaveAttribute("data-open");

    await userEvent.keyboard("{Escape}");
    await expect.element(settings).not.toBeInTheDocument();
  });

  it("clears the selection on Esc from the details before Settings closes", async () => {
    await using _screen = await renderSettingsMap();
    const settings = page.getByRole("dialog", { name: "Settings" });
    const browser = page.getByRole("button", { name: "This device", exact: true });
    const details = page.getByRole("group", { name: "Details" });

    await expect.element(settings).toHaveFocus();
    await browser.click();
    await expect.element(browser).toHaveAttribute("aria-pressed", "true");
    // Choosing a machine or link inside the details leaves focus on the panel.
    (details.element() as HTMLElement).focus();
    await userEvent.keyboard("{Escape}");

    await expect.element(browser).toHaveAttribute("aria-pressed", "false");
    expect(settings.element()).toHaveAttribute("data-open");

    await userEvent.keyboard("{Escape}");
    await expect.element(settings).not.toBeInTheDocument();
  });
});
