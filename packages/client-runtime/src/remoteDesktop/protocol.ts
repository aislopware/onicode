// @effect-diagnostics globalTimers:off -- Runs in the mobile WebView script and the desktop renderer, outside any Effect runtime.
// No schema imports here: this module is bundled into the mobile viewer's WebView script.

/** The data channel a viewer opens for input. Messages are JSON `RemoteDesktopInputEvent`s. */
export const REMOTE_DESKTOP_INPUT_CHANNEL = "onicode-input";
/** The data channel a viewer opens for `RemoteDesktopViewerMessage`s. */
export const REMOTE_DESKTOP_CONTROL_CHANNEL = "onicode-control";

/** Longest clipboard text either side sends; in UTF-8 it stays inside one data channel message. */
export const REMOTE_DESKTOP_CLIPBOARD_LIMIT = 64_000;

/** What a viewer tells the host besides input. */
export type RemoteDesktopViewerMessage =
  | {
      readonly type: "viewport";
      /** Physical pixels the viewer can show, so the host encodes no more than that. */
      readonly width: number;
      readonly height: number;
    }
  | {
      readonly type: "clipboard";
      readonly text: string;
      /** Also paste it into whatever has focus on the host. */
      readonly paste?: boolean;
    };

/** What the host tells a viewer, over the same channel. */
export interface RemoteDesktopHostMessage {
  readonly type: "clipboard";
  readonly text: string;
}

/**
 * Resolves once every ICE candidate is in the local description. Offers and answers travel
 * whole, in one request each, and with no STUN server gathering ends within milliseconds.
 */
export function iceGatheringComplete(connection: RTCPeerConnection, timeoutMs = 2_000) {
  if (connection.iceGatheringState === "complete") return Promise.resolve();
  return new Promise<void>((resolve) => {
    const done = () => {
      clearTimeout(timer);
      connection.removeEventListener("icegatheringstatechange", check);
      resolve();
    };
    const check = () => {
      if (connection.iceGatheringState === "complete") done();
    };
    // An interface that never answers must not hold the handshake; the candidates so far serve.
    const timer = setTimeout(done, timeoutMs);
    connection.addEventListener("icegatheringstatechange", check);
  });
}
