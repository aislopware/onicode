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

  it("keys links into one share by their target, leaving the other slots free", async () => {
    const fake = fakeProbes();
    const isDirectory = makeLinkTargetProbe(fake.stat, 2);
    const timedOut = new AbortController();
    const first = isDirectory("/links/a", timedOut.signal, "/mnt/share");
    void isDirectory("/links/b", new AbortController().signal, "/mnt/share");
    const healthy = isDirectory("/links/healthy", new AbortController().signal, "/local/real");
    expect(fake.calls).toEqual(["/links/a", "/links/healthy"]);
    fake.answer("/links/healthy");
    expect(await healthy).toBe(true);

    timedOut.abort(LINK_PROBE_TIMED_OUT);
    expect(await first).toBeUndefined();
    // Below the stuck target, a link answers at once and takes no slot.
    const signal = new AbortController().signal;
    expect(await isDirectory("/links/c", signal, "/mnt/share/sub")).toBeUndefined();
    const next = isDirectory("/links/next", signal, "/local/next");
    expect(fake.calls).toEqual(["/links/a", "/links/healthy", "/links/next"]);
    fake.answer("/links/next");
    expect(await next).toBe(true);
  });

  it("answers callers queued below a target once its probe outlives a timeout", async () => {
    const fake = fakeProbes();
    const isDirectory = makeLinkTargetProbe(fake.stat, 2);
    const timedOut = new AbortController();
    const hung = isDirectory("/links/a", timedOut.signal, "/mnt/share");
    const busy = isDirectory("/links/busy", new AbortController().signal, "/local/busy");
    const queuedTimedOut = new AbortController();
    const queued = isDirectory("/links/b", queuedTimedOut.signal, "/mnt/share/sub");
    const waiting = isDirectory("/links/c", new AbortController().signal, "/mnt/share/other");
    // Both timeouts fire in one tick: retrying the queued one would only reach the stuck share.
    queuedTimedOut.abort(LINK_PROBE_TIMED_OUT);
    timedOut.abort(LINK_PROBE_TIMED_OUT);
    expect(await hung).toBeUndefined();
    expect(await queued).toBeUndefined();
    // Answers before the busy probe frees a slot.
    expect(await waiting).toBeUndefined();

    fake.answer("/links/busy");
    expect(await busy).toBe(true);
    expect(fake.calls).toEqual(["/links/a", "/links/busy"]);
  });

  it("tells a caller that timed out queued behind answering probes it was crowded", async () => {
    const fake = fakeProbes();
    const isDirectory = makeLinkTargetProbe(fake.stat, 1);
    const busy = isDirectory("/links/busy", new AbortController().signal);
    const timedOut = new AbortController();
    const queued = isDirectory("/links/queued", timedOut.signal);
    timedOut.abort(LINK_PROBE_TIMED_OUT);
    expect(await queued).toBe("crowded");

    fake.answer("/links/busy");
    expect(await busy).toBe(true);
    expect(fake.calls).toEqual(["/links/busy"]);
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
