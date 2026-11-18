import { assert, describe, it } from "@effect/vitest";

import {
  CURSOR_MIN_FETCH_SPAN_MS,
  cursorFetchRange,
  isCursorCacheFresh,
  mergeCursorFetch,
  narrowedCursorSpan,
  widenedCursorSpan,
} from "./accountCache.ts";
import type { UsageRecord } from "@onicode/provider-core/server/usage";

const DAY_MS = 24 * 60 * 60 * 1000;

const record = (timestampMs: number): UsageRecord => ({
  provider: "cursor",
  timestampMs,
  model: "claude-fable-5",
  sessionId: "conversation-1",
  totals: {
    uncachedInputTokens: 0,
    cachedInputTokens: 0,
    cacheCreationTokens: 0,
    outputTokens: 1,
    reasoningTokens: 0,
  },
  reportedCostUsd: null,
  speed: "standard",
  dedupeKey: `cursor-account:a:${timestampMs}:0`,
});

describe("cursorAccountCache", () => {
  it("reads only the history the cache is missing", () => {
    const cache = {
      accountKey: "a",
      sinceMs: 0,
      untilMs: 10 * DAY_MS,
      fetchedAtMs: 10 * DAY_MS,
      records: [record(5 * DAY_MS)],
    };
    assert.isTrue(isCursorCacheFresh(cache, 10 * DAY_MS, 0));
    const narrow = { ...cache, sinceMs: DAY_MS };
    assert.isFalse(isCursorCacheFresh(narrow, 10 * DAY_MS, 0));
    const widenedRange = cursorFetchRange(narrow, 0, 10 * DAY_MS, Number.POSITIVE_INFINITY);
    assert.deepStrictEqual(widenedRange, { sinceMs: 0, untilMs: DAY_MS });
    const widened = mergeCursorFetch(narrow, "a", widenedRange!, cache.records, 10 * DAY_MS, 0);
    assert.isTrue(isCursorCacheFresh(widened.cache, 10 * DAY_MS, 0));
    assert.deepStrictEqual(
      cursorFetchRange(widened.cache, 0, 11 * DAY_MS, Number.POSITIVE_INFINITY),
      {
        sinceMs: 10 * DAY_MS - 60 * 60 * 1000,
        untilMs: 11 * DAY_MS,
      },
    );
    const nowMs = 100 * DAY_MS;
    const range = cursorFetchRange(cache, 10 * DAY_MS, nowMs, Number.POSITIVE_INFINITY)!;
    assert.deepStrictEqual(range, { sinceMs: 10 * DAY_MS, untilMs: nowMs });

    const { cache: merged } = mergeCursorFetch(
      cache,
      "a",
      range,
      [record(50 * DAY_MS)],
      nowMs,
      10 * DAY_MS,
    );
    assert.deepStrictEqual(
      merged.records.map((entry) => entry.timestampMs),
      [50 * DAY_MS],
    );
    assert.deepStrictEqual(
      cursorFetchRange(merged, 11 * DAY_MS, nowMs + DAY_MS, Number.POSITIVE_INFINITY),
      {
        sinceMs: nowMs - 60 * 60 * 1000,
        untilMs: nowMs + DAY_MS,
      },
    );
  });

  it("reports an edge that only confirms the cache as unchanged", () => {
    const cache = {
      accountKey: "a",
      sinceMs: 0,
      untilMs: 10 * DAY_MS,
      fetchedAtMs: 10 * DAY_MS,
      records: [record(DAY_MS), record(10 * DAY_MS)],
    };
    const range = cursorFetchRange(cache, 0, 11 * DAY_MS, Number.POSITIVE_INFINITY)!;
    assert.isFalse(
      mergeCursorFetch(cache, "a", range, [record(10 * DAY_MS)], 11 * DAY_MS, 0).changed,
    );
    // A retention start that moved on without pruning a record is not worth a write either.
    assert.isFalse(
      mergeCursorFetch(cache, "a", range, [record(10 * DAY_MS)], 11 * DAY_MS, DAY_MS / 2).changed,
    );
    const pruned = mergeCursorFetch(
      cache,
      "a",
      range,
      [record(10 * DAY_MS)],
      11 * DAY_MS,
      2 * DAY_MS,
    );
    assert.isTrue(pruned.changed);
    assert.deepStrictEqual(pruned.cache.records, [record(10 * DAY_MS)]);
    assert.isTrue(
      mergeCursorFetch(cache, "a", range, [record(10 * DAY_MS)], 11 * DAY_MS, 2 * DAY_MS).changed,
    );
    assert.isTrue(
      mergeCursorFetch(
        { ...cache, sinceMs: DAY_MS, records: [] },
        "a",
        { sinceMs: 0, untilMs: 11 * DAY_MS },
        [],
        11 * DAY_MS,
        0,
      ).changed,
    );
    assert.isTrue(
      mergeCursorFetch(
        cache,
        "a",
        range,
        [record(10 * DAY_MS), record(10.5 * DAY_MS)],
        11 * DAY_MS,
        0,
      ).changed,
    );
  });

  it("never reads the same range twice, and every read touches the retained cache", () => {
    const HOUR_MS = 60 * 60 * 1000;
    const nowMs = 100 * DAY_MS;
    for (const { retentionStartMs, cache } of [
      // A window shorter than the refetch overlap.
      {
        retentionStartMs: nowMs - HOUR_MS / 2,
        cache: { sinceMs: nowMs - 10 * 60 * 1000, untilMs: nowMs, records: [] },
      },
      // A cache that ends just after the retention start still holds retained history.
      {
        retentionStartMs: 10 * DAY_MS,
        cache: {
          sinceMs: 9 * DAY_MS,
          untilMs: 10 * DAY_MS + HOUR_MS / 2,
          records: [record(10 * DAY_MS + 60 * 1000)],
        },
      },
    ]) {
      let current: Parameters<typeof cursorFetchRange>[0] = {
        accountKey: "a",
        fetchedAtMs: nowMs,
        ...cache,
      };
      const seen = new Set<string>();
      for (let reads = 0; ; reads++) {
        assert.isBelow(reads, 3000);
        if (isCursorCacheFresh(current, nowMs, retentionStartMs)) break;
        const range = cursorFetchRange(current, retentionStartMs, nowMs, CURSOR_MIN_FETCH_SPAN_MS);
        assert.isNotNull(range);
        if (range === null) return;
        assert.isFalse(seen.has(`${range.sinceMs}:${range.untilMs}`));
        seen.add(`${range.sinceMs}:${range.untilMs}`);
        assert.isAtMost(range.sinceMs, current.untilMs);
        assert.isAtLeast(range.untilMs, current.sinceMs);
        const fetched = cache.records.filter(
          (entry) => entry.timestampMs >= range.sinceMs && entry.timestampMs <= range.untilMs,
        );
        current = mergeCursorFetch(current, "a", range, fetched, nowMs, retentionStartMs).cache;
      }
      assert.deepStrictEqual(
        current.records,
        cache.records.filter((entry) => entry.timestampMs >= retentionStartMs),
      );
    }
  });

  it("keeps the full refetch overlap at the narrowest span", () => {
    const cache = {
      accountKey: "a",
      sinceMs: 0,
      untilMs: 10 * DAY_MS,
      fetchedAtMs: 10 * DAY_MS,
      records: [record(10 * DAY_MS - 30 * 60 * 1000)],
    };
    const range = cursorFetchRange(cache, 0, 10 * DAY_MS + 60 * 1000, CURSOR_MIN_FETCH_SPAN_MS);
    assert.strictEqual(range?.sinceMs, 10 * DAY_MS - 60 * 60 * 1000);
    assert.strictEqual(range?.untilMs, 10 * DAY_MS + 60 * 1000);
  });

  it("serves a cache as fresh only when it reaches the time it was read", () => {
    const nowMs = 10 * DAY_MS;
    const complete = {
      accountKey: "a",
      sinceMs: 0,
      untilMs: nowMs,
      fetchedAtMs: nowMs,
      records: [],
    };
    assert.isTrue(isCursorCacheFresh(complete, nowMs, 0));
    // A forward catch-up that stopped short of the time it ran.
    assert.isFalse(isCursorCacheFresh({ ...complete, untilMs: nowMs - DAY_MS }, nowMs, 0));
  });

  it("extends an older range without copying records it did not change", () => {
    const cache = {
      accountKey: "a",
      sinceMs: 5 * DAY_MS,
      untilMs: 10 * DAY_MS,
      fetchedAtMs: 10 * DAY_MS,
      records: [record(6 * DAY_MS)],
    };
    const range = cursorFetchRange(cache, 0, 10 * DAY_MS, DAY_MS);
    assert.deepStrictEqual(range, { sinceMs: 4 * DAY_MS, untilMs: 5 * DAY_MS });
    const empty = mergeCursorFetch(cache, "a", range!, [], 10 * DAY_MS, 0);
    assert.isTrue(empty.changed);
    assert.strictEqual(empty.cache.records, cache.records);
    assert.strictEqual(empty.cache.sinceMs, 4 * DAY_MS);
    const found = mergeCursorFetch(cache, "a", range!, [record(4.5 * DAY_MS)], 10 * DAY_MS, 0);
    assert.deepStrictEqual(
      found.cache.records.map((entry) => entry.timestampMs),
      [4.5 * DAY_MS, 6 * DAY_MS],
    );
  });

  it("widens a narrowed span back to no more than half the width that ran out of time", () => {
    for (const failedWidthMs of [1_000_000_007, 8 * DAY_MS, 92 * DAY_MS + 120_000]) {
      let spanMs = narrowedCursorSpan(failedWidthMs);
      for (let reads = 0; reads < 10; reads++) spanMs = widenedCursorSpan(spanMs, failedWidthMs);
      assert.isAtMost(spanMs, failedWidthMs / 2);
      assert.isAbove(spanMs, failedWidthMs / 4);
      assert.isTrue(Number.isSafeInteger(spanMs));
    }
  });
});
