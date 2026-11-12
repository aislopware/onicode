import {
  createRemoteDesktopViewer,
  type RemoteDesktopViewer,
} from "@onicode/client-runtime/remote-desktop/viewer";

import type { RemoteDesktopConfiguration, RemoteDesktopTouchMode } from "./remote-desktop-document";

declare global {
  interface Window {
    ReactNativeWebView: { postMessage: (message: string) => void };
  }
}

// Unshifted US keys; the host's layout decides what a chord with them means.
const SYMBOL_CODES: Readonly<Record<string, string>> = {
  " ": "Space",
  "-": "Minus",
  "=": "Equal",
  "[": "BracketLeft",
  "]": "BracketRight",
  "\\": "Backslash",
  ";": "Semicolon",
  "'": "Quote",
  ",": "Comma",
  ".": "Period",
  "/": "Slash",
  "`": "Backquote",
};

function keyCode(character: string) {
  if (/^[a-z]$/i.test(character)) return `Key${character.toUpperCase()}`;
  if (/^\d$/.test(character)) return `Digit${character}`;
  return SYMBOL_CODES[character] ?? null;
}

interface Session {
  viewer: RemoteDesktopViewer | null;
  signal: { resolve: (sdp: string) => void; reject: (error: Error) => void } | null;
}

let session: Session | null = null;
let modifiers: ReadonlyArray<string> = [];

// oxlint-disable-next-line unicorn/require-post-message-target-origin -- The native WebView bridge takes one string.
const post = (message: object) => window.ReactNativeWebView.postMessage(JSON.stringify(message));

export function stop() {
  const current = session;
  session = null;
  modifiers = [];
  current?.signal?.reject(new Error("The viewer stopped."));
  current?.viewer?.stop();
}

export function start(configuration: RemoteDesktopConfiguration) {
  stop();
  if (typeof RTCPeerConnection === "undefined") {
    post({
      type: "status",
      status: "error",
      detail: "This device cannot receive the remote desktop stream.",
    });
    return;
  }
  // Set before the viewer exists: it reports its first status while it is being created.
  const current: Session = { viewer: null, signal: null };
  session = current;
  current.viewer = createRemoteDesktopViewer({
    container: document.body,
    control: configuration.control,
    touchMode: configuration.touchMode,
    onClipboard: (text) => {
      if (session === current) post({ type: "clipboard", text });
    },
    signal: (offer) =>
      new Promise<string>((resolve, reject) => {
        if (session !== current) return reject(new Error("The viewer stopped."));
        current.signal?.reject(new Error("A newer offer replaced this one."));
        current.signal = { resolve, reject };
        post({ type: "offer", sdp: offer });
      }),
    onStatus: (status, detail) => {
      if (session === current) post({ type: "status", status, detail });
    },
    onText: (text) => {
      if (session !== current || modifiers.length === 0) return false;
      const code = [...text].length === 1 ? keyCode(text) : null;
      const held = modifiers;
      modifiers = [];
      post({ type: "modifiersUsed" });
      if (code === null) return false;
      current.viewer?.chord([...held, code]);
      return true;
    },
  });
  window.addEventListener("pagehide", stop, { once: true });
}

/** The host's answer to the offer this viewer posted. */
export function answer(sdp: string) {
  const signal = session?.signal;
  if (session) session.signal = null;
  signal?.resolve(sdp);
}

/** Why the offer could not be answered, in words the viewer shows. */
export function fail(message: string) {
  const signal = session?.signal;
  if (session) session.signal = null;
  signal?.reject(new Error(message));
}

export function chord(codes: ReadonlyArray<string>) {
  modifiers = [];
  session?.viewer?.chord(codes);
}

/** Sticky modifiers the next typed character is sent with, as a chord. */
export function setModifiers(codes: ReadonlyArray<string>) {
  modifiers = codes;
}

export function setTouchMode(mode: RemoteDesktopTouchMode) {
  session?.viewer?.setTouchMode(mode);
}

/** Puts this device's clipboard text on the host's clipboard and pastes it there. */
export function paste(text: string) {
  session?.viewer?.sendClipboard(text, true);
}

export function focusKeyboard() {
  session?.viewer?.focusKeyboard();
}

export function blurKeyboard() {
  session?.viewer?.blurKeyboard();
}
