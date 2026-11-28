import { describe, expect, it } from "vite-plus/test";

import { resolveHostedBrowserWebviewWrapperStyle } from "./hostedBrowserWebviewStyle";

const hiddenSize = { width: 1280, height: 800 };

describe("resolveHostedBrowserWebviewWrapperStyle", () => {
  it("keeps a capture-active guest drawn but unseen behind translucent themes", () => {
    const style = resolveHostedBrowserWebviewWrapperStyle({
      active: false,
      renderingActive: true,
      rect: null,
      hiddenSize,
    });
    // One faint pixel above the app stays drawn; the rest of the guest is clipped away.
    expect(style).toMatchObject({
      left: 0,
      top: 0,
      visibility: "visible",
      clipPath: "inset(0 calc(100% - 1px) calc(100% - 1px) 0)",
    });
    expect(style.opacity).toBeGreaterThan(0);
    expect(style.opacity).toBeLessThan(0.05);
  });
});
