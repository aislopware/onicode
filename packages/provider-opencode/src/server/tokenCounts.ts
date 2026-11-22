interface OpenCodeTokens {
  readonly input: number;
  readonly output: number;
  readonly reasoning: number;
  readonly cache: { readonly read: number; readonly write: number };
}

// OpenCode passes provider counts through, at most checked finite; token schemas take safe
// non-negative integers. A count outside that is zero, and the usage it feeds is inexact.
function tokenCount(value: number): number | undefined {
  const count = Math.round(value);
  return Number.isSafeInteger(count) && count >= 0 ? count : undefined;
}

export const addOpenCodeTokens = (...counts: ReadonlyArray<number>): number =>
  Math.min(
    counts.reduce((total, count) => total + count, 0),
    Number.MAX_SAFE_INTEGER,
  );

export function sanitizeOpenCodeTokens<T extends OpenCodeTokens>(
  tokens: T,
): { readonly tokens: T; readonly exact: boolean } {
  const counts = [
    tokens.input,
    tokens.output,
    tokens.reasoning,
    tokens.cache.read,
    tokens.cache.write,
  ].map(tokenCount);
  const [input = 0, output = 0, reasoning = 0, read = 0, write = 0] = counts;
  return {
    tokens: { ...tokens, input, output, reasoning, cache: { ...tokens.cache, read, write } },
    exact: counts.every((count) => count !== undefined),
  };
}

/** A model limit that can be a context window: a positive safe integer. */
export function openCodeContextWindow(limit: number | undefined): number | undefined {
  const count = limit === undefined ? undefined : tokenCount(limit);
  return count === undefined || count === 0 ? undefined : count;
}
