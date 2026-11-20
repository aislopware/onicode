import {
  createRemoteDesktopViewer,
  type RemoteDesktopTouchMode,
  type RemoteDesktopViewerOptions,
} from "@onicode/client-runtime/remote-desktop/viewer";
import { expect, it, vi } from "vite-plus/test";

import { fakeTimers, stubGlobal } from "../../test/render";

class FakeChannel extends EventTarget {
  readyState = "open";
  readonly sent: Array<Record<string, unknown>> = [];
  send(data: string) {
    this.sent.push(JSON.parse(data) as Record<string, unknown>);
  }
}

class FakePeer extends EventTarget {
  connectionState = "new";
  readonly channels = new Map<string, FakeChannel>();
  addTransceiver() {}
  createDataChannel(label: string) {
    const channel = new FakeChannel();
    this.channels.set(label, channel);
    return channel;
  }
  // Negotiation never finishes; these tests drive the channels and the state directly.
  createOffer() {
    return new Promise(() => undefined);
  }
  close() {}
}

function setup(
  touchMode: RemoteDesktopTouchMode,
  options: Partial<RemoteDesktopViewerOptions> = {},
) {
  const timers = fakeTimers();
  const peers: FakePeer[] = [];
  const peerGlobal = stubGlobal(
    "RTCPeerConnection",
    class extends FakePeer {
      constructor() {
        super();
        peers.push(this);
      }
    },
  );
  const container = document.createElement("div");
  container.style.cssText = "position:fixed;left:0;top:0;width:400px;height:300px;";
  document.body.append(container);
  const statuses: string[] = [];
  const viewer = createRemoteDesktopViewer({
    container,
    control: true,
    touchMode,
    signal: () => new Promise(() => undefined),
    onStatus: (status) => statuses.push(status),
    ...options,
  });
  const surface = container.firstElementChild as HTMLElement;
  // Synthetic pointer IDs have no browser capture state to take.
  surface.setPointerCapture = () => {};
  let now = 0;
  const touch = (type: string, pointerId: number, x: number, y: number, elapsedMs = 0) => {
    now += elapsedMs;
    const event = new PointerEvent(type, {
      bubbles: true,
      cancelable: true,
      pointerId,
      pointerType: "touch",
      clientX: x,
      clientY: y,
    });
    Object.defineProperty(event, "timeStamp", { value: now });
    surface.dispatchEvent(event);
  };
  return {
    peers,
    statuses,
    surface,
    viewer,
    touch,
    /** Input sent since the last call, without the pointer moves between clicks. */
    takeInput() {
      const input = peers.at(-1)!.channels.get("onicode-input")!;
      return input.sent.splice(0);
    },
    [Symbol.dispose]() {
      viewer.stop();
      container.remove();
      peerGlobal[Symbol.dispose]();
      timers[Symbol.dispose]();
    },
  };
}

const button = (button: number, down: boolean, x: number, y: number, clicks = 1) => ({
  type: "button",
  button,
  down,
  x,
  y,
  clicks,
});

it("moves the host pointer like a trackpad and clicks wherever it is", () => {
  using screen = setup("trackpad");
  const { touch, takeInput } = screen;

  // A slow stroke moves the pointer exactly as far as the finger: 80 of 400 pixels.
  touch("pointerdown", 1, 300, 200);
  touch("pointermove", 1, 340, 200, 400);
  touch("pointermove", 1, 380, 200, 400);
  touch("pointerup", 1, 380, 200, 10);
  takeInput();

  // The tap lands at the pointer, not under the finger.
  touch("pointerdown", 1, 20, 20, 1_000);
  touch("pointerup", 1, 20, 20, 50);
  expect(takeInput()).toEqual([
    { type: "move", x: 0.7, y: 0.5 },
    button(0, true, 0.7, 0.5),
    button(0, false, 0.7, 0.5),
  ]);

  touch("pointerdown", 1, 20, 20, 1_000);
  touch("pointerdown", 2, 60, 20, 10);
  touch("pointerup", 1, 20, 20, 50);
  touch("pointerup", 2, 60, 20, 5);
  expect(takeInput().filter((event) => event.type === "button")).toEqual([
    button(2, true, 0.7, 0.5),
    button(2, false, 0.7, 0.5),
  ]);

  touch("pointerdown", 1, 20, 20, 1_000);
  touch("pointerdown", 2, 60, 20, 10);
  touch("pointerdown", 3, 100, 20, 10);
  touch("pointerup", 3, 100, 20, 50);
  touch("pointerup", 2, 60, 20, 5);
  touch("pointerup", 1, 20, 20, 5);
  expect(takeInput().filter((event) => event.type === "button")).toEqual([
    button(1, true, 0.7, 0.5),
    button(1, false, 0.7, 0.5),
  ]);

  // Tap, then touch again at once and move: the second touch holds the button through the stroke.
  touch("pointerdown", 1, 200, 100, 1_000);
  touch("pointerup", 1, 200, 100, 50);
  touch("pointerdown", 1, 200, 100, 100);
  touch("pointermove", 1, 160, 100, 400);
  touch("pointerup", 1, 160, 100, 10);
  expect(takeInput().filter((event) => event.type === "button")).toEqual([
    button(0, true, 0.7, 0.5),
    button(0, false, 0.7, 0.5),
    button(0, true, 0.7, 0.5),
    button(0, false, 0.6, 0.5),
  ]);

  // Holding still picks the button up, and the stroke that follows drags.
  touch("pointerdown", 1, 200, 100, 1_000);
  vi.advanceTimersByTime(500);
  touch("pointermove", 1, 240, 100, 900);
  touch("pointerup", 1, 240, 100, 10);
  expect(takeInput().filter((event) => event.type === "button")).toEqual([
    button(0, true, 0.6, 0.5),
    button(0, false, 0.7, 0.5),
  ]);
});

it("clicks under the finger in direct touch mode", () => {
  const onModifiersUsed = vi.fn();
  using screen = setup("touch", { onModifiersUsed });
  const { touch, takeInput } = screen;
  touch("pointerdown", 1, 100, 75);
  touch("pointerup", 1, 100, 75, 50);
  expect(takeInput()).toEqual([
    { type: "move", x: 0.25, y: 0.25 },
    button(0, true, 0.25, 0.25),
    button(0, false, 0.25, 0.25),
  ]);

  // One finger moves the picture, so a stroke sends the host nothing.
  touch("pointerdown", 1, 100, 75, 1_000);
  touch("pointermove", 1, 160, 75, 400);
  touch("pointerup", 1, 160, 75, 400);
  vi.advanceTimersByTime(1_000);
  expect(takeInput()).toEqual([]);

  // A long press that lifts where it landed is the secondary button.
  touch("pointerdown", 1, 100, 75, 1_000);
  vi.advanceTimersByTime(500);
  expect(takeInput()).toEqual([]);
  touch("pointerup", 1, 100, 75, 600);
  expect(takeInput().filter((event) => event.type === "button")).toEqual([
    button(2, true, 0.25, 0.25),
    button(2, false, 0.25, 0.25),
  ]);

  // A long press that moves drags from where it landed.
  touch("pointerdown", 1, 100, 75, 1_000);
  vi.advanceTimersByTime(500);
  touch("pointermove", 1, 200, 150, 600);
  touch("pointerup", 1, 200, 150, 10);
  expect(takeInput().filter((event) => event.type === "button")).toEqual([
    button(0, true, 0.25, 0.25),
    button(0, false, 0.5, 0.5),
  ]);

  // A sticky modifier is held around the next click, then let go.
  screen.viewer.setModifiers(["MetaLeft"]);
  touch("pointerdown", 1, 100, 75, 1_000);
  touch("pointerup", 1, 100, 75, 50);
  expect(takeInput().filter((event) => event.type !== "move")).toEqual([
    { type: "key", code: "MetaLeft", down: true },
    button(0, true, 0.25, 0.25),
    button(0, false, 0.25, 0.25),
    { type: "key", code: "MetaLeft", down: false },
  ]);
  expect(onModifiersUsed).toHaveBeenCalledOnce();
  touch("pointerdown", 1, 100, 75, 1_000);
  touch("pointerup", 1, 100, 75, 50);
  expect(takeInput().some((event) => event.type === "key")).toBe(false);
});

it("scrolls the host with two fingers and carries a flick on", () => {
  using screen = setup("trackpad");
  const { touch, takeInput } = screen;
  const scrolled = () =>
    takeInput()
      .filter((event) => event.type === "scroll")
      .reduce((total, event) => total + Number(event.dy), 0);

  touch("pointerdown", 1, 100, 200);
  touch("pointerdown", 2, 140, 200, 5);
  for (let step = 1; step <= 4; step += 1) {
    touch("pointermove", 1, 100, 200 - step * 20, 8);
    touch("pointermove", 2, 140, 200 - step * 20, 0);
  }
  vi.advanceTimersByTime(16);
  // Fingers moving up 80 pixels scroll the content down by as much, less the 10 that began it.
  expect(scrolled()).toBeCloseTo(70);

  touch("pointerup", 1, 100, 120, 8);
  touch("pointerup", 2, 140, 120, 2);
  vi.advanceTimersByTime(2_000);
  const carried = scrolled();
  expect(carried).toBeGreaterThan(100);
  vi.advanceTimersByTime(2_000);
  expect(scrolled()).toBe(0);

  // Fingers that stop before lifting leave the content where it is.
  touch("pointerdown", 1, 100, 200, 1_000);
  touch("pointerdown", 2, 140, 200, 5);
  touch("pointermove", 1, 100, 160, 8);
  touch("pointermove", 2, 140, 160, 0);
  touch("pointerup", 1, 100, 160, 300);
  touch("pointerup", 2, 140, 160, 2);
  vi.advanceTimersByTime(2_000);
  expect(scrolled()).toBeCloseTo(40);
});

it("opens at a readable zoom and keeps the picture's size under a keyboard", async () => {
  const onZoomed = vi.fn();
  // 1,600 host points across a 400 pixel surface: a quarter of the scale its text needs.
  using screen = setup("trackpad", {
    display: { width: 3_200, height: 2_400, scaleFactor: 2 },
    startReadable: true,
    onZoomed,
  });
  const video = screen.surface.querySelector("video")!;
  const view = () => {
    const [, x, y, zoom] =
      /translate\((-?[\d.]+)px, ?(-?[\d.]+)px\) scale\(([\d.]+)\)/.exec(video.style.transform) ??
      [];
    return { x: Number(x), y: Number(y), zoom: Number(zoom) };
  };
  await vi.waitFor(() => expect(view().zoom).toBeCloseTo(3.4));
  // The middle of the display sits at the middle of the surface.
  expect(view()).toMatchObject({ x: 200 - 200 * 3.4, y: 150 - 150 * 3.4 });
  expect(onZoomed).toHaveBeenLastCalledWith(true);

  // A keyboard that leaves the pointer showing moves nothing; one that covers it moves the
  // picture only as far as showing the pointer takes, and back when it goes.
  const before = view();
  screen.viewer.setBottomInset(100, 0);
  expect(view()).toEqual(before);
  screen.viewer.setBottomInset(200, 0);
  expect(view()).toEqual({ ...before, y: before.y - 74 });
  screen.viewer.setBottomInset(0, 0);
  expect(view()).toEqual(before);

  screen.viewer.toggleZoom();
  expect(video.style.transform).toBe("");
  expect(onZoomed).toHaveBeenLastCalledWith(false);
  screen.viewer.toggleZoom();
  expect(view()).toEqual(before);
});

it("opens on the host's pointer, and where an earlier viewer left off", async () => {
  const display = { width: 3_200, height: 2_400, scaleFactor: 2 };
  const center = (video: HTMLVideoElement) => {
    const [, x, y, zoom] =
      /translate\((-?[\d.]+)px, ?(-?[\d.]+)px\) scale\(([\d.]+)\)/.exec(video.style.transform) ??
      [];
    return { x: (200 - Number(x)) / Number(zoom) / 400, y: (150 - Number(y)) / Number(zoom) / 300 };
  };
  const onView = vi.fn();
  {
    using screen = setup("trackpad", { display, startReadable: true, onView });
    const video = screen.surface.querySelector("video")!;
    await vi.waitFor(() => expect(center(video).x).toBeCloseTo(0.5));

    const control = screen.peers[0]!.channels.get("onicode-control")!;
    const pointer = { type: "pointer", x: 0.75, y: 0.25 };
    control.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(pointer) }));
    expect(center(video).x).toBeCloseTo(0.75);
    expect(center(video).y).toBeCloseTo(0.25);
    vi.advanceTimersByTime(300);
    expect(onView).toHaveBeenLastCalledWith({
      width: expect.closeTo(1_360),
      x: expect.closeTo(0.75),
      y: expect.closeTo(0.25),
      pointer: { x: 0.75, y: 0.25 },
    });

    // The first tap clicks where the host's pointer already was.
    screen.touch("pointerdown", 1, 20, 20, 1_000);
    screen.touch("pointerup", 1, 20, 20, 50);
    expect(screen.takeInput().filter((event) => event.type === "button")).toEqual([
      button(0, true, 0.75, 0.25),
      button(0, false, 0.75, 0.25),
    ]);
    // After that this viewer knows the pointer better than the host's start.
    control.dispatchEvent(
      new MessageEvent("message", { data: JSON.stringify({ ...pointer, x: 0.1 }) }),
    );
    expect(center(video).x).toBeCloseTo(0.75);
  }

  using screen = setup("trackpad", {
    display,
    startReadable: true,
    view: { width: 800, x: 0.4, y: 0.6, pointer: { x: 0.3, y: 0.7 } },
  });
  const video = screen.surface.querySelector("video")!;
  await vi.waitFor(() => expect(video.style.transform).toContain("scale(2)"));
  expect(center(video).x).toBeCloseTo(0.4);
  expect(center(video).y).toBeCloseTo(0.6);
  screen.touch("pointerdown", 1, 20, 20, 1_000);
  screen.touch("pointerup", 1, 20, 20, 50);
  expect(screen.takeInput().filter((event) => event.type === "button")).toEqual([
    button(0, true, 0.3, 0.7),
    button(0, false, 0.3, 0.7),
  ]);
});

it("reconnects after a lost path, and not after the host hangs up", () => {
  using screen = setup("trackpad");
  const { peers, statuses, surface } = screen;
  const video = surface.querySelector("video")!;

  video.dispatchEvent(new Event("playing"));
  peers[0]!.connectionState = "failed";
  peers[0]!.dispatchEvent(new Event("connectionstatechange"));
  expect(statuses).toEqual(["connecting", "streaming", "reconnecting"]);
  vi.advanceTimersByTime(500);
  expect(peers).toHaveLength(2);

  video.dispatchEvent(new Event("playing"));
  peers[1]!.channels.get("onicode-input")!.dispatchEvent(new Event("close"));
  expect(statuses).toEqual(["connecting", "streaming", "reconnecting", "streaming", "error"]);
  vi.advanceTimersByTime(60_000);
  expect(peers).toHaveLength(2);
});
