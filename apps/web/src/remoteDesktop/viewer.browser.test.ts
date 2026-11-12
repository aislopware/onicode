import {
  createRemoteDesktopViewer,
  type RemoteDesktopTouchMode,
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

function setup(touchMode: RemoteDesktopTouchMode) {
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
  using screen = setup("touch");
  screen.touch("pointerdown", 1, 100, 75);
  screen.touch("pointerup", 1, 100, 75, 50);
  expect(screen.takeInput()).toEqual([
    { type: "move", x: 0.25, y: 0.25 },
    button(0, true, 0.25, 0.25),
    button(0, false, 0.25, 0.25),
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
