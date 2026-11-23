import { describe, expect, it } from "@effect/vitest";

import { LINK_PROBE_TIMED_OUT, makeLinkTargetProbe } from "./linkTargets.ts";

describe("makeLinkTargetProbe", () => {
  const fakeProbes = () => {
    const calls: string[] = [];
    const answers = new Map<string, (answer: boolean) => void>();
    const started = new Map<string, PromiseWithResolvers<void>>();
    const startOf = (fullPath: string) => {
      if (!started.has(fullPath)) started.set(fullPath, Promise.withResolvers<void>());
      return started.get(fullPath)!;
    };
    const stat = (fullPath: string) => {
      calls.push(fullPath);
      startOf(fullPath).resolve();
      return new Promise<boolean>((resolve) => answers.set(fullPath, resolve));
    };
    return {
      calls,
      answer: (fullPath: string) => answers.get(fullPath)!(true),
      started: (fullPath: string) => startOf(fullPath).promise,
      stat,
    };
  };

  it("shares one stat between overlapping browses of a link", async () => {
    const fake = fakeProbes();
    const isDirectory = makeLinkTargetProbe(fake.stat, 1);
    const signal = new AbortController().signal;
    const first = isDirectory("/links/share", signal);
    const second = isDirectory("/links/share", signal);
    // A caller cancelled rather than timed out leaves the probe shared.
    const cancelled = new AbortController();
    const third = isDirectory("/links/share", cancelled.signal);
    cancelled.abort();
    expect(await third).toBeUndefined();
    const fourth = isDirectory("/links/share", signal);
    fake.answer("/links/share");
    expect(await Promise.all([first, second, fourth])).toEqual([true, true, true]);
    expect(fake.calls).toEqual(["/links/share"]);
  });

  it("waits for a slot, and gives up without a stat when its timeout aborts", async () => {
    const fake = fakeProbes();
    const isDirectory = makeLinkTargetProbe(fake.stat, 1);
    const hung = isDirectory("/links/stalled", new AbortController().signal);
    const timedOut = new AbortController();
    const abandoned = isDirectory("/links/abandoned", timedOut.signal);
    const waiting = isDirectory("/links/healthy", new AbortController().signal);
    timedOut.abort();
    expect(await abandoned).toBeUndefined();

    fake.answer("/links/stalled");
    expect(await hung).toBe(true);
    await fake.started("/links/healthy");
    expect(fake.calls).toEqual(["/links/stalled", "/links/healthy"]);
    fake.answer("/links/healthy");
    expect(await waiting).toBe(true);
  });

  it("hands a free slot on when its caller aborts between the wake and taking it", async () => {
    const fake = fakeProbes();
    const isDirectory = makeLinkTargetProbe(fake.stat, 1);
    const busy = isDirectory("/links/busy", new AbortController().signal);
    // Aborts once the wake has detached the caller's abort listener, before the caller resumes.
    const late = new AbortController();
    const detach = late.signal.removeEventListener.bind(late.signal);
    Object.defineProperty(late.signal, "removeEventListener", {
      value: (...args: Parameters<typeof detach>) => {
        detach(...args);
        late.abort();
      },
    });
    const abandoned = isDirectory("/links/abandoned", late.signal);
    const waiting = isDirectory("/links/healthy", new AbortController().signal);
    fake.answer("/links/busy");
    expect(await busy).toBe(true);
    expect(await abandoned).toBeUndefined();

    await fake.started("/links/healthy");
    fake.answer("/links/healthy");
    expect(await waiting).toBe(true);
    expect(fake.calls).toEqual(["/links/busy", "/links/healthy"]);
  });

  it("answers at once when only stats that outlived a timeout hold the pool", async () => {
    const fake = fakeProbes();
    const isDirectory = makeLinkTargetProbe(fake.stat, 1);
    const timedOut = new AbortController();
    const hung = isDirectory("/links/stalled", timedOut.signal);
    // Queued before the stat outlived its timeout, it answers then rather than at its own.
    const queued = isDirectory("/links/queued", new AbortController().signal);
    timedOut.abort(LINK_PROBE_TIMED_OUT);
    expect(await hung).toBeUndefined();
    expect(await queued).toBeUndefined();

    expect(await isDirectory("/links/next", new AbortController().signal)).toBeUndefined();
    expect(await isDirectory("/links/stalled", new AbortController().signal)).toBeUndefined();
    expect(fake.calls).toEqual(["/links/stalled"]);
  });

  it("lets a queued browse of a link join the stat another caller starts for it", async () => {
    const fake = fakeProbes();
    const isDirectory = makeLinkTargetProbe(fake.stat, 1);
    const signal = new AbortController().signal;
    const busy = isDirectory("/links/busy", signal);
    const first = isDirectory("/links/share", signal);
    const second = isDirectory("/links/share", signal);
    fake.answer("/links/busy");
    await fake.started("/links/share");
    fake.answer("/links/share");
    expect(await Promise.all([busy, first, second])).toEqual([true, true, true]);
    expect(fake.calls).toEqual(["/links/busy", "/links/share"]);
  });
});
