// @effect-diagnostics globalTimers:off -- Browser-side viewer driven by DOM events, with no Effect runtime to schedule on.
import type { RemoteDesktopInputEvent } from "@onicode/contracts";

import {
  iceGatheringComplete,
  REMOTE_DESKTOP_CLIPBOARD_LIMIT,
  REMOTE_DESKTOP_CONTROL_CHANNEL,
  REMOTE_DESKTOP_INPUT_CHANNEL,
  REMOTE_DESKTOP_KEYBOARD_SENTINEL,
  remoteDesktopKeyboardEdit,
  type RemoteDesktopHostMessage,
  type RemoteDesktopKeyboardEdit,
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

/** The shared display, so the picture can be laid out and scaled before its first frame arrives. */
export interface RemoteDesktopViewerDisplay {
  /** Physical pixels. */
  readonly width: number;
  readonly height: number;
  /** Physical pixels in one of the host's points. */
  readonly scaleFactor: number;
}

/** What part of the display a viewer shows, and where it left the pointer. */
export interface RemoteDesktopViewerView {
  /** How wide the whole display is drawn, in CSS pixels, so the scale survives a rotation. */
  readonly width: number;
  /** The display point at the middle of what is visible, 0 to 1. */
  readonly x: number;
  readonly y: number;
  readonly pointer: { readonly x: number; readonly y: number };
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
  readonly display?: RemoteDesktopViewerDisplay;
  /** Opens zoomed to where the host's text is readable, for a screen too small to show it all. */
  readonly startReadable?: boolean;
  /** Opens where an earlier viewer of this display left off, in place of `startReadable`. */
  readonly view?: RemoteDesktopViewerView;
  /** The view settled somewhere new, for the client to hand back as `view` next time. */
  readonly onView?: (view: RemoteDesktopViewerView) => void;
  /** Whether the picture is zoomed past showing the whole display. */
  readonly onZoomed?: (zoomed: boolean) => void;
  /** A long press took hold, which a touch device confirms with a tap of its own. */
  readonly onLongPress?: () => void;
  /** The modifiers from `setModifiers` went out with a click or a drag. */
  readonly onModifiersUsed?: () => void;
}

export interface RemoteDesktopViewer {
  readonly stop: () => void;
  /** Presses the keys in order and releases them in reverse, as one chord. */
  readonly chord: (codes: ReadonlyArray<string>) => void;
  /** Opens the soft keyboard on touch devices. */
  readonly focusKeyboard: () => void;
  /** Types a change made in a keyboard field the client keeps outside this viewer. */
  readonly typeEdit: (edit: Pick<RemoteDesktopKeyboardEdit, "backspaces" | "lines">) => void;
  readonly blurKeyboard: () => void;
  readonly setTouchMode: (mode: RemoteDesktopTouchMode) => void;
  /** Puts text on the host's clipboard; `paste` also pastes it there. */
  readonly sendClipboard: (text: string, paste?: boolean) => void;
  /** Modifier keys the next click or drag is made with. */
  readonly setModifiers: (codes: ReadonlyArray<string>) => void;
  /**
   * How much of the picture's bottom something covers, such as a soft keyboard. The picture keeps
   * its size and moves only as far as showing the pointer takes, over `durationMs`.
   */
  readonly setBottomInset: (inset: number, durationMs: number) => void;
  /** Switches between the whole display and the readable zoom around the pointer. */
  readonly toggleZoom: () => void;
}

const TAP_SLOP_PX = 10;
const TAP_MAX_MS = 300;
const DOUBLE_TAP_MS = 350;
const DOUBLE_TAP_SLOP_PX = 32;
const LONG_PRESS_MS = 500;
const PINCH_SLOP_PX = 24;
const MAX_ZOOM = 8;
// Device pixels per host point: the host's 13 point text at the 11 points iOS calls its smallest.
const READABLE_SCALE = 0.85;
// A fit this close to readable keeps the whole display instead of cropping a few percent of it.
const READABLE_ENOUGH = 0.8;
const VIEW_CHANGE_MS = 250;
// The curve Android moves its keyboard on, and close to the one iOS does.
const VIEW_CHANGE_EASING = "cubic-bezier(0.2,0,0,1)";
// How a released scroll or pan slows down; Chrome Remote Desktop's fling uses the same constant.
const GLIDE_TIME_CONSTANT_MS = 250;
const GLIDE_START_PX_PER_MS = 0.15;
const GLIDE_STOP_PX_PER_MS = 0.02;
// A finger that rested this long before lifting was stopping, not flicking.
const GLIDE_REST_MS = 80;
const GLIDE_SAMPLE_MS = 100;
const DISCONNECT_GRACE_MS = 4_000;
// A dropped path is retried for this long before the viewer gives up and shows the error.
const RECONNECT_WINDOW_MS = 30_000;
const RECONNECT_DELAYS_MS = [500, 1_000, 2_000, 4_000];
// How near an edge of what is visible the pointer gets before the picture moves with it.
const CURSOR_MARGIN = 0.2;
const CURSOR_MARGIN_MAX_PX = 96;
// A keyboard moves the picture only for a pointer it would cover, or all but cover.
const INSET_MARGIN_PX = 24;
// macOS draws its arrow about this many points tall; smaller than the pixels below, it is lost.
const HOST_POINTER_POINTS = 17;
const HOST_POINTER_MIN_PX = 12;
const POINTER_REST_MS = 400;
const POINTER_FADE = "opacity 120ms linear";
const LOUPE_SIZE_PX = 120;
const LOUPE_ZOOM = 1.6;
// Clear of the fingertip that covers what the loupe shows.
const LOUPE_GAP_PX = 28;
const HOST_ENDED = "The host ended the session.";
const HOST_LOST = "The connection to the host was lost.";
const CURSOR_IMAGE =
  "url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 20 24'%3E%3Cpath d='M2 1v19l5-4.6 3.3 7.4 3-1.4-3.2-7.2H17z' fill='%23000' stroke='%23fff' stroke-width='1.6' stroke-linejoin='round'/%3E%3C/svg%3E\")";

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
  pointer.style.cssText = `position:absolute;left:0;top:0;width:20px;height:24px;margin:-1px 0 0 -2px;background:${CURSOR_IMAGE} no-repeat;pointer-events:none;display:none;will-change:transform;transition:${POINTER_FADE};`;
  surface.append(pointer);

  // Shows what a finger covers while it holds or drags under direct touch.
  const loupe = document.createElement("div");
  loupe.style.cssText = `position:absolute;left:0;top:0;width:${LOUPE_SIZE_PX}px;height:${LOUPE_SIZE_PX}px;border-radius:50%;overflow:hidden;background:#000;box-shadow:0 0 0 1px rgba(255,255,255,.4),0 8px 24px rgba(0,0,0,.5);pointer-events:none;display:none;will-change:transform;`;
  const loupeVideo = document.createElement("video");
  loupeVideo.muted = true;
  loupeVideo.playsInline = true;
  loupeVideo.disablePictureInPicture = true;
  loupeVideo.style.cssText =
    "position:absolute;left:0;top:0;object-fit:contain;transform-origin:0 0;pointer-events:none;";
  const loupeMark = document.createElement("div");
  loupeMark.style.cssText =
    "position:absolute;left:50%;top:50%;width:6px;height:6px;margin:-3px 0 0 -3px;border-radius:50%;background:#fff;box-shadow:0 0 0 1px rgba(0,0,0,.6);";
  loupe.append(loupeVideo, loupeMark);
  surface.append(loupe);

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
  let insetBottom = 0;
  /** Where the picture stood before an inset moved it, and where the inset left it. */
  let lift: { from: number; to: number } | null = null;
  let touchMode = options.touchMode ?? "trackpad";
  let modifiers: ReadonlyArray<string> = [];
  /** Where the host's pointer is as far as this viewer knows, 0 to 1 across the display. */
  let cursor = { x: 0.5, y: 0.5 };
  let touched = false;
  let opened = false;
  /** The picture's rectangle inside the surface before zoom, letterboxed by object-fit. */
  const pictureRect = () => {
    const bounds = surface.getBoundingClientRect();
    const sourceWidth = video.videoWidth || options.display?.width || bounds.width;
    const sourceHeight = video.videoHeight || options.display?.height || bounds.height;
    const scale = Math.min(bounds.width / sourceWidth, bounds.height / sourceHeight) || 1;
    const width = sourceWidth * scale;
    const height = sourceHeight * scale;
    return {
      bounds,
      left: (bounds.width - width) / 2,
      top: (bounds.height - height) / 2,
      width,
      height,
      /** What nothing covers. */
      visibleHeight: Math.max(1, bounds.height - insetBottom),
    };
  };
  /** Host points across the display, or null when the host did not say. */
  const hostPoints = () =>
    options.display && options.display.scaleFactor > 0
      ? options.display.width / options.display.scaleFactor
      : null;
  const maxZoom = () => {
    const points = hostPoints();
    const fit = pictureRect().width;
    // Always far enough to double the host's points, however small the fit is.
    return points === null || fit <= 0 ? MAX_ZOOM : Math.max(MAX_ZOOM, (2 * points) / fit);
  };
  /** The zoom at which the host's text is readable, or 1 when the whole display already is. */
  const readableZoom = () => {
    const points = hostPoints();
    const fit = pictureRect().width;
    if (points === null || fit <= 0) return 1;
    const scale = fit / points;
    return scale >= READABLE_SCALE * READABLE_ENOUGH ? 1 : READABLE_SCALE / scale;
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

  let viewTimer: ReturnType<typeof setTimeout> | null = null;
  const reportView = () => {
    if (!options.onView || !opened) return;
    if (viewTimer !== null) clearTimeout(viewTimer);
    viewTimer = setTimeout(() => {
      viewTimer = null;
      if (laidOut === null) return;
      options.onView?.({ width: laidOut.fitWidth * zoom, ...laidOut.center, pointer: cursor });
    }, 300);
  };
  cleanups.push(() => {
    if (viewTimer !== null) clearTimeout(viewTimer);
  });
  let transitionTimer: ReturnType<typeof setTimeout> | null = null;
  const endTransition = () => {
    if (transitionTimer === null) return;
    clearTimeout(transitionTimer);
    transitionTimer = null;
    video.style.transition = "";
    pointer.style.transition = POINTER_FADE;
  };
  cleanups.push(endTransition);
  /** The picture's fit width and the display point at the middle of what is visible. */
  let laidOut: { fitWidth: number; center: { x: number; y: number } } | null = null;
  let zoomed = false;
  /** Shows the view. A finger moves it at once; a change the viewer makes eases over `durationMs`. */
  const applyTransform = (durationMs = 0) => {
    const rect = pictureRect();
    const { bounds, visibleHeight } = rect;
    zoom = clamp(zoom, 1, maxZoom());
    const width = rect.width * zoom;
    const height = rect.height * zoom;
    // On each axis the picture is centred while it fits, and otherwise covers what is visible.
    const maxX = Math.max(0, (bounds.width - width) / 2);
    const maxY = Math.max(0, (bounds.height - height) / 2);
    const left = clamp(panX + rect.left * zoom, Math.min(maxX, bounds.width - width), maxX);
    const top = clamp(panY + rect.top * zoom, Math.min(maxY, visibleHeight - height), maxY);
    panX = left - rect.left * zoom;
    panY = top - rect.top * zoom;
    endTransition();
    if (durationMs > 0 && !window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      const transition = `transform ${durationMs}ms ${VIEW_CHANGE_EASING}`;
      video.style.transition = transition;
      pointer.style.transition = `${transition},${POINTER_FADE}`;
      transitionTimer = setTimeout(endTransition, durationMs + 50);
    }
    video.style.transform =
      zoom === 1 && panX === 0 && panY === 0 ? "" : `translate(${panX}px,${panY}px) scale(${zoom})`;
    placePointer();
    if (rect.width > 0) {
      laidOut = {
        fitWidth: rect.width,
        center: {
          x: clamp((bounds.width / 2 - left) / width, 0, 1),
          y: clamp((visibleHeight / 2 - top) / height, 0, 1),
        },
      };
    }
    if (zoomed !== zoom > 1.01) {
      zoomed = !zoomed;
      options.onZoomed?.(zoomed);
    }
    reportView();
  };
  /** Puts a display point at the middle of what is visible, at this zoom. */
  const showAt = (nextZoom: number, point: { x: number; y: number }, durationMs = 0) => {
    zoom = clamp(nextZoom, 1, maxZoom());
    const rect = pictureRect();
    panX = rect.bounds.width / 2 - (rect.left + point.x * rect.width) * zoom;
    panY = rect.visibleHeight / 2 - (rect.top + point.y * rect.height) * zoom;
    applyTransform(durationMs);
  };
  const cursorOffset = () => {
    const rect = pictureRect();
    return {
      rect,
      x: panX + (rect.left + cursor.x * rect.width) * zoom,
      y: panY + (rect.top + cursor.y * rect.height) * zoom,
    };
  };
  /** Whether the host's own pointer, which the picture carries with its real shape, is big enough to see. */
  const hostPointerVisible = () => {
    const points = hostPoints();
    const shown = pictureRect().width * zoom;
    return points !== null && (shown / points) * HOST_POINTER_POINTS >= HOST_POINTER_MIN_PX;
  };
  let pointerRest: ReturnType<typeof setTimeout> | null = null;
  cleanups.push(() => {
    if (pointerRest !== null) clearTimeout(pointerRest);
  });
  /** The drawn pointer leads a moving finger without the stream's delay, then yields to the host's. */
  const wakePointer = () => {
    pointer.style.opacity = "1";
    if (pointerRest !== null) clearTimeout(pointerRest);
    pointerRest = setTimeout(() => {
      pointerRest = null;
      if (hostPointerVisible()) pointer.style.opacity = "0";
    }, POINTER_REST_MS);
  };
  const placePointer = () => {
    const shown = options.control && touched && touchMode === "trackpad";
    pointer.style.display = shown ? "block" : "none";
    if (!shown) return;
    if (!hostPointerVisible()) pointer.style.opacity = "1";
    const { x, y } = cursorOffset();
    pointer.style.transform = `translate(${x}px,${y}px)`;
  };
  const hideLoupe = () => {
    if (loupe.style.display === "none") return;
    loupe.style.display = "none";
    loupeVideo.srcObject = null;
  };
  const showLoupe = (clientX: number, clientY: number) => {
    const { bounds } = pictureRect();
    if (loupe.style.display === "none") {
      loupeVideo.srcObject = video.srcObject;
      loupeVideo.style.width = `${bounds.width}px`;
      loupeVideo.style.height = `${bounds.height}px`;
      void loupeVideo.play().catch(() => undefined);
      loupe.style.display = "block";
    }
    const x = clientX - bounds.left;
    const y = clientY - bounds.top;
    const scale = zoom * LOUPE_ZOOM;
    const middle = LOUPE_SIZE_PX / 2;
    loupeVideo.style.transform = `translate(${middle - ((x - panX) / zoom) * scale}px,${middle - ((y - panY) / zoom) * scale}px) scale(${scale})`;
    const above = y - LOUPE_GAP_PX - LOUPE_SIZE_PX;
    loupe.style.transform = `translate(${clamp(x - middle, 0, Math.max(0, bounds.width - LOUPE_SIZE_PX))}px,${above >= 0 ? above : y + LOUPE_GAP_PX}px)`;
  };
  cleanups.push(hideLoupe);
  /** Moves the picture so the pointer stays inside what is visible, `margin` from its edges. */
  const followCursor = (durationMs = 0, margin: number | null = null) => {
    const { rect, x, y } = cursorOffset();
    const edge = (size: number) => margin ?? Math.min(size * CURSOR_MARGIN, CURSOR_MARGIN_MAX_PX);
    const marginX = edge(rect.bounds.width);
    const marginY = edge(rect.visibleHeight);
    panX += Math.max(0, marginX - x) - Math.max(0, x - (rect.bounds.width - marginX));
    panY += Math.max(0, marginY - y) - Math.max(0, y - (rect.visibleHeight - marginY));
    applyTransform(durationMs);
  };
  /** Device pixels of finger travel to the host points that move its content as far on screen. */
  const scrollScale = () => {
    const points = hostPoints();
    const shown = pictureRect().width * zoom;
    return points === null || shown <= 0 ? 1 : points / shown;
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
  const layout = () => {
    const fitWidth = pictureRect().width;
    if (fitWidth <= 0) return;
    if (!opened) {
      opened = true;
      if (options.view) {
        cursor = options.view.pointer;
        showAt(options.view.width / fitWidth, options.view);
      } else if (options.startReadable) showAt(readableZoom(), cursor);
    } else if (laidOut !== null && zoom > 1 && Math.abs(fitWidth - laidOut.fitWidth) > 0.5) {
      // A rotation changes what fits; what was being read keeps its size and its place.
      showAt((zoom * laidOut.fitWidth) / fitWidth, laidOut.center);
    }
    applyTransform();
    reportViewport();
  };
  const resizeObserver = new ResizeObserver(layout);
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
  /**
   * "held" is a direct-touch long press that has not moved: lifting makes it a secondary click,
   * moving makes it a drag. "pan" moves the picture, "scroll" the host's content.
   */
  let gesture: "pending" | "move" | "drag" | "held" | "pan" | "scroll" | "transform" | "done" =
    "done";
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
  const holdModifiers = (down: boolean, codes: ReadonlyArray<string>) => {
    for (const code of down ? codes : [...codes].reverse()) send({ type: "key", code, down });
  };
  const modifiersUsed = (codes: ReadonlyArray<string>) => {
    if (codes.length === 0) return;
    modifiers = [];
    options.onModifiersUsed?.();
  };
  /** The modifiers the primary button went down with, released when it comes up. */
  let pressedWith: ReadonlyArray<string> = [];
  const press = (down: boolean, point: { x: number; y: number }) => {
    if (down) {
      pressedWith = modifiers;
      holdModifiers(true, pressedWith);
    }
    send({ type: "button", button: 0, down, ...point, clicks: 1 });
    if (down) return;
    holdModifiers(false, pressedWith);
    modifiersUsed(pressedWith);
    pressedWith = [];
  };
  const click = (button: 0 | 1 | 2, point: { x: number; y: number }, clicks: number) => {
    const held = modifiers;
    cursor = point;
    send({ type: "move", ...point });
    holdModifiers(true, held);
    send({ type: "button", button, down: true, ...point, clicks });
    send({ type: "button", button, down: false, ...point, clicks });
    holdModifiers(false, held);
    modifiersUsed(held);
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

  /** Where a pan or scroll has travelled lately, for the speed it carries on with once lifted. */
  let trail: Array<{ at: number; x: number; y: number }> = [];
  const track = (dx: number, dy: number, at: number) => {
    const last = trail.at(-1);
    trail.push({ at, x: (last?.x ?? 0) + dx, y: (last?.y ?? 0) + dy });
    while (trail.length > 2 && at - (trail[0]?.at ?? at) > GLIDE_SAMPLE_MS) trail.shift();
  };
  let glideFrame = 0;
  const stopGlide = () => {
    if (glideFrame !== 0) cancelAnimationFrame(glideFrame);
    glideFrame = 0;
  };
  cleanups.push(stopGlide);
  /** Carries a lifted pan or scroll on, slowing the way a scroll view does. `step` returns false to stop. */
  const glide = (liftedAt: number, step: (dx: number, dy: number) => boolean) => {
    stopGlide();
    const first = trail[0];
    const last = trail.at(-1);
    trail = [];
    if (!first || !last || last.at <= first.at || liftedAt - last.at > GLIDE_REST_MS) return;
    let x = (last.x - first.x) / (last.at - first.at);
    let y = (last.y - first.y) / (last.at - first.at);
    if (Math.hypot(x, y) < GLIDE_START_PX_PER_MS) return;
    let previous: number | null = null;
    const tick = (now: number) => {
      const elapsed = previous === null ? 16 : clamp(now - previous, 1, 64);
      previous = now;
      const decay = Math.exp(-elapsed / GLIDE_TIME_CONSTANT_MS);
      // The distance covered while the speed fell by `decay`.
      const travel = GLIDE_TIME_CONSTANT_MS * (1 - decay);
      const going = step(x * travel, y * travel);
      x *= decay;
      y *= decay;
      glideFrame =
        going && Math.hypot(x, y) >= GLIDE_STOP_PX_PER_MS ? requestAnimationFrame(tick) : 0;
    };
    glideFrame = requestAnimationFrame(tick);
  };
  const panBy = (dx: number, dy: number) => {
    const fromX = panX;
    const fromY = panY;
    panX += dx;
    panY += dy;
    applyTransform();
    return panX !== fromX || panY !== fromY;
  };
  const scrollBy = (dx: number, dy: number) => {
    const scale = scrollScale();
    queueScroll(dx * scale, dy * scale);
    return true;
  };

  // A WebView types into a field focused from script only once a touch has focused it again.
  let keyboardUnclaimed = false;
  const focusKeyboardField = () => {
    keyboard.blur();
    keyboard.focus({ preventScroll: true });
  };
  listen(surface, "pointerdown", (event) => {
    if (isMouse(event)) return;
    event.preventDefault();
    if (keyboardUnclaimed) {
      keyboardUnclaimed = false;
      focusKeyboardField();
    }
    surface.setPointerCapture(event.pointerId);
    stopGlide();
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
    wakePointer();
    touchFingers = Math.max(touchFingers, touches.size);
    if (touches.size === 1) {
      gesture = "pending";
      touchStartedAt = event.timeStamp;
      lastMoveAt = event.timeStamp;
      touchFingers = 1;
      trail = [];
      const { clientX, clientY } = event;
      tapDrag =
        touchMode === "trackpad" &&
        lastTap !== null &&
        event.timeStamp - lastTap.at < DOUBLE_TAP_MS &&
        Math.hypot(clientX - lastTap.x, clientY - lastTap.y) < DOUBLE_TAP_SLOP_PX;
      longPress = setTimeout(() => {
        longPress = null;
        if (gesture !== "pending") return;
        options.onLongPress?.();
        if (touchMode === "touch") {
          gesture = "held";
          showLoupe(clientX, clientY);
          return;
        }
        // Holding still picks up what is under the pointer, ready to drag.
        gesture = "drag";
        flushNow();
        press(true, cursor);
      }, LONG_PRESS_MS);
      return;
    }
    clearLongPress();
    hideLoupe();
    if (gesture === "drag") {
      // Another finger ends the drag where it is, not wherever the gesture goes next.
      flushNow();
      press(false, cursor);
      gesture = "done";
    }
    if (touches.size === 2) {
      gesture = "pending";
      trail = [];
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
          // Under direct touch one finger moves the picture; a drag starts from a long press.
          gesture = "pan";
        }
      } else if (gesture === "held") {
        if (!moved(touch)) return;
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
      if (gesture === "pan") {
        track(touch.x - fromX, touch.y - fromY, event.timeStamp);
        panBy(touch.x - fromX, touch.y - fromY);
      } else if (touchMode === "touch") {
        const point = gesture === "drag" ? locate(touch.x, touch.y, true) : null;
        if (point) {
          queueMove(point);
          showLoupe(touch.x, touch.y);
        }
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
        wakePointer();
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
      // Two fingers moving together scroll the host in either mode; apart or together, they zoom.
      gesture = pinched ? "transform" : "scroll";
      if (gesture === "scroll" && touchMode === "touch") {
        const point = locate(pinch.centerX, pinch.centerY, false);
        if (point) queueMove(point);
      }
    }
    if (gesture === "scroll") {
      track(previous.x - center.x, previous.y - center.y, event.timeStamp);
      scrollBy(previous.x - center.x, previous.y - center.y);
    } else if (gesture === "transform") {
      const { bounds } = pictureRect();
      const nextZoom = clamp(pinch.zoom * (distance / (pinch.distance || 1)), 1, maxZoom());
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
    hideLoupe();
    if (touches.size > 0) return;
    const lifted = event.type === "pointerup";
    if (gesture === "drag") {
      flushNow();
      press(false, cursor);
    } else if (gesture === "held") {
      const point = locate(touch.startX, touch.startY, false);
      if (lifted && point) click(2, point, 1);
    } else if (gesture === "pan" && lifted) {
      glide(event.timeStamp, panBy);
    } else if (gesture === "scroll" && lifted) {
      glide(event.timeStamp, scrollBy);
    }
    const quick = event.timeStamp - touchStartedAt < TAP_MAX_MS;
    const point = touchMode === "trackpad" ? cursor : locate(touch.startX, touch.startY, false);
    if (gesture === "pending" && quick && lifted && point) {
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
    if (gesture === "transform") {
      // A pinch that ends a hair from the whole display meant the whole display.
      if (zoom < 1.05) showAt(1, cursor, VIEW_CHANGE_MS);
      reportViewport();
    }
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
  // On screen and focusable, or iOS refuses to raise the keyboard; 16px stops its focus zoom. At
  // the top, where no keyboard covers it, so a WebView has no reason to scroll the page to it.
  keyboard.style.cssText =
    "position:absolute;left:0;top:0;width:1px;height:1px;opacity:0;border:0;padding:0;font-size:16px;resize:none;";
  keyboard.value = REMOTE_DESKTOP_KEYBOARD_SENTINEL;
  let typed = REMOTE_DESKTOP_KEYBOARD_SENTINEL;
  surface.append(keyboard);
  const resetKeyboard = () => {
    typed = REMOTE_DESKTOP_KEYBOARD_SENTINEL;
    keyboard.value = typed;
  };
  const tap = (code: string) => {
    send({ type: "key", code, down: true });
    send({ type: "key", code, down: false });
  };
  const typeText = (text: string) => {
    if (!options.onText?.(text)) send({ type: "text", text });
  };
  const typeEdit = ({
    backspaces,
    lines,
  }: Pick<RemoteDesktopKeyboardEdit, "backspaces" | "lines">) => {
    for (let count = 0; count < backspaces; count += 1) tap("Backspace");
    lines.forEach((line, index) => {
      if (index > 0) tap("Enter");
      if (line) typeText(line);
    });
  };
  listen(keyboard, "input", () => {
    const edit = remoteDesktopKeyboardEdit(typed, keyboard.value);
    typeEdit(edit);
    typed = keyboard.value;
    if (edit.reset) resetKeyboard();
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
      if (message.type === "pointer") {
        // Once a finger has moved the pointer, this viewer knows where it is better than the host's start.
        if (touched || !Number.isFinite(message.x) || !Number.isFinite(message.y)) return;
        cursor = { x: clamp(message.x, 0, 1), y: clamp(message.y, 0, 1) };
        if (opened && !options.view) showAt(zoom, cursor);
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

  listen(video, "resize", layout);
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
      // A keyboard the system closed leaves the field focused, so this focuses it afresh.
      focusKeyboardField();
      keyboardUnclaimed = true;
    },
    blurKeyboard() {
      keyboardUnclaimed = false;
      keyboard.blur();
    },
    typeEdit,
    setTouchMode(mode) {
      touchMode = mode;
      placePointer();
    },
    setModifiers(codes) {
      modifiers = codes;
    },
    setBottomInset(inset, durationMs) {
      const next = Math.max(0, inset);
      if (next === insetBottom) return;
      const before = panY;
      const movedSince = lift !== null && panY !== lift.to;
      insetBottom = next;
      if (next === 0) {
        // Back to where the picture stood, unless it has been moved since.
        if (lift !== null && !movedSince) panY = lift.from;
        lift = null;
        applyTransform(durationMs);
        return;
      }
      followCursor(durationMs, INSET_MARGIN_PX);
      if (lift === null) lift = { from: before, to: panY };
      else if (!movedSince) lift.to = panY;
    },
    toggleZoom() {
      stopGlide();
      const readable = readableZoom();
      showAt(zoom > 1.01 ? 1 : readable > 1.01 ? readable : 2, cursor, VIEW_CHANGE_MS);
      reportViewport();
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
