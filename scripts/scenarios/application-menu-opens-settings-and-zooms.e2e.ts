/// <reference lib="dom" />
// The DOM lib types the function this scenario runs in the page.
import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

declare global {
  interface Window {
    desktopBridge?: { readonly getWindowZoomFactor: () => Promise<number> };
  }
}

test(
  "The application menu opens Settings and zooms the window",
  WEB,
  async ({ onicode, desktop, app, browser, screen }) => {
    await onicode.project("demo");
    await onicode.open();
    const menus = (await desktop.applicationMenu()).map((menu) => menu.label);
    expect(menus).toEqual(expect.arrayContaining(["File", "Edit", "View", "Window", "Help"]));

    // macOS keeps Settings in the menu named after the app, which comes first; elsewhere it is in File.
    await desktop.chooseApplicationMenuItem(menus[0] ?? "File", "Settings...");
    const settings = screen.getByRole("dialog", "Settings");
    await expect(settings).toBeVisible();
    await settings.getByRole("region", "Settings content").getByRole("button", "Close").tap();

    const zoom = async () => (await desktop.window()).zoomFactor;
    const rendererZoom = () => browser.evaluate(() => window.desktopBridge!.getWindowZoomFactor());
    expect(await zoom()).toBe(1);
    await desktop.chooseApplicationMenuItem("View", "Zoom In");
    await expect.poll(zoom).toBeGreaterThan(1);
    expect(await rendererZoom()).toBeGreaterThan(1);
    await app.screenshot("zoomed-in");
    await desktop.chooseApplicationMenuItem("View", "Zoom Out");
    await expect.poll(zoom).toBe(1);
    await desktop.chooseApplicationMenuItem("View", "Zoom In");
    await desktop.chooseApplicationMenuItem("View", "Actual Size");
    await expect.poll(zoom).toBe(1);
    expect(await rendererZoom()).toBe(1);
  },
);
