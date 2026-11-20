import type { PlatformOSType } from "react-native";

/** The global the generated remote desktop script defines. */
export const REMOTE_DESKTOP_GLOBAL = "OniRemoteDesktop";

/** The contract's limit on a session description; the server refuses anything longer. */
const MAX_SESSION_DESCRIPTION_LENGTH = 64 * 1024;
const MAX_DETAIL_LENGTH = 1_000;
/** `REMOTE_DESKTOP_CLIPBOARD_LIMIT`, which the host never sends more than. */
const MAX_CLIPBOARD_LENGTH = 64_000;

export type RemoteDesktopTouchMode = "trackpad" | "touch";

/** What part of the display the viewer shows and where its pointer is, each 0 to 1. */
export interface RemoteDesktopView {
  /** How wide the whole display is drawn, in points. */
  readonly width: number;
  readonly x: number;
  readonly y: number;
  readonly pointer: { readonly x: number; readonly y: number };
}

export interface RemoteDesktopConfiguration {
  /** False shows the picture and sends no input. */
  readonly control: boolean;
  readonly touchMode: RemoteDesktopTouchMode;
  /** The shared display in physical pixels, and how many of them make one of its points. */
  readonly display: {
    readonly width: number;
    readonly height: number;
    readonly scaleFactor: number;
  };
  /** Where an earlier viewer of this display left off. */
  readonly view?: RemoteDesktopView;
}

export type RemoteDesktopMessage =
  | { readonly type: "offer"; readonly sdp: string }
  | {
      readonly type: "status";
      readonly status: "connecting" | "streaming" | "reconnecting" | "error";
      readonly detail: string | undefined;
    }
  /** Text copied on the host, for this device's clipboard. */
  | { readonly type: "clipboard"; readonly text: string }
  /** The sticky modifiers went out with a key or a click, so the toolbar releases them. */
  | { readonly type: "modifiersUsed" }
  /** Whether the picture is zoomed past showing the whole display. */
  | { readonly type: "zoomed"; readonly zoomed: boolean }
  /** A long press took hold; the device confirms it with a tap of its own. */
  | { readonly type: "longPress" }
  /** The view settled somewhere new. */
  | { readonly type: "view"; readonly view: RemoteDesktopView };

/** The documents the device viewer already receives WebRTC in on each platform. */
export function remoteDesktopBaseUrl(platform: PlatformOSType) {
  return platform === "android" ? "https://localhost/" : "file:///";
}

export function remoteDesktopDocument(configuration: string, script: string) {
  const safeConfiguration = configuration.replace(/</g, "\\u003c");
  const safeScript = script.replace(/<\/script/gi, "<\\/script");
  const failure = `window.ReactNativeWebView.postMessage(JSON.stringify({type:"status",status:"error",detail:"Remote desktop viewer stopped unexpectedly."}));`;
  return `<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no"><style>html,body{margin:0;width:100%;height:100%;overflow:hidden;overscroll-behavior:none;touch-action:none;background:#000}body{position:fixed;inset:0}</style></head><body><script>window.addEventListener("error",function(){${failure}});window.addEventListener("unhandledrejection",function(){${failure}});\n${safeScript}\ntry{${REMOTE_DESKTOP_GLOBAL}.start(${safeConfiguration});}catch{${failure}}</script></body></html>`;
}

const isUnit = (value: unknown): value is number =>
  typeof value === "number" && value >= 0 && value <= 1;

function remoteDesktopView(value: unknown): RemoteDesktopView | null {
  if (typeof value !== "object" || value === null) return null;
  if (!("width" in value) || !("x" in value) || !("y" in value) || !("pointer" in value)) {
    return null;
  }
  const { width, x, y, pointer } = value;
  if (typeof width !== "number" || !Number.isFinite(width) || width <= 0) return null;
  if (!isUnit(x) || !isUnit(y) || typeof pointer !== "object" || pointer === null) return null;
  if (!("x" in pointer) || !("y" in pointer) || !isUnit(pointer.x) || !isUnit(pointer.y)) {
    return null;
  }
  return { width, x, y, pointer: { x: pointer.x, y: pointer.y } };
}

export function remoteDesktopMessage(data: string): RemoteDesktopMessage | null {
  try {
    const message: unknown = JSON.parse(data);
    if (typeof message !== "object" || message === null || !("type" in message)) return null;
    if (message.type === "modifiersUsed" || message.type === "longPress") {
      return { type: message.type };
    }
    if (message.type === "zoomed" && "zoomed" in message && typeof message.zoomed === "boolean") {
      return { type: message.type, zoomed: message.zoomed };
    }
    if (message.type === "view" && "view" in message) {
      const view = remoteDesktopView(message.view);
      return view === null ? null : { type: message.type, view };
    }
    if (
      message.type === "clipboard" &&
      "text" in message &&
      typeof message.text === "string" &&
      message.text.length > 0 &&
      message.text.length <= MAX_CLIPBOARD_LENGTH
    ) {
      return { type: message.type, text: message.text };
    }
    if (
      message.type === "offer" &&
      "sdp" in message &&
      typeof message.sdp === "string" &&
      message.sdp.length > 0 &&
      message.sdp.length <= MAX_SESSION_DESCRIPTION_LENGTH
    ) {
      return { type: message.type, sdp: message.sdp };
    }
    if (
      message.type === "status" &&
      "status" in message &&
      (message.status === "connecting" ||
        message.status === "streaming" ||
        message.status === "reconnecting" ||
        message.status === "error") &&
      (!("detail" in message) || typeof message.detail === "string")
    ) {
      return {
        type: message.type,
        status: message.status,
        detail:
          "detail" in message && typeof message.detail === "string" && message.detail.length > 0
            ? message.detail.slice(0, MAX_DETAIL_LENGTH)
            : undefined,
      };
    }
  } catch {}
  return null;
}
