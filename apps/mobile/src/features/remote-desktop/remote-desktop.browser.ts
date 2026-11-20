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
/** The modifiers that stay held after they are used, until the toolbar lets go of them. */
let locked: ReadonlyArray<string> = [];

// oxlint-disable-next-line unicorn/require-post-message-target-origin -- The native WebView bridge takes one string.
const post = (message: object) => window.ReactNativeWebView.postMessage(JSON.stringify(message));

/** Lets go of the modifiers that were held for one use, and tells the toolbar. */
function releaseModifiers(current: Session) {
  modifiers = locked;
  current.viewer?.setModifiers(locked);
  post({ type: "modifiersUsed" });
}

export function stop() {
  const current = session;
  session = null;
  modifiers = [];
  locked = [];
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
    display: configuration.display,
    startReadable: true,
    ...(configuration.view ? { view: configuration.view } : {}),
    onView: (view) => {
      if (session === current) post({ type: "view", view });
    },
    onZoomed: (zoomed) => {
      if (session === current) post({ type: "zoomed", zoomed });
    },
    onLongPress: () => {
      if (session === current) post({ type: "longPress" });
    },
    onModifiersUsed: () => {
      if (session === current) releaseModifiers(current);
    },
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
      releaseModifiers(current);
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
  modifiers = locked;
  session?.viewer?.setModifiers(locked);
  session?.viewer?.chord(codes);
}

/** Sticky modifiers the next typed character, click or drag is sent with; `held` ones stay after it. */
export function setModifiers(codes: ReadonlyArray<string>, held: ReadonlyArray<string> = []) {
  modifiers = codes;
  locked = held;
  session?.viewer?.setModifiers(codes);
}

/** How much of the picture's bottom the keyboard and its toolbar cover. */
export function setBottomInset(inset: number, durationMs: number) {
  session?.viewer?.setBottomInset(inset, durationMs);
}

export function toggleZoom() {
  session?.viewer?.toggleZoom();
}

export function setTouchMode(mode: RemoteDesktopTouchMode) {
  session?.viewer?.setTouchMode(mode);
}

/** Puts this device's clipboard text on the host's clipboard and pastes it there. */
export function paste(text: string) {
  session?.viewer?.sendClipboard(text, true);
}

/** Types a change made in the native keyboard field Android uses in place of the viewer's own. */
export function typeEdit(backspaces: number, lines: ReadonlyArray<string>) {
  session?.viewer?.typeEdit({ backspaces, lines });
}

export function focusKeyboard() {
  session?.viewer?.focusKeyboard();
}

export function blurKeyboard() {
  session?.viewer?.blurKeyboard();
}
