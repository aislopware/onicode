/** Seconds a scenario with no recorded weight is taken to run: the median of those under 15 s. */
const DEFAULT_WEIGHT = 9;

/**
 * The files of one shard, balanced by run time: longest first, each to the lightest shard so far.
 * e2e's own `--shard` cuts the sorted list into equal counts, which put most slow scenarios in one
 * shard. Every file lands in exactly one shard whether or not it has a weight.
 */
export function shardScenarios(
  files: ReadonlyArray<string>,
  weights: Readonly<Record<string, number>>,
  shard: string,
) {
  const match = /^(\d+)\/(\d+)$/.exec(shard);
  const index = Number(match?.[1]);
  const count = Number(match?.[2]);
  if (!Number.isSafeInteger(index) || !Number.isSafeInteger(count) || index < 1 || index > count) {
    throw new Error("Shard must be index/count with 1 <= index <= count");
  }
  const weight = (file: string) => weights[file] ?? DEFAULT_WEIGHT;
  const shards = Array.from({ length: count }, () => ({ total: 0, files: [] as Array<string> }));
  const ordered = [...new Set(files)].sort(
    (a, b) => weight(b) - weight(a) || (a < b ? -1 : a > b ? 1 : 0),
  );
  for (const file of ordered) {
    const lightest = shards.reduce((best, next) => (next.total < best.total ? next : best));
    lightest.total += weight(file);
    lightest.files.push(file);
  }
  return shards[index - 1]!.files.sort();
}
