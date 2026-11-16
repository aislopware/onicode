/// <reference lib="dom" />
// The DOM lib types the function this scenario runs in the page.
import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

const appearance = () => ({
  dark: document.documentElement.classList.contains("dark"),
  theme: document.documentElement.dataset.themeId ?? null,
});

// Onicode ships one theme, so the scenario saves a custom one to switch to.
const saveCustomTheme = () => {
  window.localStorage.setItem(
    "onicode:themes:v1",
    JSON.stringify([
      {
        id: "nightfall",
        label: "Nightfall",
        appearance: "dark",
        colors: { canvas: "#1a1b26", accent: "#7aa2f7" },
        variants: { light: { canvas: "#f4f5fb", accent: "#3d59a1" } },
      },
    ]),
  );
  window.dispatchEvent(new StorageEvent("storage", { key: "onicode:themes:v1" }));
  return null;
};

test(
  "The color scheme and theme apply at once, reach the native shell, and survive a reload",
  WEB,
  async ({ onicode, desktop, app, browser, screen }) => {
    await onicode.project("demo");
    await onicode.open("/settings/appearance");
    const dark = screen.getByRole("button", "Use dark mode");
    const light = screen.getByRole("button", "Use light mode");

    await dark.tap();
    await expect(dark).toHaveAttribute("aria-pressed", "true");
    await expect.poll(() => browser.evaluate(appearance)).toEqual({ dark: true, theme: null });
    await expect.poll(async () => (await desktop.window()).themeSource).toBe("dark");

    await browser.evaluate(saveCustomTheme);
    await screen.getByRole("button", "Use Nightfall theme").tap();
    await expect
      .poll(() => browser.evaluate(appearance))
      .toEqual({ dark: true, theme: "nightfall" });
    await app.screenshot("nightfall-dark");

    await onicode.open("/settings/appearance");
    await expect(dark).toHaveAttribute("aria-pressed", "true");
    expect(await browser.evaluate(appearance)).toEqual({ dark: true, theme: "nightfall" });

    await screen.getByRole("button", "Use Onicode theme").tap();
    await light.tap();
    await expect.poll(() => browser.evaluate(appearance)).toEqual({ dark: false, theme: null });
    await expect.poll(async () => (await desktop.window()).themeSource).toBe("light");

    const system = screen.getByRole("button", "Follow the system appearance");
    await system.tap();
    await expect(system).toHaveAttribute("aria-pressed", "true");
    await expect.poll(async () => (await desktop.window()).themeSource).toBe("system");
  },
);
