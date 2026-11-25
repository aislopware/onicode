import "~/index.css";

import { RegistryContext } from "@effect/atom-react";
import { type LegendListRef } from "@legendapp/list/react";
import { scopeThreadRef } from "@onicode/client-runtime/environment";
import {
  type DeviceServiceState,
  type EnvironmentId,
  MessageId,
  type ScopedThreadRef,
  ThreadId,
} from "@onicode/contracts";
import { AsyncResult, Atom } from "effect/reactivity";
import { useEffect, useRef, useState } from "react";
import { expect, it, vi } from "vite-plus/test";

import { act, render } from "../../../test/render";
import {
  type PreviewMiniPlayerSource,
  selectThreadPreviewMiniPlayer,
  usePreviewMiniPlayerStore,
} from "../../previewMiniPlayerStore";
import { appAtomRegistry } from "../../rpc/atomRegistry";
import { deviceEnvironment } from "../../state/device";
import { ChatCanvas } from "../chat/ChatCanvas";
import { createChatCanvasStore } from "../chat/canvasStore";
import { MessagesTimeline } from "../chat/MessagesTimeline";
import type { TimelineViewport } from "../chat/timelineViewport";
import { useTimelineFollow } from "../chat/useTimelineFollow";
import { deviceStreamScreenAtom } from "../device/deviceStreamSession";
import { ThreadPreviewMiniPlayer, useThreadPreviewCanvasSource } from "./ThreadPreviewMiniPlayer";

vi.mock(import("../device/DeviceStreamView"), () => ({
  DeviceStreamView: (({ viewId }: { viewId: string }) => (
    <div data-testid="stream" data-view-id={viewId} />
  )) as never,
}));

const threadRef = scopeThreadRef("env-1" as EnvironmentId, ThreadId.make("thread-1"));
const defaultThreadRef = threadRef;
const phone: PreviewMiniPlayerSource = {
  kind: "device",
  hostId: "local",
  deviceId: "phone",
  platform: "ios",
  name: "Phone",
};

function Harness({ threadRef = defaultThreadRef }: { readonly threadRef?: ScopedThreadRef }) {
  const [store] = useState(createChatCanvasStore);
  const miniPlayer = usePreviewMiniPlayerStore((state) =>
    selectThreadPreviewMiniPlayer(state.byThreadKey, threadRef),
  );
  const preview = useThreadPreviewCanvasSource(threadRef, miniPlayer);
  return (
    <ChatCanvas store={store} threadRef={threadRef} preview={preview} composerOverlayElement={null}>
      {miniPlayer ? (
        <ThreadPreviewMiniPlayer threadRef={threadRef} miniPlayer={miniPlayer} />
      ) : null}
    </ChatCanvas>
  );
}

it("holds its frame through a source change and stops when the gesture is interrupted", async () => {
  using _state = vi
    .spyOn(deviceEnvironment, "state")
    .mockReturnValue(Atom.make<AsyncResult.AsyncResult<DeviceServiceState>>(AsyncResult.initial()));
  using _store = {
    [Symbol.dispose]: () => usePreviewMiniPlayerStore.getState().removeThread(threadRef),
  };
  usePreviewMiniPlayerStore.getState().open(threadRef, phone);
  const container = document.createElement("div");
  container.style.cssText = "display:flex;position:fixed;inset:0;width:1200px;height:800px";
  await using screen = await render(
    <RegistryContext.Provider value={appAtomRegistry}>
      <Harness />
    </RegistryContext.Provider>,
    { container },
  );
  const player = screen.container.querySelector<HTMLElement>("[data-preview-mini-player]")!;
  const handle = (direction: string) =>
    player.querySelector<HTMLElement>(`[data-preview-mini-player-resize="${direction}"]`)!;
  const rect = () => {
    const { left, top, right, bottom } = player.getBoundingClientRect();
    return { left, top, right, bottom };
  };
  const point = (target: HTMLElement, type: string, x: number, y: number, buttons: number) =>
    act(() =>
      target.dispatchEvent(
        new PointerEvent(type, {
          pointerId: 1,
          pointerType: "mouse",
          isPrimary: true,
          button: type === "pointermove" ? -1 : 0,
          buttons,
          clientX: x,
          clientY: y,
          bubbles: true,
          cancelable: true,
        }),
      ),
    );
  const viewId = screen.getByTestId("stream").element().getAttribute("data-view-id")!;
  const portrait = rect();

  // Held without moving while the device rotates: the frame waits for the release.
  await point(handle("northwest"), "pointerdown", portrait.left, portrait.top, 1);
  await act(() =>
    appAtomRegistry.set(deviceStreamScreenAtom(viewId), {
      width: 1_000,
      height: 2_000,
      orientation: "landscape_left",
    }),
  );
  expect(rect()).toEqual(portrait);

  // The release never arrived, so the next buttonless move ends the gesture where it was held.
  await point(handle("northwest"), "pointermove", portrait.left - 200, portrait.top - 200, 0);
  const landscape = rect();
  expect({ right: landscape.right, bottom: landscape.bottom }).toEqual({
    right: portrait.right,
    bottom: portrait.bottom,
  });
  expect(landscape.right - landscape.left).toBe(2 * (landscape.bottom - landscape.top));
  await point(handle("northwest"), "pointermove", portrait.left - 300, portrait.top - 300, 1);
  expect(rect()).toEqual(landscape);

  await point(handle("west"), "pointerdown", landscape.left, landscape.top + 10, 1);
  await act(() => window.dispatchEvent(new Event("blur")));
  await point(handle("west"), "pointermove", landscape.left - 100, landscape.top + 10, 1);
  expect(rect()).toEqual(landscape);
});

it("keeps one stream when switching between threads floating the same device", async () => {
  using _state = vi
    .spyOn(deviceEnvironment, "state")
    .mockReturnValue(Atom.make<AsyncResult.AsyncResult<DeviceServiceState>>(AsyncResult.initial()));
  const otherThreadRef = scopeThreadRef("env-1" as EnvironmentId, ThreadId.make("thread-2"));
  using _store = {
    [Symbol.dispose]: () => {
      usePreviewMiniPlayerStore.getState().removeThread(threadRef);
      usePreviewMiniPlayerStore.getState().removeThread(otherThreadRef);
    },
  };
  usePreviewMiniPlayerStore.getState().open(threadRef, phone);
  usePreviewMiniPlayerStore.getState().open(otherThreadRef, phone);
  const harness = (ref: ScopedThreadRef) => (
    <RegistryContext.Provider value={appAtomRegistry}>
      <Harness threadRef={ref} />
    </RegistryContext.Provider>
  );
  const container = document.createElement("div");
  container.style.cssText = "display:flex;position:fixed;inset:0;width:1200px;height:800px";
  await using screen = await render(harness(threadRef), { container });
  const viewId = () => screen.getByTestId("stream").element().getAttribute("data-view-id");
  const first = viewId();

  // The stream session is keyed by view, so a new view id reconnects the device.
  await screen.rerender(harness(otherThreadRef));
  expect(viewId()).toBe(first);
});

const timelineEntries = Array.from({ length: 5 }, (_, index) => ({
  id: `entry-msg-${index}`,
  kind: "message" as const,
  createdAt: "2026-03-17T19:12:28.000Z",
  message: {
    id: MessageId.make(`msg-${index}`),
    role: index % 2 === 0 ? ("user" as const) : ("assistant" as const),
    text: `Message ${index}: ${"word ".repeat(18)}`,
    runId: null,
    createdAt: "2026-03-17T19:12:28.000Z",
    updatedAt: "2026-03-17T19:12:28.000Z",
    streaming: false,
  },
}));

function TimelinePreviewHarness({
  threadRef: activeThreadRef,
  composerInset = 80,
  onViewport,
  onFollowState,
}: {
  readonly threadRef: ScopedThreadRef;
  readonly composerInset?: number;
  readonly onViewport?: (viewport: TimelineViewport) => void;
  readonly onFollowState?: (state: {
    readonly liveFollowEnabled: boolean;
    readonly pressPageUp: () => void;
  }) => void;
}) {
  const [store] = useState(createChatCanvasStore);
  const listRef = useRef<LegendListRef | null>(null);
  const threadKey = `${activeThreadRef.environmentId}:${activeThreadRef.threadId}`;
  const miniPlayer = usePreviewMiniPlayerStore((state) =>
    selectThreadPreviewMiniPlayer(state.byThreadKey, activeThreadRef),
  );
  const preview = useThreadPreviewCanvasSource(activeThreadRef, miniPlayer);
  const follow = useTimelineFollow({
    legendListRef: listRef,
    composerTimelineInset: composerInset,
    routeThreadKey: threadKey,
    activeThreadKey: threadKey,
    isServerThread: false,
    serverProjection: null,
    threadStatus: "live",
    activeActivityRun: null,
    activeRunningTurnId: null,
    timelineEntries,
    timelineAnchorMessageId: null,
    setTimelineAnchor: () => {},
  });
  useEffect(() => {
    onFollowState?.({
      liveFollowEnabled: follow.timelineLiveFollowEnabled,
      pressPageUp: () => follow.onComposerPageScrollKeyDownStable("PageUp"),
    });
  });
  return (
    <ChatCanvas
      store={store}
      threadRef={activeThreadRef}
      preview={preview}
      composerOverlayElement={null}
    >
      <div className="relative flex min-h-0 flex-1 flex-col">
        <MessagesTimeline
          listRef={listRef}
          timelineEntries={timelineEntries}
          isWorking={false}
          latestRun={null}
          turnDiffSummaries={[]}
          runs={[]}
          routeThreadKey={threadKey}
          onOpenThread={() => {}}
          supportsConversationRollback={false}
          onRevertToTurnCount={() => {}}
          isRevertingCheckpoint={false}
          onImageExpand={() => {}}
          activeThreadEnvironmentId={activeThreadRef.environmentId}
          markdownCwd={undefined}
          resolvedTheme="light"
          timestampFormat="locale"
          workspaceRoot={undefined}
          anchorMessageId={null}
          onAnchorReady={() => {}}
          onAnchorSizeChanged={() => {}}
          contentInsetEndAdjustment={composerInset}
          emptyPlaceholderInset={composerInset}
          liveFollowEnabled={follow.timelineLiveFollowEnabled}
          onIsAtEndChange={follow.onIsAtEndChangeStable}
          onFooterGrow={follow.onTimelineFooterGrowStable}
          onViewportAttach={(viewport) => {
            onViewport?.(viewport);
            return follow.attachTimelineViewport(viewport);
          }}
        />
      </div>
      {miniPlayer ? (
        <ThreadPreviewMiniPlayer threadRef={activeThreadRef} miniPlayer={miniPlayer} />
      ) : null}
    </ChatCanvas>
  );
}

it("holds the timeline end inset steady during a drag and settles it on release", async () => {
  const dragThreadRef = scopeThreadRef("env-1" as EnvironmentId, ThreadId.make("thread-drag"));
  using _state = vi
    .spyOn(deviceEnvironment, "state")
    .mockReturnValue(Atom.make<AsyncResult.AsyncResult<DeviceServiceState>>(AsyncResult.initial()));
  using _store = {
    [Symbol.dispose]: () => usePreviewMiniPlayerStore.getState().removeThread(dragThreadRef),
  };
  usePreviewMiniPlayerStore.getState().open(dragThreadRef, phone);
  let attachedViewport: TimelineViewport | null = null;
  const container = document.createElement("div");
  container.style.cssText = "display:flex;position:fixed;inset:0;width:720px;height:700px";
  await using screen = await render(
    <RegistryContext.Provider value={appAtomRegistry}>
      <TimelinePreviewHarness
        threadRef={dragThreadRef}
        onViewport={(viewport) => {
          attachedViewport = viewport;
        }}
      />
    </RegistryContext.Provider>,
    { container },
  );
  const viewId = screen.getByTestId("stream").element().getAttribute("data-view-id")!;
  using _screen = {
    [Symbol.dispose]: () => appAtomRegistry.set(deviceStreamScreenAtom(viewId), null),
  };
  await act(() =>
    appAtomRegistry.set(deviceStreamScreenAtom(viewId), {
      width: 1_000,
      height: 600,
      orientation: "landscape_left",
    }),
  );
  const player = screen.container.querySelector<HTMLElement>("[data-preview-mini-player]")!;
  const handle = player.querySelector<HTMLElement>(".cursor-grab")!;
  let initialTop = 0;
  let initialInset = 0;
  await vi.waitFor(() => {
    const rect = player.getBoundingClientRect();
    expect(rect.width).toBeGreaterThan(rect.height);
    initialTop = rect.top;
    initialInset = Math.ceil(700 - initialTop);
    expect(attachedViewport?.endInset()).toBe(initialInset);
  });
  const insetChanges: number[] = [];
  using _sub = {
    [Symbol.dispose]: attachedViewport!.onEndInsetChange((inset) => insetChanges.push(inset)),
  };
  const point = (type: string, x: number, y: number, buttons: number) =>
    act(() =>
      handle.dispatchEvent(
        new PointerEvent(type, {
          pointerId: 1,
          pointerType: "mouse",
          isPrimary: true,
          button: type === "pointermove" ? -1 : 0,
          buttons,
          clientX: x,
          clientY: y,
          bubbles: true,
          cancelable: true,
        }),
      ),
    );
  const start = handle.getBoundingClientRect();
  const startX = start.left + start.width / 2;
  const startY = start.top + start.height / 2;
  await point("pointerdown", startX, startY, 1);
  for (const offset of [20, 40, 60]) {
    await point("pointermove", startX, startY - offset, 1);
    expect(attachedViewport!.endInset()).toBe(initialInset);
    expect(insetChanges).toEqual([]);
  }
  const movedTop = player.getBoundingClientRect().top;
  expect(movedTop).toBeLessThan(initialTop - 40);

  await point("pointerup", startX, startY - 60, 0);
  const settledInset = Math.ceil(700 - movedTop);
  expect(attachedViewport!.endInset()).toBe(settledInset);
  expect(insetChanges).toEqual([settledInset]);
});

it("scrolls the last rows clear when a floating preview opens and lets PageUp release follow", async () => {
  const followThreadRef = scopeThreadRef("env-1" as EnvironmentId, ThreadId.make("thread-follow"));
  using _state = vi
    .spyOn(deviceEnvironment, "state")
    .mockReturnValue(Atom.make<AsyncResult.AsyncResult<DeviceServiceState>>(AsyncResult.initial()));
  using _store = {
    [Symbol.dispose]: () => usePreviewMiniPlayerStore.getState().removeThread(followThreadRef),
  };
  let followState = { liveFollowEnabled: true, pressPageUp: () => {} };
  const container = document.createElement("div");
  container.style.cssText = "display:flex;position:fixed;inset:0;width:720px;height:700px";
  await using screen = await render(
    <RegistryContext.Provider value={appAtomRegistry}>
      <TimelinePreviewHarness
        threadRef={followThreadRef}
        composerInset={80}
        onFollowState={(next) => {
          followState = next;
        }}
      />
    </RegistryContext.Provider>,
    { container },
  );
  const lastRow = () =>
    screen.container.querySelector<HTMLElement>('[data-timeline-row-id="entry-msg-4"]');
  await vi.waitFor(() => {
    expect(lastRow()).not.toBeNull();
  });
  expect(followState.liveFollowEnabled).toBe(true);

  await act(() => {
    usePreviewMiniPlayerStore.getState().open(followThreadRef, phone);
  });
  await vi.waitFor(() => {
    const player = screen.container.querySelector<HTMLElement>("[data-preview-mini-player]");
    expect(player).not.toBeNull();
    expect(lastRow()!.getBoundingClientRect().bottom).toBeLessThanOrEqual(
      player!.getBoundingClientRect().top + 1,
    );
  });

  await act(() => {
    followState.pressPageUp();
  });
  await vi.waitFor(() => {
    expect(followState.liveFollowEnabled).toBe(false);
  });
});
