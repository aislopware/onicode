import { describe, expect, it } from "vite-plus/test";

import { createReplayHold } from "./replayHold";

function fakeReplay() {
  let id: string | undefined = "replay-1";
  let next = 2;
  const calls: string[] = [];
  // Each flush hands its finish to the test, which awaits it in order.
  const pending: Array<() => void> = [];
  const waiting: Array<(finish: () => void) => void> = [];
  const replay = {
    getReplayId: () => id,
    flush: () => {
      calls.push(`flush ${id}`);
      return new Promise<void>((resolve) => {
        const handOver = waiting.shift();
        if (handOver) handOver(resolve);
        else pending.push(resolve);
      });
    },
    stop: () => {
      if (id !== undefined) calls.push(`stop ${id}`);
      id = undefined;
      return Promise.resolve();
    },
    startBuffering: () => {
      id = `replay-${next++}`;
      calls.push(`buffer ${id}`);
    },
  };
  /** Resolves to the next flush's finish once that flush has started. */
  const flushStarted = (): Promise<() => void> => {
    const started = pending.shift();
    return started ? Promise.resolve(started) : new Promise((resolve) => waiting.push(resolve));
  };
  const flushed = async () => (await flushStarted())();
  return { replay, calls, flushStarted, flushed };
}

describe("createReplayHold", () => {
  it("sends each replay once and buffers afresh after each", async () => {
    const { replay, calls, flushed } = fakeReplay();
    const hold = createReplayHold(replay, () => false);
    const error = hold.sendBuffered("replay-1");
    await flushed();
    expect(await error).toBe("replay-1");
    const report = hold.sendBuffered();
    await flushed();
    expect(await report).toBe("replay-2");
    expect(calls).toEqual([
      "flush replay-1",
      "stop replay-1",
      "buffer replay-2",
      "flush replay-2",
      "stop replay-2",
      "buffer replay-3",
    ]);
  });

  it("links a report opened during an error's send to that replay, not the next buffer", async () => {
    const { replay, calls, flushed } = fakeReplay();
    const hold = createReplayHold(replay, () => false);
    const error = hold.sendBuffered("replay-1");
    const report = hold.sendBuffered();
    const secondError = hold.sendBuffered("replay-1");
    await flushed();
    expect(await error).toBe("replay-1");
    expect(await report).toBe("replay-1");
    expect(await secondError).toBe("replay-1");
    // The fresh buffer stays untouched for whatever comes next.
    expect(calls).toEqual(["flush replay-1", "stop replay-1", "buffer replay-2"]);
    expect(await hold.sendBuffered("replay-1")).toBeUndefined();
    expect(calls).toHaveLength(3);
  });

  it("stops at once on a private address mid-send, links nothing, and does not restart", async () => {
    const { replay, calls, flushStarted } = fakeReplay();
    let isPrivate = false;
    const hold = createReplayHold(replay, () => isPrivate);
    const report = hold.sendBuffered();
    const finish = await flushStarted();
    isPrivate = true;
    hold.syncAddress();
    expect(calls).toEqual(["flush replay-1", "stop replay-1"]);
    finish();
    expect(await report).toBeUndefined();
    expect(replay.getReplayId()).toBeUndefined();

    // Nothing is buffered there, so a report from that address sends no replay.
    expect(await hold.sendBuffered()).toBeUndefined();
    isPrivate = false;
    hold.syncAddress();
    expect(replay.getReplayId()).toBe("replay-2");
  });
});
