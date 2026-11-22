// @effect-diagnostics-next-line nodeBuiltinImport:off - The probe fixture uses a Node macrotask boundary.
import * as NodeTimersPromises from "node:timers/promises";
import {
  EnvironmentId,
  EventId,
  RunId,
  ThreadId,
  TurnItemId,
  type OrchestrationV2ProjectedTurnItem,
  type OrchestrationV2ThreadHistoryPage,
  type OrchestrationV2ThreadStreamItem,
  type PeerLinkClientPairingResult,
} from "@onicode/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Queue from "effect/Queue";
import * as Stream from "effect/Stream";
import { TestClock } from "effect/testing";

import {
  connectLinkedEnvironment,
  followLinkedThread,
  keepLinkedThreadView,
  LINKED_THREAD_RETRY_MS,
  type LinkedThreadView,
} from "./linkedThread.ts";
import { v2MessageRow, v2Now, v2Projection } from "./state/orchestrationV2TestFixtures.ts";

const snapshot = (
  snapshotSequence: number,
  historyCursor: string | null = null,
): OrchestrationV2ThreadStreamItem => ({
  kind: "snapshot",
  snapshotSequence,
  projection: v2Projection,
  historyCursor,
  hasMoreHistory: historyCursor !== null,
});

const retitled = (sequence: number, title: string): OrchestrationV2ThreadStreamItem => ({
  kind: "event",
  sequence,
  event: {
    id: EventId.make(`event:${sequence}`),
    type: "thread.metadata-updated",
    threadId: v2Projection.thread.id,
    occurredAt: v2Now,
    payload: { ...v2Projection.thread, title },
  },
});

const failure = (reason: string) => ({ _tag: "PeerLinkThreadReadError", reason });
const titleOf = (view: LinkedThreadView) => view.thread.projection?.thread.title ?? null;

it.effect("keeps what it read while a linked environment is out of reach, and asks it again", () =>
  Effect.gen(function* () {
    const attempts = [
      Stream.make(snapshot(1), retitled(2, "Renamed")).pipe(
        Stream.concat(Stream.fail(failure("unreachable"))),
      ),
      Stream.make(snapshot(5)).pipe(Stream.concat(Stream.fail(failure("expired")))),
    ];
    let opened = 0;
    const views = yield* Queue.unbounded<LinkedThreadView>();
    const following = yield* followLinkedThread({
      items: Stream.suspend(() => attempts[opened++]!),
      historyPage: () => Effect.die("unused"),
    }).pipe(
      Stream.runForEach((view) => Queue.offer(views, view)),
      Effect.flip,
      Effect.forkChild,
    );

    expect(titleOf(yield* Queue.take(views))).toBe(v2Projection.thread.title);
    expect(titleOf(yield* Queue.take(views))).toBe("Renamed");
    // Out of reach: the thread stays as read, with the reason beside it.
    const waiting = yield* Queue.take(views);
    expect([titleOf(waiting), waiting.error]).toEqual(["Renamed", failure("unreachable")]);
    expect(opened).toBe(1);

    yield* TestClock.adjust(LINKED_THREAD_RETRY_MS);
    // It answers again: its snapshot replaces the thread and clears the reason.
    const back = yield* Queue.take(views);
    expect([titleOf(back), back.error, back.thread.sequence]).toEqual([
      v2Projection.thread.title,
      null,
      5,
    ]);
    // A link that expired cannot come back by waiting, so the stream ends with it.
    expect(yield* Fiber.join(following)).toEqual(failure("expired"));
    expect(opened).toBe(2);
  }),
);

it.effect("keeps the thread a view showed while its reopened stream has read nothing", () =>
  Effect.gen(function* () {
    const attempts = [
      Stream.make(snapshot(1, "cursor-1"), retitled(2, "Renamed")),
      // The view closed and opened again while the linked environment was out of reach.
      Stream.fail(failure("unreachable")),
      Stream.make(snapshot(5)).pipe(Stream.concat(Stream.never)),
    ];
    let opened = 0;
    const follow = () =>
      followLinkedThread({
        items: Stream.suspend(() => attempts[opened++]!),
        historyPage: () => Effect.die("unused"),
      });
    let kept: LinkedThreadView | null = null;
    yield* follow().pipe(
      Stream.runForEach((view) => Effect.sync(() => (kept = keepLinkedThreadView(kept, view)))),
    );

    const views = yield* Queue.unbounded<LinkedThreadView>();
    yield* follow().pipe(
      Stream.runForEach((view) => Queue.offer(views, keepLinkedThreadView(kept, view))),
      Effect.forkScoped,
    );
    const waiting = yield* Queue.take(views);
    expect([titleOf(waiting), waiting.error]).toEqual(["Renamed", failure("unreachable")]);
    // Earlier turns belong to a stream that is gone, so none are offered until one is back.
    expect(waiting.thread.historyCursor).toBeNull();

    yield* TestClock.adjust(LINKED_THREAD_RETRY_MS);
    const back = yield* Queue.take(views);
    expect([titleOf(back), back.error, back.thread.sequence]).toEqual([
      v2Projection.thread.title,
      null,
      5,
    ]);
  }).pipe(Effect.scoped),
);

it.effect("loads the turns before a snapshot's window once per request", () =>
  Effect.gen(function* () {
    const asked: string[] = [];
    const page: OrchestrationV2ThreadHistoryPage = {
      snapshotSequence: 1,
      items: [],
      nextCursor: null,
      hasMoreHistory: false,
    };
    const views = yield* Queue.unbounded<LinkedThreadView>();
    yield* followLinkedThread({
      items: Stream.make(snapshot(1, "cursor-1")).pipe(Stream.concat(Stream.never)),
      historyPage: (cursor) =>
        Effect.sync(() => {
          asked.push(cursor);
          return page;
        }),
    }).pipe(
      Stream.runForEach((view) => Queue.offer(views, view)),
      Effect.forkScoped,
    );

    const shown = yield* Queue.take(views);
    expect(shown.thread.historyCursor).toBe("cursor-1");
    shown.loadEarlier();
    // A second tap while the page loads asks for nothing more.
    shown.loadEarlier();
    expect((yield* Queue.take(views)).history).toEqual({ loading: true, failed: false });
    const loaded = yield* Queue.take(views);
    expect([loaded.history.loading, loaded.thread.historyCursor]).toEqual([false, null]);
    // Nothing is left to load.
    loaded.loadEarlier();
    expect(asked).toEqual(["cursor-1"]);
  }).pipe(Effect.scoped),
);

it.effect("brings back the turns a closed view had loaded when it opens again", () =>
  Effect.gen(function* () {
    const older = v2MessageRow("Older", 1);
    const recent = v2MessageRow("Recent", 2);
    const latest = v2MessageRow("Latest", 3);
    const oldest = v2MessageRow("Oldest", 0);
    const afterRewind = v2MessageRow("After rewind", 4);
    const parentThreadId = ThreadId.make("thread-parent");
    const parentRow = (text: string, ordinal: number): OrchestrationV2ProjectedTurnItem => {
      const row = v2MessageRow(text, ordinal);
      return {
        ...row,
        visibility: "inherited",
        sourceThreadId: parentThreadId,
        item: { ...row.item, threadId: parentThreadId },
      };
    };
    const parentOlder = parentRow("Parent older", 2);
    const parentLatest = parentRow("Parent latest", 5);
    const forkLatest = v2MessageRow("Fork latest", 1);
    const forkEarlier = v2MessageRow("Fork earlier", 1);
    const forkLater = v2MessageRow("Fork later", 2);
    const forkAfterRewind = v2MessageRow("Fork after rewind", 3);
    // As the server builds it: the parent as source, ordinal 0, after the inherited rows.
    const forkMarker = (
      visibility: "synthetic" | "inherited",
      threadId: ThreadId,
    ): OrchestrationV2ProjectedTurnItem => {
      const id = TurnItemId.make(`turn-item:fork:${threadId}`);
      return {
        position: 0,
        visibility,
        sourceThreadId: parentThreadId,
        sourceItemId: id,
        item: {
          id,
          threadId,
          runId: null,
          nodeId: null,
          providerTurnId: null,
          nativeItemRef: null,
          parentItemId: null,
          ordinal: 0,
          status: "completed",
          title: "Forked from conversation",
          startedAt: null,
          completedAt: v2Now,
          updatedAt: v2Now,
          type: "fork",
          source: { type: "run", threadId: parentThreadId, runId: RunId.make("run-parent") },
          targetThreadId: threadId,
        },
      };
    };
    const window = (
      snapshotSequence: number,
      historyCursor: string,
      ...rows: ReadonlyArray<OrchestrationV2ProjectedTurnItem>
    ): OrchestrationV2ThreadStreamItem => ({
      kind: "snapshot",
      snapshotSequence,
      projection: {
        ...v2Projection,
        turnItems: rows.map((row) => row.item),
        visibleTurnItems: rows,
      },
      historyCursor,
      hasMoreHistory: true,
    });
    const ownForkMarker = forkMarker("synthetic", v2Projection.thread.id);
    const pages: Record<string, Pick<OrchestrationV2ThreadHistoryPage, "items" | "nextCursor">> = {
      "before-recent": { items: [older], nextCursor: "before-older" },
      "before-latest": { items: [recent], nextCursor: "before-recent" },
      "before-after-rewind": { items: [oldest], nextCursor: "before-oldest" },
      "before-oldest": { items: [], nextCursor: null },
      "before-parent-latest": { items: [parentOlder], nextCursor: "before-parent-older" },
      "before-fork-marker": { items: [parentLatest], nextCursor: "before-parent-latest" },
      "before-parent-older": { items: [], nextCursor: null },
      "before-fork-later": { items: [forkEarlier], nextCursor: "before-fork-earlier" },
      "before-fork-after-rewind": { items: [ownForkMarker], nextCursor: "before-fork-marker" },
    };
    const asked: string[] = [];
    const follow = (
      items: ReadonlyArray<OrchestrationV2ThreadStreamItem>,
      previous: LinkedThreadView | null,
    ) =>
      Effect.gen(function* () {
        const views = yield* Queue.unbounded<LinkedThreadView>();
        yield* followLinkedThread({
          items: Stream.fromIterable(items).pipe(Stream.concat(Stream.never)),
          historyPage: (cursor) =>
            Effect.sync(() => {
              asked.push(cursor);
              return { ...pages[cursor]!, snapshotSequence: 4, hasMoreHistory: true };
            }),
          previous,
        }).pipe(
          Stream.runForEach((view) => Queue.offer(views, view)),
          Effect.forkScoped,
        );
        return views;
      });
    const settled = Effect.fnUntraced(function* (views: Queue.Queue<LinkedThreadView>) {
      let view = yield* Queue.take(views);
      while (view.history.loading) view = yield* Queue.take(views);
      return view;
    });
    const rows = (view: LinkedThreadView) =>
      view.thread.projection?.visibleTurnItems.map((row) => row.sourceItemId);

    const first = yield* follow([window(1, "before-recent", recent), retitled(2, "Renamed")], null);
    (yield* Queue.take(first)).loadEarlier();
    yield* Queue.take(first); // Loading.
    yield* Queue.take(first); // The earlier page.
    const closed = yield* Queue.take(first);
    expect([rows(closed), closed.thread.sequence]).toEqual([
      [older.sourceItemId, recent.sourceItemId],
      2,
    ]);

    // Nothing changed while the view was closed: the rows it held are shown as they were.
    const same = yield* settled(yield* follow([window(2, "before-recent", recent)], closed));
    expect([rows(same), same.thread.historyCursor]).toEqual([
      [older.sourceItemId, recent.sourceItemId],
      "before-older",
    ]);
    expect(asked).toEqual(["before-recent"]);

    // The thread moved on: pages are read again until the oldest row it showed is back.
    const moved = yield* settled(yield* follow([window(3, "before-latest", latest)], closed));
    expect([rows(moved), moved.thread.historyCursor]).toEqual([
      [older.sourceItemId, recent.sourceItemId, latest.sourceItemId],
      "before-older",
    ]);
    expect(asked).toEqual(["before-recent", "before-latest", "before-recent"]);

    // Another client rewound past the oldest row it showed: an older row of that source stops it.
    asked.length = 0;
    const rewound = yield* settled(
      yield* follow([window(4, "before-after-rewind", afterRewind)], closed),
    );
    expect([rows(rewound), rewound.thread.historyCursor]).toEqual([
      [oldest.sourceItemId, afterRewind.sourceItemId],
      "before-oldest",
    ]);
    expect(asked).toEqual(["before-after-rewind"]);

    // A fork marker in the new window is newer than the parent rows it showed, despite ordinal 0.
    const forked = yield* follow([window(1, "before-parent-latest", parentLatest)], null);
    (yield* Queue.take(forked)).loadEarlier();
    yield* Queue.take(forked); // Loading.
    const forkClosed = yield* Queue.take(forked);
    expect(rows(forkClosed)).toEqual([parentOlder.sourceItemId, parentLatest.sourceItemId]);
    for (const marker of [
      forkMarker("synthetic", v2Projection.thread.id),
      // Forked again: the first fork's marker comes along as an inherited row.
      forkMarker("inherited", ThreadId.make("thread-middle")),
    ]) {
      asked.length = 0;
      const reopened = yield* settled(
        yield* follow([window(2, "before-fork-marker", marker, forkLatest)], forkClosed),
      );
      expect(rows(reopened)).toEqual([
        parentOlder.sourceItemId,
        parentLatest.sourceItemId,
        marker.sourceItemId,
        forkLatest.sourceItemId,
      ]);
      expect(asked).toEqual(["before-fork-marker", "before-parent-latest"]);
    }

    // The fork was rewound to its fork point, so none of its rows the view showed are left: its
    // marker stops the read instead of the parent's whole history.
    const forkOnly = yield* follow([window(1, "before-fork-later", forkLater)], null);
    (yield* Queue.take(forkOnly)).loadEarlier();
    yield* Queue.take(forkOnly); // Loading.
    const forkOnlyClosed = yield* Queue.take(forkOnly);
    expect(rows(forkOnlyClosed)).toEqual([forkEarlier.sourceItemId, forkLater.sourceItemId]);
    asked.length = 0;
    const forkRewound = yield* settled(
      yield* follow([window(2, "before-fork-after-rewind", forkAfterRewind)], forkOnlyClosed),
    );
    expect([rows(forkRewound), forkRewound.thread.historyCursor]).toEqual([
      [ownForkMarker.sourceItemId, forkAfterRewind.sourceItemId],
      "before-fork-marker",
    ]);
    expect(asked).toEqual(["before-fork-after-rewind"]);
  }).pipe(Effect.scoped),
);

describe("connectLinkedEnvironment", () => {
  const lan = "http://192.168.1.20:3773";
  const tailnet = "https://vm.tail.ts.net";
  const target = {
    environmentId: EnvironmentId.make("environment-vm"),
    label: "VM",
    urls: [lan, tailnet],
  };
  const credential: PeerLinkClientPairingResult = {
    type: "credential",
    ...target,
    credential: "ONE-TIME",
    expiresAt: DateTime.makeUnsafe(0),
  };
  const unsupported: PeerLinkClientPairingResult = {
    type: "unsupported",
    reason: "link_access",
    ...target,
  };
  const connect = (
    pairing: PeerLinkClientPairingResult | null,
    reachable: ReadonlyArray<string> | ((host: string) => Promise<boolean>),
    paired = true,
  ) => {
    const pairs: Array<{ host: string; pairingCode: string }> = [];
    return connectLinkedEnvironment({
      label: "vm (saved name)",
      requestPairing: async () => pairing,
      reaches:
        typeof reachable === "function" ? reachable : async (host) => reachable.includes(host),
      pair: async (input) => {
        pairs.push(input);
        return paired;
      },
      signIn: async () => false,
    }).then((outcome) => ({ outcome, pairs }));
  };

  it("redeems the pairing at the first address this device reaches", async () => {
    expect(await connect(credential, [tailnet])).toEqual({
      outcome: { type: "connected" },
      pairs: [{ host: tailnet, pairingCode: "ONE-TIME" }],
    });
    expect((await connect(credential, [tailnet, lan])).pairs[0]?.host).toBe(lan);
  });

  it.each([
    {
      name: "redeems at an alternate after the preferred probe fails",
      pairing: credential,
      preferredReachable: false,
      expected: {
        outcome: { type: "connected" },
        pairs: [{ host: tailnet, pairingCode: "ONE-TIME" }],
      },
    },
    {
      name: "prefills the preferred address even when an alternate answers first",
      pairing: unsupported,
      preferredReachable: true,
      expected: { outcome: { type: "manual", label: "VM", host: lan }, pairs: [] },
    },
  ])("$name without waiting for a silent last address", async (row) => {
    const last = "https://other.example.test";
    const preferred = Promise.withResolvers<boolean>();
    const silent = Promise.withResolvers<boolean>();
    const connecting = connect({ ...row.pairing, urls: [lan, tailnet, last] }, (host) => {
      if (host === lan) return preferred.promise;
      if (host === last) return silent.promise;
      queueMicrotask(() => preferred.resolve(row.preferredReachable));
      return Promise.resolve(true);
    });
    try {
      // oxlint-disable-next-line onicode/no-real-time-wait-in-tests -- Let the already runnable probe continuations finish while the last probe stays pending.
      const pending = NodeTimersPromises.setImmediate("still waiting for an address");
      expect(await Promise.race([connecting, pending])).toEqual(row.expected);
    } finally {
      silent.resolve(false);
      preferred.resolve(false);
    }
  });

  it("says the machine is out of reach, and spends nothing, when no address answers", async () => {
    expect(await connect(credential, [])).toEqual({
      outcome: { type: "unreachable", label: "VM" },
      pairs: [],
    });
  });

  it("leaves pairing to the user when the link cannot do it, with an address filled in", async () => {
    const manual = (host: string | null) => ({
      outcome: { type: "manual", label: "VM", host },
      pairs: [],
    });
    expect(await connect(unsupported, [tailnet])).toEqual(manual(tailnet));
    // An address the user can correct beats an empty field.
    expect(await connect(unsupported, [])).toEqual(manual(lan));
    expect(await connect({ ...unsupported, urls: [] }, [])).toEqual(manual(null));
    // The linking environment never answered, so only the client's own name is known.
    expect((await connect(null, [lan])).outcome).toEqual({
      type: "manual",
      label: "vm (saved name)",
      host: null,
    });
    // The one-time pairing was refused where it was redeemed.
    expect((await connect(credential, [lan], false)).outcome).toEqual({
      type: "manual",
      label: "VM",
      host: lan,
    });
  });

  it("signs in over the tailnet where the link cannot pair this device", async () => {
    const signIns: Array<string> = [];
    const tailnetIpv4 = "http://100.101.1.2:3773";
    const outcome = await connectLinkedEnvironment({
      label: "vm (saved name)",
      requestPairing: async () => ({
        type: "unsupported",
        reason: "link_access",
        ...target,
        urls: [lan, tailnetIpv4, tailnet],
      }),
      reaches: async () => true,
      pair: async () => true,
      signIn: async (host) => {
        signIns.push(host);
        return host === tailnet;
      },
    });
    // The LAN address answers first, but only tailnet addresses can sign a device in.
    expect({ outcome, signIns }).toEqual({
      outcome: { type: "connected" },
      signIns: [tailnetIpv4, tailnet],
    });
  });
});
