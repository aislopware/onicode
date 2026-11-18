import type { BrowserSurfaceRect } from "./browserSurfaceStore";

export interface HostedBrowserWebviewSize {
  readonly width: number;
  readonly height: number;
}

export interface HostedBrowserWebviewWrapperStyle {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  readonly height: number;
  readonly zIndex: number;
  readonly pointerEvents: "auto" | "none";
  readonly borderRadius?: number;
  readonly visibility?: "hidden" | "visible";
  readonly clipPath?: string;
  readonly opacity?: number;
}

const HIDDEN_BROWSER_WEBVIEW_OFFSET = -100_000;
/** Above every layer of the app, so nothing covers the pixel. */
const DRAWN_SLIVER_Z_INDEX = 2_147_483_647;

export function resolveHostedBrowserWebviewWrapperStyle(input: {
  readonly active: boolean;
  readonly renderingActive: boolean;
  readonly keepPaintableWhenInactive?: boolean;
  readonly cornerRadius?: number;
  readonly zIndex?: number;
  readonly rect: BrowserSurfaceRect | null;
  readonly hiddenSize: HostedBrowserWebviewSize;
}): HostedBrowserWebviewWrapperStyle {
  const {
    active,
    cornerRadius = 0,
    hiddenSize,
    keepPaintableWhenInactive = false,
    rect,
    renderingActive,
    zIndex = 30,
  } = input;
  if (active && rect) {
    return {
      left: rect.x,
      top: rect.y,
      width: rect.width,
      height: rect.height,
      zIndex,
      pointerEvents: "auto",
      ...(cornerRadius > 0 ? { borderRadius: cornerRadius } : {}),
    };
  }

  if (renderingActive) {
    // Chromium slows a guest to one frame a second once nobody draws its frames, and a guest
    // behind the app is not drawn. One pixel of it, too faint to see, is.
    return {
      left: 0,
      top: 0,
      width: hiddenSize.width,
      height: hiddenSize.height,
      zIndex: DRAWN_SLIVER_Z_INDEX,
      pointerEvents: "none",
      visibility: "visible",
      clipPath: "inset(0 calc(100% - 1px) calc(100% - 1px) 0)",
      opacity: 0.01,
    };
  }

  return {
    left: HIDDEN_BROWSER_WEBVIEW_OFFSET,
    top: HIDDEN_BROWSER_WEBVIEW_OFFSET,
    width: hiddenSize.width,
    height: hiddenSize.height,
    zIndex: -1,
    pointerEvents: "none",
    visibility: keepPaintableWhenInactive ? "visible" : "hidden",
  };
}
