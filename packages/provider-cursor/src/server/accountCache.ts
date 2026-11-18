// Occurrence indices are fetch-local; replace entire refetched ranges to prevent key collisions.
import { cursorRateModel } from "./accountUsage.ts";
import type { UsageRecord } from "@onicode/provider-core/server/usage";

export type CursorCredentialSource = { readonly kind: "apiKey"; readonly apiKey: string };

export const CURSOR_ACCOUNT_TTL_MS = 60 * 1000;

/** Dashboard events can be finalized after they were first read. */
const REFETCH_OVERLAP_MS = 60 * 60 * 1000;

export interface CursorAccountCache {
  readonly accountKey: string;
  readonly sinceMs: number;
  readonly untilMs: number;
  readonly fetchedAtMs: number;
  readonly records: readonly UsageRecord[];
}

interface CursorFetchRange {
  readonly sinceMs: number;
  readonly untilMs: number;
}

/** A partial forward read leaves `untilMs` behind the time it ran, so it is not fresh. */
export function isCursorCacheFresh(
  cache: CursorAccountCache | undefined,
  nowMs: number,
  retentionStartMs: number,
): boolean {
  return (
    cache !== undefined &&
    cache.sinceMs <= retentionStartMs &&
    cache.untilMs >= cache.fetchedAtMs &&
    nowMs - cache.fetchedAtMs < CURSOR_ACCOUNT_TTL_MS
  );
}

/** Wider than the refetch overlap, so a forward read keeps all of it and still moves on. */
export const CURSOR_MIN_FETCH_SPAN_MS = 2 * REFETCH_OVERLAP_MS;

/** Reads one refresh may make; a refresh that stops short resumes from the cache next time. */
export const CURSOR_MAX_READS_PER_REFRESH = 8;

/**
 * The next range to read, at most `maxSpanMs` wide, or null when the cache is complete: the newest
 * history first, then forward to now, then older history down to the retention start. Every range
 * touches a cache that still holds retained history, so a merge never drops it.
 */
export function cursorFetchRange(
  cache: CursorAccountCache | undefined,
  retentionStartMs: number,
  nowMs: number,
  maxSpanMs: number,
): CursorFetchRange | null {
  if (cache === undefined || cache.untilMs < retentionStartMs) {
    return { sinceMs: Math.max(retentionStartMs, nowMs - maxSpanMs), untilMs: nowMs };
  }
  if (cache.untilMs < nowMs) {
    const sinceMs = Math.max(retentionStartMs, cache.untilMs - REFETCH_OVERLAP_MS);
    return { sinceMs, untilMs: Math.min(nowMs, sinceMs + maxSpanMs) };
  }
  if (cache.sinceMs > retentionStartMs) {
    return {
      sinceMs: Math.max(retentionStartMs, cache.sinceMs - maxSpanMs),
      untilMs: cache.sinceMs,
    };
  }
  return null;
}

export function mergeCursorFetch(
  cache: CursorAccountCache | undefined,
  accountKey: string,
  range: CursorFetchRange,
  fetched: readonly UsageRecord[],
  nowMs: number,
  retentionStartMs: number,
) {
  const inRange = (record: UsageRecord) =>
    record.timestampMs >= range.sinceMs && record.timestampMs <= range.untilMs;
  const base =
    cache !== undefined && range.sinceMs <= cache.untilMs && range.untilMs >= cache.sinceMs
      ? cache
      : undefined;
  const older = base !== undefined && range.untilMs <= base.sinceMs;
  const previous = base?.records ?? [];
  const added = fetched.filter(inRange);
  const replacedKeys = new Set<string | null>();
  let pruned = false;
  for (const record of previous) {
    if (inRange(record)) replacedKeys.add(record.dedupeKey);
    else if (record.timestampMs < retentionStartMs) pruned = true;
  }
  const recordsChanged =
    pruned ||
    replacedKeys.size !== added.length ||
    added.some((record) => !replacedKeys.has(record.dedupeKey));
  let records = previous;
  if (base === undefined || recordsChanged) {
    const kept = previous.filter(
      (record) => !inRange(record) && record.timestampMs >= retentionStartMs,
    );
    records = older ? added.concat(kept) : kept.concat(added);
  }
  const sinceMs = Math.max(
    retentionStartMs,
    Math.min(base?.sinceMs ?? range.sinceMs, range.sinceMs),
  );
  return {
    // A newer `untilMs` alone is not worth a write: a cache that ends earlier only rereads more.
    changed: base === undefined || recordsChanged || sinceMs !== base.sinceMs,
    cache: {
      accountKey,
      sinceMs,
      untilMs: older ? base.untilMs : range.untilMs,
      fetchedAtMs: nowMs,
      records,
    } satisfies CursorAccountCache,
  };
}

/** Versioned files isolate caches for different server versions sharing a state directory. */
export const CURSOR_ACCOUNT_CACHE_FILE_NAME = "usage-cursor-account-cache-v2.json";
const CURSOR_ACCOUNT_CACHE_VERSION = 2;

type SerializedRecord = readonly [
  timestampMs: number,
  modelIndex: number,
  sessionIndex: number,
  uncachedInputTokens: number,
  cachedInputTokens: number,
  cacheCreationTokens: number,
  outputTokens: number,
  reportedCostUsd: number | null,
  dedupeKey: string | null,
];

interface SerializedAccount {
  readonly accountKey: string;
  readonly sinceMs: number;
  readonly untilMs: number;
  readonly fetchedAtMs: number;
  readonly records: readonly SerializedRecord[];
  /** Widest read learned to finish, so a backfill resumed after a restart need not relearn it. */
  readonly spanMs?: number;
}

export function encodeCursorAccountCaches(
  caches: ReadonlyMap<string, CursorAccountCache>,
  spans: ReadonlyMap<string, number>,
): string {
  const strings: string[] = [];
  const indexes = new Map<string, number>();
  const intern = (value: string) => {
    let index = indexes.get(value);
    if (index === undefined) {
      index = strings.length;
      strings.push(value);
      indexes.set(value, index);
    }
    return index;
  };
  const accounts: Record<string, SerializedAccount> = {};
  for (const [credentialKey, cache] of caches) {
    const spanMs = spans.get(credentialKey);
    accounts[credentialKey] = {
      ...(spanMs === undefined ? {} : { spanMs }),
      accountKey: cache.accountKey,
      sinceMs: cache.sinceMs,
      untilMs: cache.untilMs,
      fetchedAtMs: cache.fetchedAtMs,
      records: cache.records.map((record) => [
        record.timestampMs,
        intern(record.model),
        intern(record.sessionId),
        record.totals.uncachedInputTokens,
        record.totals.cachedInputTokens,
        record.totals.cacheCreationTokens,
        record.totals.outputTokens,
        record.reportedCostUsd,
        record.dedupeKey,
      ]),
    };
  }
  return JSON.stringify({ version: CURSOR_ACCOUNT_CACHE_VERSION, strings, accounts });
}

const isFiniteNumber = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value);

/** Drop a malformed account whole; surviving records would leave gaps in a range marked complete. */
export function decodeCursorAccountCaches(document: unknown): {
  readonly caches: Map<string, CursorAccountCache>;
  readonly spans: Map<string, number>;
} {
  const caches = new Map<string, CursorAccountCache>();
  const spans = new Map<string, number>();
  if (typeof document !== "object" || document === null) return { caches, spans };
  const root = document as {
    readonly version?: unknown;
    readonly strings?: unknown;
    readonly accounts?: unknown;
  };
  const strings = root.strings;
  if (
    root.version !== CURSOR_ACCOUNT_CACHE_VERSION ||
    !Array.isArray(strings) ||
    !strings.every((value): value is string => typeof value === "string") ||
    typeof root.accounts !== "object" ||
    root.accounts === null
  ) {
    return { caches, spans };
  }
  const stringAt = (index: unknown): string | undefined =>
    typeof index === "number" ? strings[index] : undefined;

  const decodeRecord = (row: unknown): UsageRecord | null => {
    if (!Array.isArray(row)) return null;
    const [
      timestampMs,
      modelIndex,
      sessionIndex,
      uncached,
      cached,
      creation,
      output,
      cost,
      key,
    ]: readonly unknown[] = row;
    const model = stringAt(modelIndex);
    const sessionId = stringAt(sessionIndex);
    if (
      !isFiniteNumber(timestampMs) ||
      model === undefined ||
      sessionId === undefined ||
      !isFiniteNumber(uncached) ||
      !isFiniteNumber(cached) ||
      !isFiniteNumber(creation) ||
      !isFiniteNumber(output) ||
      (cost !== null && !isFiniteNumber(cost)) ||
      (key !== null && typeof key !== "string")
    ) {
      return null;
    }
    return {
      provider: "cursor",
      timestampMs,
      model,
      rateModel: cursorRateModel(model),
      sessionId,
      totals: {
        uncachedInputTokens: uncached,
        cachedInputTokens: cached,
        cacheCreationTokens: creation,
        outputTokens: output,
        reasoningTokens: 0,
      },
      reportedCostUsd: cost,
      speed: "standard",
      dedupeKey: key,
    };
  };

  for (const [credentialKey, raw] of Object.entries(root.accounts)) {
    if (typeof raw !== "object" || raw === null) continue;
    const account = raw as Partial<Record<keyof SerializedAccount, unknown>>;
    if (
      typeof account.accountKey !== "string" ||
      !isFiniteNumber(account.sinceMs) ||
      !isFiniteNumber(account.untilMs) ||
      !isFiniteNumber(account.fetchedAtMs) ||
      !Array.isArray(account.records)
    ) {
      continue;
    }
    const records: UsageRecord[] = [];
    for (const row of account.records) {
      const record = decodeRecord(row);
      if (record === null) break;
      records.push(record);
    }
    if (records.length !== account.records.length) continue;
    caches.set(credentialKey, {
      accountKey: account.accountKey,
      sinceMs: account.sinceMs,
      untilMs: account.untilMs,
      fetchedAtMs: account.fetchedAtMs,
      records,
    });
    if (isFiniteNumber(account.spanMs) && account.spanMs >= CURSOR_MIN_FETCH_SPAN_MS) {
      spans.set(credentialKey, account.spanMs);
    }
  }
  return { caches, spans };
}
