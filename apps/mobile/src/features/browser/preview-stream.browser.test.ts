import type {
  PreviewStreamControl,
  PreviewStreamEvents,
  PreviewStreamInput,
} from "@onicode/client-runtime/preview/server-browser-stream";
import { expect, it, vi } from "vite-plus/test";

import type { PreviewStreamMessage } from "./preview-stream-document";

const mocks = vi.hoisted(() => ({
  handlers: [] as PreviewStreamEvents[],
  sent: [] as PreviewStreamInput[],
  painted: [] as Array<() => void>,
}));

vi.mock(import("@onicode/client-runtime/preview/server-browser-stream"), async (original) => ({
  ...(await original()),
  createPreviewStreamClient: (_target, handlers) => {
    mocks.handlers.push(handlers);
    return { send: (input) => mocks.sent.push(input) > 0, stop: () => undefined };
  },
  createPreviewFramePainter: (_canvas, onPainted) => {
    // Like the real painter, a stopped one drops a decode still in flight.
    let stopped = false;
    mocks.painted.push(() => {
      if (!stopped) onPainted?.();
    });
    return { paint: () => undefined, stop: () => (stopped = true) };
  },
}));

import { start, stop } from "./preview-stream.browser";

it("keeps nothing from the previous connection's page", () => {
  vi.useFakeTimers();
  const posted: PreviewStreamMessage[] = [];
  vi.stubGlobal("ReactNativeWebView", {
    postMessage: (message: string) => posted.push(JSON.parse(message)),
  });
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(private readonly changed: () => void) {}
      observe() {
        this.changed();
      }
      disconnect() {}
    },
  );
  using _layout = vi
    .spyOn(HTMLElement.prototype, "getBoundingClientRect")
    .mockReturnValue(DOMRect.fromRect({ width: 400, height: 300 }));
  try {
    start({
      access: { httpBase: "https://environment.test", wsBase: "wss://environment.test", query: {} },
      threadId: "thread",
      tabId: "tab",
      interactive: true,
      background: "black",
      labels: {
        page: "Page",
        pageInput: "Page input",
        pictureInPictureNotReady: "Not ready",
        pictureInPictureUnavailable: "Unavailable",
      },
    });
    vi.advanceTimersByTime(0);
    const handlers = mocks.handlers.at(-1)!;
    const canvas = document.querySelector("canvas")!;
    canvas.width = 400;
    canvas.height = 300;
    canvas.setPointerCapture = () => undefined;
    const cursor = document.querySelector<HTMLElement>(".agent-cursor")!;
    const control = (controller: PreviewStreamControl["controller"]) =>
      handlers.onControl?.({ canOperate: true, controller, generation: 1, dialog: null });
    const statuses = () =>
      posted.flatMap((message) => (message.type === "status" ? [message.status] : []));

    handlers.onConnectedChange(true);
    control("agent");
    handlers.onViewport({ width: 400, height: 300 });
    mocks.painted.at(-1)!();
    handlers.onPointer?.({ phase: "move", x: 100, y: 100, sequence: 1 });
    expect(statuses().at(-1)).toBe("streaming");
    expect(cursor.style.opacity).toBe("1");

    // A decode from the closed connection lands after the disconnect.
    const lateDecode = mocks.painted.at(-1)!;
    handlers.onConnectedChange(false);
    lateDecode();
    expect(statuses().at(-1)).toBe("connecting");

    handlers.onConnectedChange(true);
    control("agent");
    vi.advanceTimersByTime(1_000);
    expect(cursor.style.opacity).toBe("0");

    // The remote page resized while disconnected, and the first frame beats its new size.
    control("you");
    const input = document.querySelector("textarea")!;
    expect(input.disabled).toBe(true);
    mocks.painted.at(-1)!();
    expect(input.disabled).toBe(false);
    const click = () => {
      canvas.dispatchEvent(
        new PointerEvent("pointerdown", {
          button: 0,
          buttons: 1,
          clientX: 10,
          clientY: 10,
          isPrimary: true,
          pointerType: "mouse",
        }),
      );
      return mocks.sent.filter((sent) => sent.type === "mouse");
    };
    mocks.sent.length = 0;
    expect(click()).toEqual([]);

    handlers.onViewport({ width: 800, height: 600 });
    expect(click()).toMatchObject([{ action: "down", x: 20, y: 20 }]);
  } finally {
    stop();
    vi.useRealTimers();
    vi.unstubAllGlobals();
  }
});
