import { EnvironmentId } from "@onicode/contracts";
import { expect, it, vi } from "vite-plus/test";

import { createThreadIntentPrefetch, prefetchesOnHover } from "./threadIntentPrefetch.logic";

const local = EnvironmentId.make("local");
const remote = EnvironmentId.make("remote");

it.each([
  { pointerType: "mouse", environmentId: local, localEnvironmentId: local, expected: true },
  { pointerType: "mouse", environmentId: remote, localEnvironmentId: local, expected: false },
  { pointerType: "mouse", environmentId: remote, localEnvironmentId: null, expected: false },
  { pointerType: "touch", environmentId: local, localEnvironmentId: local, expected: false },
  { pointerType: "pen", environmentId: local, localEnvironmentId: local, expected: false },
])(
  "hover prefetches a $pointerType row of $environmentId with local $localEnvironmentId: $expected",
  ({ expected, ...input }) => {
    expect(prefetchesOnHover(input)).toBe(expected);
  },
);

function fakeTimers(): Disposable {
  vi.useFakeTimers();
  return { [Symbol.dispose]: () => void vi.useRealTimers() };
}

it.each([false, true])(
  "keeps a row held by pointerdown while the pointer is back on it (hover prefetches: %s)",
  (hoverPrefetches) => {
    using _timers = fakeTimers();
    let holds = 0;
    let releases = 0;
    const hold = () => {
      holds += 1;
      return () => void (releases += 1);
    };
    const prefetch = createThreadIntentPrefetch();
    prefetch.pointerDown(hold);
    prefetch.pointerLeave();
    vi.advanceTimersByTime(500);
    prefetch.pointerEnter(hoverPrefetches, hold);
    vi.advanceTimersByTime(5_000);
    expect({ holds, releases }).toEqual({ holds: 1, releases: 0 });

    prefetch.pointerLeave();
    vi.advanceTimersByTime(1_000);
    expect({ holds, releases }).toEqual({ holds: 1, releases: 1 });
  },
);
