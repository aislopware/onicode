import { expect, it, vi } from "vite-plus/test";

import {
  createReplayScrollPause,
  REPLAY_MAX_PAUSE_MS,
  REPLAY_RESUME_DELAY_MS,
} from "./replayScrollPause";

function setup() {
  vi.useFakeTimers();
  const calls: string[] = [];
  const pause = createReplayScrollPause({
    pause: () => calls.push("pause"),
    resume: () => calls.push("resume"),
  });
  return { calls, pause, [Symbol.dispose]: () => vi.useRealTimers() };
}

it("leaves taps alone and pauses a drag once until the fling settles", () => {
  using t = setup();
  t.pause.touchStart(100, 100);
  t.pause.touchMove(104, 106);
  t.pause.touchEnd(0);
  vi.advanceTimersByTime(REPLAY_MAX_PAUSE_MS);
  expect(t.calls).toEqual([]);

  t.pause.touchStart(100, 100);
  t.pause.touchMove(100, 120);
  t.pause.touchMove(100, 200);
  t.pause.touchEnd(0);
  vi.advanceTimersByTime(REPLAY_RESUME_DELAY_MS - 1);
  expect(t.calls).toEqual(["pause"]);
  vi.advanceTimersByTime(1);
  expect(t.calls).toEqual(["pause", "resume"]);
});

it("resumes after a native scroll cancels the touch, even when the cancel still lists it", () => {
  using t = setup();
  t.pause.touchStart(100, 100);
  t.pause.touchCancel();
  vi.advanceTimersByTime(REPLAY_RESUME_DELAY_MS);
  expect(t.calls).toEqual(["pause", "resume"]);
});

it("holds the pause while a finger stops a fling, and never past the cap", () => {
  using t = setup();
  t.pause.touchStart(100, 100);
  t.pause.touchCancel();
  vi.advanceTimersByTime(REPLAY_RESUME_DELAY_MS - 1);
  t.pause.touchStart(100, 300);
  vi.advanceTimersByTime(REPLAY_MAX_PAUSE_MS - 1);
  expect(t.calls).toEqual(["pause"]);
  vi.advanceTimersByTime(1);
  expect(t.calls).toEqual(["pause", "resume"]);
});

it("resumes at once on request and drops the pending resume", () => {
  using t = setup();
  t.pause.touchStart(0, 0);
  t.pause.touchMove(0, 50);
  t.pause.resumeNow();
  t.pause.touchEnd(0);
  vi.advanceTimersByTime(REPLAY_MAX_PAUSE_MS);
  expect(t.calls).toEqual(["pause", "resume"]);
});
