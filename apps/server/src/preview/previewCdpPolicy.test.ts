import { expect, it } from "vite-plus/test";

import {
  cdpErrorMessage,
  driverTargetId,
  isPanelUrl,
  pageCommandParams,
  routePageCommand,
} from "./previewCdpPolicy.ts";

it.each([
  ["Runtime.evaluate", "forward"],
  ["Input.dispatchMouseEvent", "forward"],
  ["Page.navigate", "forward"],
  ["Network.getCookies", "forward"],
  // The panel already saves downloads and decides which tab is in front.
  ["Page.setDownloadBehavior", "acknowledge"],
  ["Page.bringToFront", "acknowledge"],
  ["Target.getTargets", "target"],
  ["Target.createTarget", "target"],
  ["Target.closeTarget", "target"],
  ["Target.getTargetInfo", "target"],
  // Everything below would reach past the thread's own tabs.
  ["Target.setAutoAttach", "refuse"],
  ["Target.attachToTarget", "refuse"],
  ["Browser.close", "refuse"],
  ["Storage.getCookies", "refuse"],
  ["Network.getAllCookies", "refuse"],
  ["SystemInfo.getProcessInfo", "refuse"],
  ["Page.close", "refuse"],
  ["NoDomain", "refuse"],
] as const)("routes %s to %s", (method, route) => {
  expect(routePageCommand(method)).toBe(route);
});

it("opens web pages and a driver's blank start page, nothing else", () => {
  expect(
    ["https://example.com/a", "http://localhost:3000", "about:blank"].filter(isPanelUrl),
  ).toHaveLength(3);
  expect(
    ["file:///etc/passwd", "chrome://settings", "javascript:alert(1)", "example.com", ""].filter(
      isPanelUrl,
    ),
  ).toEqual([]);
});

it("captures the page, never the window a tab sits in", () => {
  const clip = { x: 0, y: 0, width: 800, height: 600, scale: 1 };
  expect(
    pageCommandParams("Page.captureScreenshot", { format: "png", clip, fromSurface: false }),
  ).toEqual({ format: "png", clip });
  expect(pageCommandParams("Runtime.evaluate", { fromSurface: false })).toEqual({
    fromSurface: false,
  });
});

it("names a tab with an id obu accepts as a file name, the same every time", () => {
  const id = driverTargetId("thread-1", "tab_17");
  expect(id).toMatch(/^[0-9A-F]{32}$/);
  expect(driverTargetId("thread-1", "tab_17")).toBe(id);
  expect(driverTargetId("thread-2", "tab_17")).not.toBe(id);
});

it("keeps the browser's own words from a Playwright protocol error", () => {
  expect(
    cdpErrorMessage(
      new Error(
        "cdpSession.send: Protocol error (Runtime.callFunctionOn): Cannot find context with specified id\nCall log:\n  - x",
      ),
    ),
  ).toBe("Cannot find context with specified id");
  expect(cdpErrorMessage(new Error("A human controls this tab."))).toBe(
    "A human controls this tab.",
  );
});
