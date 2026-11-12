// @effect-diagnostics globalTimers:off -- Browser-side viewer driven by DOM events, with no Effect runtime to schedule on.
import type { RemoteDesktopInputEvent } from "@onicode/contracts";

import {
  iceGatheringComplete,
  REMOTE_DESKTOP_CLIPBOARD_LIMIT,
  REMOTE_DESKTOP_CONTROL_CHANNEL,
  REMOTE_DESKTOP_INPUT_CHANNEL,
  type RemoteDesktopHostMessage,
  type RemoteDesktopViewerMessage,
} from "./protocol.ts";

/** "reconnecting" follows a picture that was streaming and lost its network path. */
export type RemoteDesktopViewerStatus = "connecting" | "streaming" | "reconnecting" | "error";

/**
 * How fingers drive the host. "trackpad" moves its pointer like a laptop trackpad and taps click
 * wherever the pointer is; "touch" clicks under the finger.
 */
export type RemoteDesktopTouchMode = "trackpad" | "touch";

export interface RemoteDesktopViewerStats {
  readonly width: number;
  readonly height: number;
  readonly framesPerSecond: number;
  readonly kilobitsPerSecond: number;
  /** Round trip to the host in milliseconds, null until the first measurement. */
  readonly roundTripMs: number | null;
  readonly codec: string | null;
  /** How the two peers reach each other: "host" is direct, "relay" would be a TURN server. */
  readonly route: string | null;
}

export interface RemoteDesktopViewerOptions {
  /** The viewer fills this element with the host's picture. */
  readonly container: HTMLElement;
  /** Trades this viewer's offer for the host's answer. */
  readonly signal: (offer: string) => Promise<string>;
  /** Whether to send input at all; false shows the picture only. */
  readonly control: boolean;
  readonly onStatus: (status: RemoteDesktopViewerStatus, detail?: string) => void;
  /** Called while the viewer holds keyboard focus, so a host app can route shortcuts to it. */
  readonly onKeyboardFocus?: (focused: boolean) => void;
  /** Called about once a second while set. */
  readonly onStats?: (stats: RemoteDesktopViewerStats) => void;
  /** Sees soft-keyboard text before it is typed; true means it was sent some other way, such as a chord. */
  readonly onText?: (text: string) => boolean;
  /** Defaults to "trackpad". A mouse always points directly. */
  readonly touchMode?: RemoteDesktopTouchMode;
  /** The host's clipboard changed; the client copies the text to its own. */
  readonly onClipboard?: (text: string) => void;
}

export interface RemoteDesktopViewer {
  readonly stop: () => void;
  /** Presses the keys in order and releases them in reverse, as one chord. */
  readonly chord: (codes: ReadonlyArray<string>) => void;
  /** Opens the soft keyboard on touch devices. */
  readonly focusKeyboard: () => void;
  readonly blurKeyboard: () => void;
  readonly setTouchMode: (mode: RemoteDesktopTouchMode) => void;
  /** Puts text on the host's clipboard; `paste` also pastes it there. */
  readonly sendClipboard: (text: string, paste?: boolean) => void;
}

const TAP_SLOP_PX = 10;
const TAP_MAX_MS = 300;
const DOUBLE_TAP_MS = 350;
const DOUBLE_TAP_SLOP_PX = 32;
const LONG_PRESS_MS = 500;
const PINCH_SLOP_PX = 24;
const MAX_ZOOM = 8;
const DISCONNECT_GRACE_MS = 4_000;
// A dropped path is retried for this long before the viewer gives up and shows the error.
const RECONNECT_WINDOW_MS = 30_000;
const RECONNECT_DELAYS_MS = [500, 1_000, 2_000, 4_000];
const CURSOR_MARGIN_PX = 48;
const HOST_ENDED = "The host ended the session.";
const HOST_LOST = "The connection to the host was lost.";
const CURSOR_IMAGE =
  "url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 20 24'%3E%3Cpath d='M2 1v19l5-4.6 3.3 7.4 3-1.4-3.2-7.2H17z' fill='%23000' stroke='%23fff' stroke-width='1.6' stroke-linejoin='round'/%3E%3C/svg%3E\")";
// A zero-width space the soft keyboard can always delete, so Backspace reaches us on an empty field.
const KEYBOARD_SENTINEL = "​";
const KEYBOARD_BUFFER_LIMIT = 64;

const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value));

export function createRemoteDesktopViewer(
  options: RemoteDesktopViewerOptions,
): RemoteDesktopViewer {
  const { container } = options;
  const cleanups: Array<() => void> = [];
  const listen = <K extends keyof HTMLElementEventMap>(
    target: HTMLElement,
    type: K,
    handler: (event: HTMLElementEventMap[K]) => void,
    listenerOptions?: AddEventListenerOptions,
  ) => {
    target.addEventListener(type, handler, listenerOptions);
    cleanups.push(() => target.removeEventListener(type, handler, listenerOptions));
  };

  const surface = document.createElement("div");
  surface.tabIndex = 0;
  surface.style.cssText =
    "position:absolute;inset:0;overflow:hidden;background:#000;outline:none;touch-action:none;user-select:none;-webkit-user-select:none;";
  const video = document.createElement("video");
  video.autoplay = true;
  video.muted = true;
  video.playsInline = true;
  video.disablePictureInPicture = true;
  video.style.cssText =
    "position:absolute;inset:0;width:100%;height:100%;object-fit:contain;transform-origin:0 0;pointer-events:none;";
  surface.append(video);
  container.append(surface);
  cleanups.push(() => surface.remove());

  // Drawn here because a phone shows the host's own pointer only a few pixels tall.
  const pointer = document.createElement("div");
  pointer.style.cssText = `position:absolute;left:0;top:0;width:20px;height:24px;margin:-1px 0 0 -2px;background:${CURSOR_IMAGE} no-repeat;pointer-events:none;display:none;will-change:transform;`;
  surface.append(pointer);

  interface Link {
    readonly peer: RTCPeerConnection;
    readonly input: RTCDataChannel;
    readonly control: RTCDataChannel;
  }
  let link: Link | null = null;
  let stopped = false;
  let streaming = false;
  let lastClipboard: string | null = null;

  const fail = (detail: string) => {
    if (stopped) return;
    streaming = false;
    options.onStatus("error", detail);
  };
  const send = (event: RemoteDesktopInputEvent) => {
    if (!options.control || link?.input.readyState !== "open") return;
    link.input.send(JSON.stringify(event));
  };
  const tell = (message: RemoteDesktopViewerMessage) => {
    if (link?.control.readyState === "open") link.control.send(JSON.stringify(message));
  };

  let zoom = 1;
  let panX = 0;
  let panY = 0;
  let touchMode = options.touchMode ?? "trackpad";
  /** Where the host's pointer is as far as this viewer knows, 0 to 1 across the display. */
  let cursor = { x: 0.5, y: 0.5 };
  let touched = false;
  /** The picture's rectangle inside the surface before zoom, letterboxed by object-fit. */
  const pictureRect = () => {
    const bounds = surface.getBoundingClientRect();
    const sourceWidth = video.videoWidth || bounds.width;
    const sourceHeight = video.videoHeight || bounds.height;
    const scale = Math.min(bounds.width / sourceWidth, bounds.height / sourceHeight) || 1;
    const width = sourceWidth * scale;
    const height = sourceHeight * scale;
    return {
      bounds,
      left: (bounds.width - width) / 2,
      top: (bounds.height - height) / 2,
      width,
      height,
    };
  };
  /** Where a client point lands on the host display, or null outside the picture. */
  const locate = (clientX: number, clientY: number, clampInside: boolean) => {
    const rect = pictureRect();
    const x = ((clientX - rect.bounds.left - panX) / zoom - rect.left) / rect.width;
    const y = ((clientY - rect.bounds.top - panY) / zoom - rect.top) / rect.height;
    if (!Number.isFinite(x) || !Number.isFinite(y)) return null;
    if (!clampInside && (x < 0 || x > 1 || y < 0 || y > 1)) return null;
    return { x: clamp(x, 0, 1), y: clamp(y, 0, 1) };
  };
  const applyTransform = () => {
    const { bounds } = pictureRect();
    zoom = clamp(zoom, 1, MAX_ZOOM);
    panX = clamp(panX, bounds.width * (1 - zoom), 0);
    panY = clamp(panY, bounds.height * (1 - zoom), 0);
    video.style.transform = zoom === 1 ? "" : `translate(${panX}px,${panY}px) scale(${zoom})`;
    placePointer();
  };
  const cursorOffset = () => {
    const rect = pictureRect();
    return {
      bounds: rect.bounds,
      x: panX + (rect.left + cursor.x * rect.width) * zoom,
      y: panY + (rect.top + cursor.y * rect.height) * zoom,
    };
  };
  const placePointer = () => {
    const shown = options.control && touched && touchMode === "trackpad";
    pointer.style.display = shown ? "block" : "none";
    if (!shown) return;
    const { x, y } = cursorOffset();
    pointer.style.transform = `translate(${x}px,${y}px)`;
  };
  /** Pans a zoomed picture so the pointer stays on screen. */
  const followCursor = () => {
    if (zoom === 1) return placePointer();
    const { bounds, x, y } = cursorOffset();
    const marginX = Math.min(CURSOR_MARGIN_PX, bounds.width / 4);
    const marginY = Math.min(CURSOR_MARGIN_PX, bounds.height / 4);
    panX += Math.max(0, marginX - x) - Math.max(0, x - (bounds.width - marginX));
    panY += Math.max(0, marginY - y) - Math.max(0, y - (bounds.height - marginY));
    applyTransform();
  };

  let viewportTimer: ReturnType<typeof setTimeout> | null = null;
  const reportViewport = () => {
    if (viewportTimer !== null) clearTimeout(viewportTimer);
    // Settle first: a resize drag or a pinch would otherwise reconfigure the encoder per frame.
    viewportTimer = setTimeout(() => {
      viewportTimer = null;
      const { bounds } = pictureRect();
      const density = (window.devicePixelRatio || 1) * zoom;
      tell({
        type: "viewport",
        width: Math.round(bounds.width * density),
        height: Math.round(bounds.height * density),
      });
    }, 200);
  };
  cleanups.push(() => {
    if (viewportTimer !== null) clearTimeout(viewportTimer);
  });
  const resizeObserver = new ResizeObserver(() => {
    applyTransform();
    reportViewport();
  });
  resizeObserver.observe(surface);
  cleanups.push(() => resizeObserver.disconnect());

  let pendingMove: { x: number; y: number } | null = null;
  let pendingScroll: { dx: number; dy: number } | null = null;
  let frame = 0;
  // One frame per burst of motion, and none while the pointer rests.
  const flush = () => {
    frame = 0;
    if (pendingMove) send({ type: "move", ...pendingMove });
    if (pendingScroll) send({ type: "scroll", ...pendingScroll });
    pendingMove = null;
    pendingScroll = null;
  };
  const schedule = () => {
    if (frame === 0) frame = requestAnimationFrame(flush);
  };
  const flushNow = () => {
    if (frame !== 0) cancelAnimationFrame(frame);
    flush();
  };
  cleanups.push(() => {
    if (frame !== 0) cancelAnimationFrame(frame);
  });
  const queueMove = (point: { x: number; y: number }) => {
    cursor = point;
    pendingMove = point;
    schedule();
  };
  const queueScroll = (dx: number, dy: number) => {
    pendingScroll = { dx: (pendingScroll?.dx ?? 0) + dx, dy: (pendingScroll?.dy ?? 0) + dy };
    schedule();
  };

  const mouseButtons = new Set<0 | 1 | 2>();
  const isMouse = (event: PointerEvent) => event.pointerType !== "touch";
  const mouseButton = (button: number) =>
    button === 0 || button === 1 || button === 2 ? button : null;

  listen(surface, "pointermove", (event) => {
    if (!isMouse(event)) return;
    const point = locate(event.clientX, event.clientY, mouseButtons.size > 0);
    if (point) queueMove(point);
  });
  listen(surface, "pointerdown", (event) => {
    if (!isMouse(event)) return;
    surface.focus({ preventScroll: true });
    const button = mouseButton(event.button);
    const point = locate(event.clientX, event.clientY, false);
    if (button === null || !point) return;
    event.preventDefault();
    surface.setPointerCapture(event.pointerId);
    mouseButtons.add(button);
    pendingMove = null;
    flushNow();
    send({ type: "button", button, down: true, ...point, clicks: clamp(event.detail || 1, 1, 3) });
  });
  const releaseMouse = (event: PointerEvent) => {
    if (!isMouse(event)) return;
    const button = mouseButton(event.button);
    if (button === null || !mouseButtons.delete(button)) return;
    const point = locate(event.clientX, event.clientY, true);
    if (!point) return;
    pendingMove = null;
    flushNow();
    send({ type: "button", button, down: false, ...point, clicks: clamp(event.detail || 1, 1, 3) });
  };
  listen(surface, "pointerup", releaseMouse);
  listen(surface, "pointercancel", releaseMouse);
  listen(surface, "contextmenu", (event) => event.preventDefault());
  listen(
    surface,
    "wheel",
    (event) => {
      event.preventDefault();
      // Lines and pages become pixels; the host scrolls in pixels.
      const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? surface.clientHeight : 1;
      queueScroll(event.deltaX * unit, event.deltaY * unit);
    },
    { passive: false },
  );

  const heldKeys = new Set<string>();
  const releaseKeys = () => {
    for (const code of heldKeys) send({ type: "key", code, down: false });
    heldKeys.clear();
  };
  listen(surface, "keydown", (event) => {
    if (!options.control || event.code === "") return;
    event.preventDefault();
    heldKeys.add(event.code);
    send({ type: "key", code: event.code, down: true });
  });
  listen(surface, "keyup", (event) => {
    if (!options.control || !heldKeys.delete(event.code)) return;
    event.preventDefault();
    send({ type: "key", code: event.code, down: false });
  });
  listen(surface, "focus", () => {
    // A view-only picture forwards no keys, so the host app keeps its own shortcuts.
    if (options.control) options.onKeyboardFocus?.(true);
  });
  listen(surface, "blur", () => {
    // macOS never delivers the key-up of a key released after focus left.
    releaseKeys();
    options.onKeyboardFocus?.(false);
  });
  cleanups.push(() => options.onKeyboardFocus?.(false));

  interface Touch {
    readonly startX: number;
    readonly startY: number;
    x: number;
    y: number;
  }
  const touches = new Map<number, Touch>();
  let gesture: "pending" | "move" | "drag" | "scroll" | "transform" | "done" = "done";
  let touchStartedAt = 0;
  let touchFingers = 0;
  let lastMoveAt = 0;
  let longPress: ReturnType<typeof setTimeout> | null = null;
  let lastTap: { at: number; x: number; y: number } | null = null;
  /** This touch began as the second tap of a double tap, so moving it drags. */
  let tapDrag = false;
  let pinch: { distance: number; zoom: number; centerX: number; centerY: number } | null = null;
  const clearLongPress = () => {
    if (longPress !== null) clearTimeout(longPress);
    longPress = null;
  };
  cleanups.push(clearLongPress);
  const press = (down: boolean, point: { x: number; y: number }) =>
    send({ type: "button", button: 0, down, ...point, clicks: 1 });
  const click = (button: 0 | 1 | 2, point: { x: number; y: number }, clicks: number) => {
    cursor = point;
    send({ type: "move", ...point });
    send({ type: "button", button, down: true, ...point, clicks });
    send({ type: "button", button, down: false, ...point, clicks });
  };
  const moved = (touch: Touch) =>
    Math.hypot(touch.x - touch.startX, touch.y - touch.startY) >= TAP_SLOP_PX;
  const centroid = () => {
    let x = 0;
    let y = 0;
    for (const touch of touches.values()) {
      x += touch.x;
      y += touch.y;
    }
    return { x: x / touches.size, y: y / touches.size };
  };
  const spread = () => {
    const [a, b] = [...touches.values()];
    return a && b ? Math.hypot(a.x - b.x, a.y - b.y) : 0;
  };

  listen(surface, "pointerdown", (event) => {
    if (isMouse(event)) return;
    event.preventDefault();
    surface.setPointerCapture(event.pointerId);
    touches.set(event.pointerId, {
      startX: event.clientX,
      startY: event.clientY,
      x: event.clientX,
      y: event.clientY,
    });
    if (!touched) {
      touched = true;
      placePointer();
    }
    touchFingers = Math.max(touchFingers, touches.size);
    if (touches.size === 1) {
      gesture = "pending";
      touchStartedAt = event.timeStamp;
      lastMoveAt = event.timeStamp;
      touchFingers = 1;
      const { clientX, clientY } = event;
      tapDrag =
        touchMode === "trackpad" &&
        lastTap !== null &&
        event.timeStamp - lastTap.at < DOUBLE_TAP_MS &&
        Math.hypot(clientX - lastTap.x, clientY - lastTap.y) < DOUBLE_TAP_SLOP_PX;
      longPress = setTimeout(() => {
        longPress = null;
        if (gesture !== "pending") return;
        if (touchMode === "trackpad") {
          // Holding still picks up what is under the pointer, ready to drag.
          gesture = "drag";
          flushNow();
          press(true, cursor);
          return;
        }
        gesture = "done";
        const point = locate(clientX, clientY, false);
        if (point) click(2, point, 1);
      }, LONG_PRESS_MS);
      return;
    }
    clearLongPress();
    if (gesture === "drag") {
      // Another finger ends the drag where it is, not wherever the gesture goes next.
      flushNow();
      press(false, cursor);
      gesture = "done";
    }
    if (touches.size === 2) {
      gesture = "pending";
      const center = centroid();
      pinch = { distance: spread(), zoom, centerX: center.x, centerY: center.y };
    } else if (touches.size > 3 || gesture !== "pending") {
      gesture = "done";
    }
  });

  listen(surface, "pointermove", (event) => {
    const touch = touches.get(event.pointerId);
    if (isMouse(event) || !touch) return;
    const previous = centroid();
    const fromX = touch.x;
    const fromY = touch.y;
    touch.x = event.clientX;
    touch.y = event.clientY;
    if (touchFingers === 1) {
      if (gesture === "pending") {
        if (!moved(touch)) return;
        clearLongPress();
        if (touchMode === "trackpad") {
          gesture = tapDrag ? "drag" : "move";
          if (tapDrag) press(true, cursor);
        } else {
          const start = locate(touch.startX, touch.startY, false);
          if (!start) {
            gesture = "done";
            return;
          }
          gesture = "drag";
          cursor = start;
          send({ type: "move", ...start });
          press(true, start);
        }
      }
      if (touchMode === "touch") {
        const point = gesture === "drag" ? locate(touch.x, touch.y, true) : null;
        if (point) queueMove(point);
      } else if (gesture === "move" || gesture === "drag") {
        const rect = pictureRect();
        const dx = touch.x - fromX;
        const dy = touch.y - fromY;
        const elapsed = Math.max(1, event.timeStamp - lastMoveAt);
        lastMoveAt = event.timeStamp;
        // A slow finger places the pointer exactly; a fast one crosses the display in one stroke.
        const gain = clamp(0.6 + (Math.hypot(dx, dy) / elapsed) * 1.4, 1, 3);
        queueMove({
          x: clamp(cursor.x + (dx * gain) / (rect.width * zoom), 0, 1),
          y: clamp(cursor.y + (dy * gain) / (rect.height * zoom), 0, 1),
        });
        followCursor();
      }
      return;
    }
    if (touchFingers > 2) {
      // Three fingers only tap.
      if (moved(touch)) gesture = "done";
      return;
    }
    if (touches.size !== 2 || !pinch) return;
    const center = centroid();
    const distance = spread();
    if (gesture === "pending") {
      const pinched = Math.abs(distance - pinch.distance) > PINCH_SLOP_PX;
      const panned = Math.hypot(center.x - pinch.centerX, center.y - pinch.centerY) > TAP_SLOP_PX;
      if (!pinched && !panned) return;
      // Two fingers moving together scroll the host. Under direct touch a zoomed picture has no
      // other way to move, so there they pan it instead.
      gesture = pinched || (touchMode === "touch" && zoom > 1) ? "transform" : "scroll";
      if (gesture === "scroll" && touchMode === "touch") {
        const point = locate(pinch.centerX, pinch.centerY, false);
        if (point) queueMove(point);
      }
    }
    if (gesture === "scroll") {
      queueScroll(previous.x - center.x, previous.y - center.y);
    } else if (gesture === "transform") {
      const { bounds } = pictureRect();
      const nextZoom = clamp(pinch.zoom * (distance / (pinch.distance || 1)), 1, MAX_ZOOM);
      // Keep the picture point under the fingers under the fingers.
      const anchorX = (previous.x - bounds.left - panX) / zoom;
      const anchorY = (previous.y - bounds.top - panY) / zoom;
      zoom = nextZoom;
      panX = center.x - bounds.left - anchorX * zoom;
      panY = center.y - bounds.top - anchorY * zoom;
      applyTransform();
    }
  });

  const endTouch = (event: PointerEvent) => {
    const touch = touches.get(event.pointerId);
    if (isMouse(event) || !touch) return;
    touches.delete(event.pointerId);
    clearLongPress();
    if (touches.size > 0) return;
    if (gesture === "drag") {
      flushNow();
      press(false, cursor);
    }
    const quick = event.timeStamp - touchStartedAt < TAP_MAX_MS;
    const point = touchMode === "trackpad" ? cursor : locate(touch.startX, touch.startY, false);
    if (gesture === "pending" && quick && event.type === "pointerup" && point) {
      if (touchFingers === 1) {
        const double =
          lastTap !== null &&
          event.timeStamp - lastTap.at < DOUBLE_TAP_MS &&
          Math.hypot(touch.startX - lastTap.x, touch.startY - lastTap.y) < DOUBLE_TAP_SLOP_PX;
        click(0, point, double ? 2 : 1);
        lastTap = double ? null : { at: event.timeStamp, x: touch.startX, y: touch.startY };
      } else {
        // Two fingers are the secondary button and three the middle one, as on a trackpad.
        click(touchFingers === 2 ? 2 : 1, point, 1);
      }
    }
    if (gesture === "transform") reportViewport();
    gesture = "done";
    pinch = null;
    tapDrag = false;
    touchFingers = 0;
  };
  listen(surface, "pointerup", endTouch);
  listen(surface, "pointercancel", endTouch);

  const keyboard = document.createElement("textarea");
  keyboard.autocapitalize = "off";
  keyboard.autocomplete = "off";
  keyboard.spellcheck = false;
  keyboard.setAttribute("autocorrect", "off");
  keyboard.setAttribute("aria-label", "Remote keyboard");
  // On screen and focusable, or iOS refuses to raise the keyboard; 16px stops its focus zoom.
  keyboard.style.cssText =
    "position:absolute;left:0;bottom:0;width:1px;height:1px;opacity:0;border:0;padding:0;font-size:16px;resize:none;";
  keyboard.value = KEYBOARD_SENTINEL;
  let typed = KEYBOARD_SENTINEL;
  surface.append(keyboard);
  const resetKeyboard = () => {
    typed = KEYBOARD_SENTINEL;
    keyboard.value = typed;
  };
  const tap = (code: string) => {
    send({ type: "key", code, down: true });
    send({ type: "key", code, down: false });
  };
  const typeText = (text: string) => {
    if (!options.onText?.(text)) send({ type: "text", text });
  };
  // Keyboards rewrite what they already typed (autocorrect, Telex, IME), so send the difference
  // between the field before and after instead of trusting each event's own description.
  listen(keyboard, "input", () => {
    const next = keyboard.value;
    let shared = 0;
    while (shared < typed.length && shared < next.length && typed[shared] === next[shared]) {
      shared += 1;
    }
    for (const _removed of typed.slice(shared)) tap("Backspace");
    const [first, ...lines] = next.slice(shared).split("\n");
    if (first) typeText(first);
    for (const line of lines) {
      tap("Enter");
      if (line) typeText(line);
    }
    typed = next;
    if (
      lines.length > 0 ||
      !next.startsWith(KEYBOARD_SENTINEL) ||
      next.length > KEYBOARD_BUFFER_LIMIT
    ) {
      resetKeyboard();
    }
  });
  // Stop the surface's own key handling from seeing keys that produce text here.
  listen(keyboard, "keydown", (event) => event.stopPropagation());
  listen(keyboard, "keyup", (event) => event.stopPropagation());

  let disconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  /** When the picture was lost, while it is being recovered; null otherwise. */
  let lostAt: number | null = null;
  let attempts = 0;
  const clearDisconnectTimer = () => {
    if (disconnectTimer !== null) clearTimeout(disconnectTimer);
    disconnectTimer = null;
  };
  cleanups.push(clearDisconnectTimer, () => {
    if (reconnectTimer !== null) clearTimeout(reconnectTimer);
  });

  const open = () => {
    const peer = new RTCPeerConnection({ bundlePolicy: "max-bundle" });
    peer.addTransceiver("video", { direction: "recvonly" });
    const current: Link = {
      peer,
      input: peer.createDataChannel(REMOTE_DESKTOP_INPUT_CHANNEL),
      control: peer.createDataChannel(REMOTE_DESKTOP_CONTROL_CHANNEL),
    };
    link = current;
    /** Ends this link. A lost path is tried again; a host that hung up is not. */
    const drop = (detail: string, retry: boolean) => {
      if (stopped || link !== current) return;
      link = null;
      clearDisconnectTimer();
      peer.close();
      const wasStreaming = streaming;
      streaming = false;
      if (wasStreaming) lostAt = performance.now();
      if (!retry || lostAt === null || performance.now() - lostAt > RECONNECT_WINDOW_MS) {
        lostAt = null;
        return fail(detail);
      }
      options.onStatus("reconnecting");
      const delay = RECONNECT_DELAYS_MS[Math.min(attempts, RECONNECT_DELAYS_MS.length - 1)];
      attempts += 1;
      reconnectTimer = setTimeout(open, delay);
    };
    current.control.addEventListener("open", () => {
      if (link === current) reportViewport();
    });
    current.control.addEventListener("message", (event) => {
      if (link !== current || !options.control) return;
      let message: RemoteDesktopHostMessage;
      try {
        message = JSON.parse(String(event.data)) as RemoteDesktopHostMessage;
      } catch {
        return;
      }
      if (message.type !== "clipboard" || typeof message.text !== "string") return;
      lastClipboard = message.text;
      options.onClipboard?.(message.text);
    });
    // The host closes the channel when it hangs up; a lost path leaves it open and silent.
    current.input.addEventListener("close", () => drop(HOST_ENDED, false));
    peer.addEventListener("connectionstatechange", () => {
      if (link !== current) return;
      clearDisconnectTimer();
      const state = peer.connectionState;
      if (state === "failed") {
        drop(
          streaming || lostAt !== null
            ? HOST_LOST
            : "Could not reach the host directly. Both devices need the same network or tailnet.",
          true,
        );
      } else if (state === "disconnected") {
        // A network change often recovers by itself within a couple of seconds.
        disconnectTimer = setTimeout(() => drop(HOST_LOST, true), DISCONNECT_GRACE_MS);
      }
    });
    peer.addEventListener("track", (event) => {
      if (link !== current) return;
      // Show each frame as it arrives; a jitter buffer only adds delay to a screen.
      event.receiver.jitterBufferTarget = 0;
      video.srcObject = event.streams[0] ?? new MediaStream([event.track]);
      void video.play().catch(() => undefined);
    });
    void (async () => {
      try {
        await peer.setLocalDescription(await peer.createOffer());
        await iceGatheringComplete(peer);
        const offer = peer.localDescription?.sdp;
        if (!offer) throw new Error("Could not describe this device to the host.");
        const answer = await options.signal(offer);
        if (link !== current) return;
        await peer.setRemoteDescription({ type: "answer", sdp: answer });
      } catch (error) {
        drop(error instanceof Error ? error.message : "Could not connect to the host.", true);
      }
    })();
  };

  listen(video, "resize", () => {
    applyTransform();
    reportViewport();
  });
  listen(video, "playing", () => {
    if (stopped || streaming || link === null) return;
    streaming = true;
    lostAt = null;
    attempts = 0;
    options.onStatus("streaming");
  });

  let statsTimer: ReturnType<typeof setInterval> | null = null;
  if (options.onStats) {
    let lastBytes = 0;
    let lastAt = 0;
    const report = options.onStats;
    statsTimer = setInterval(() => {
      if (!streaming || link === null) return;
      void link.peer.getStats().then((stats) => {
        let inbound: Record<string, unknown> | undefined;
        let pair: Record<string, unknown> | undefined;
        stats.forEach((entry: Record<string, unknown>) => {
          if (entry.type === "inbound-rtp" && entry.kind === "video") inbound = entry;
          if (entry.type === "candidate-pair" && entry.nominated === true) pair = entry;
        });
        if (!inbound) return;
        const bytes = Number(inbound.bytesReceived ?? 0);
        const at = Number(inbound.timestamp ?? 0);
        // A new link counts its bytes from zero again.
        const kilobitsPerSecond =
          lastAt > 0 && bytes >= lastBytes ? ((bytes - lastBytes) * 8) / (at - lastAt) : 0;
        lastBytes = bytes;
        lastAt = at;
        const codec = stats.get(String(inbound.codecId)) as { mimeType?: string } | undefined;
        const local = stats.get(String(pair?.localCandidateId)) as
          | { candidateType?: string }
          | undefined;
        report({
          width: video.videoWidth,
          height: video.videoHeight,
          framesPerSecond: Math.round(Number(inbound.framesPerSecond ?? 0)),
          kilobitsPerSecond: Math.round(kilobitsPerSecond),
          roundTripMs:
            typeof pair?.currentRoundTripTime === "number"
              ? Math.round(pair.currentRoundTripTime * 1000)
              : null,
          codec: codec?.mimeType?.replace("video/", "") ?? null,
          route: local?.candidateType ?? null,
        });
      });
    }, 1_000);
  }

  options.onStatus("connecting");
  open();
  return {
    stop() {
      if (stopped) return;
      stopped = true;
      if (statsTimer !== null) clearInterval(statsTimer);
      // The host releases whatever this viewer still holds when the connection closes.
      for (const cleanup of cleanups.splice(0)) cleanup();
      link?.peer.close();
      link = null;
    },
    chord(codes) {
      for (const code of codes) send({ type: "key", code, down: true });
      for (const code of [...codes].reverse()) send({ type: "key", code, down: false });
    },
    focusKeyboard() {
      resetKeyboard();
      keyboard.focus({ preventScroll: true });
    },
    blurKeyboard() {
      keyboard.blur();
    },
    setTouchMode(mode) {
      touchMode = mode;
      placePointer();
    },
    sendClipboard(text, paste = false) {
      if (!options.control || text.length === 0 || text.length > REMOTE_DESKTOP_CLIPBOARD_LIMIT) {
        return;
      }
      // What the host just sent is already on its clipboard.
      if (!paste && text === lastClipboard) return;
      lastClipboard = text;
      tell({ type: "clipboard", text, ...(paste ? { paste } : {}) });
    },
  };
}
