import type { PlatformOSType } from "react-native";

/** The global the generated remote desktop script defines. */
export const REMOTE_DESKTOP_GLOBAL = "OniRemoteDesktop";

/** The contract's limit on a session description; the server refuses anything longer. */
const MAX_SESSION_DESCRIPTION_LENGTH = 64 * 1024;
const MAX_DETAIL_LENGTH = 1_000;
/** `REMOTE_DESKTOP_CLIPBOARD_LIMIT`, which the host never sends more than. */
const MAX_CLIPBOARD_LENGTH = 64_000;

export type RemoteDesktopTouchMode = "trackpad" | "touch";

export interface RemoteDesktopConfiguration {
  /** False shows the picture and sends no input. */
  readonly control: boolean;
  readonly touchMode: RemoteDesktopTouchMode;
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
  /** The sticky modifiers went out with a typed character, so the toolbar releases them. */
  | { readonly type: "modifiersUsed" };

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

export function remoteDesktopMessage(data: string): RemoteDesktopMessage | null {
  try {
    const message: unknown = JSON.parse(data);
    if (typeof message !== "object" || message === null || !("type" in message)) return null;
    if (message.type === "modifiersUsed") return { type: message.type };
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
