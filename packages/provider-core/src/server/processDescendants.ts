import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Schedule from "effect/Schedule";
import * as Semaphore from "effect/Semaphore";

import { signalProcessGroup } from "./processGroup.ts";

export interface ProcessTableRow {
  readonly pid: number;
  readonly ppid: number;
  readonly pgid: number;
  readonly started: string;
}

export interface ProcessDescendantTracker {
  readonly tracked: ReadonlyMap<string, ProcessTableRow>;
  readonly refresh: Effect.Effect<void>;
  /** Polls this root process from now until its turn settles. */
  readonly work: () => void;
  readonly settle: Effect.Effect<void>;
  /** Reads the table after the root process exited: what it left that is still the same process. */
  readonly leftovers: Effect.Effect<ReadonlyArray<ProcessTableRow>>;
}

export interface ProcessDescendantPoll {
  readonly poll: Effect.Effect<void>;
  readonly track: (rootPid: number) => ProcessDescendantTracker;
  readonly hasLive: () => boolean;
}

export interface SharedProcessDescendantTracker {
  readonly tracked: ReadonlyMap<string, ProcessTableRow>;
  readonly refresh: Effect.Effect<void>;
  readonly work: () => void;
  readonly settle: Effect.Effect<void>;
  readonly stop: Effect.Effect<void>;
}

export interface SharedProcessDescendantPoll {
  readonly poll: Effect.Effect<void>;
  readonly track: (
    rootPid: number,
    signal?: (pid: number, signal: NodeJS.Signals) => void,
  ) => SharedProcessDescendantTracker;
  readonly hasLive: () => boolean;
}

// How often a working process's tree is recorded when no OS scope stops what it leaves behind.
const DEFAULT_POLL_INTERVAL = "1 second";

export const PROCESS_TABLE_PS_ARGS = ["-A", "-o", "pid=,ppid=,pgid=,lstart="] as const;

/** Parses `ps -A -o pid=,ppid=,pgid=,lstart=`; pid plus start time tells a process from a reused pid. */
export function parseProcessTable(table: string): ReadonlyArray<ProcessTableRow> {
  return table.split("\n").flatMap((line) => {
    const [pid, ppid, pgid, ...started] = line.trim().split(/\s+/);
    const row = {
      pid: Number(pid),
      ppid: Number(ppid),
      pgid: Number(pgid),
      started: started.join(" "),
    };
    return Number.isSafeInteger(row.pid) &&
      Number.isSafeInteger(row.ppid) &&
      Number.isSafeInteger(row.pgid) &&
      row.started
      ? [row]
      : [];
  });
}

const defaultSignalProcess = (pid: number, signal: NodeJS.Signals): void => {
  if (pid < 0) signalProcessGroup(-pid, signal);
  else process.kill(pid, signal);
};

// A job leading its own group takes the processes it started since with it.
export function killProcessDescendant(
  row: ProcessTableRow,
  signal: (pid: number, signal: NodeJS.Signals) => void = defaultSignalProcess,
): void {
  if (!Number.isSafeInteger(row.pid) || row.pid <= 1) return;
  try {
    if (row.pgid === row.pid) signal(-row.pid, "SIGKILL");
    else signal(row.pid, "SIGKILL");
  } catch {}
}

function processDescendants(
  rows: ReadonlyArray<ProcessTableRow>,
  rootPids: ReadonlyArray<number>,
): ReadonlyArray<ProcessTableRow> {
  const children = Map.groupBy(rows, (row) => row.ppid);
  const found: Array<ProcessTableRow> = [];
  const seen = new Set<number>(rootPids);
  const pending = [...rootPids];
  for (let parent = pending.pop(); parent !== undefined; parent = pending.pop()) {
    for (const child of children.get(parent) ?? []) {
      if (seen.has(child.pid)) continue;
      seen.add(child.pid);
      found.push(child);
      pending.push(child.pid);
    }
  }
  return found;
}

const processIdentity = (row: ProcessTableRow) => `${row.pid} ${row.started}`;

/** What one root process started, by pid and start time, recorded from each process table read while it runs. */
function makeProcessDescendantTracker(rootPid: number) {
  const tracked = new Map<string, ProcessTableRow>();
  // After exit the root's pid may belong to an unrelated process, so only tracked jobs are roots.
  const record = (rows: ReadonlyArray<ProcessTableRow>, rootRunning: boolean) => {
    const running = new Set(rows.map(processIdentity));
    // `ps -A` lists itself, so no rows is a failed read.
    if (rows.length === 0) return running;
    const roots = rootRunning ? [rootPid] : [];
    for (const [key, row] of tracked) {
      if (running.has(key)) roots.push(row.pid);
      else tracked.delete(key);
    }
    for (const row of processDescendants(rows, roots)) tracked.set(processIdentity(row), row);
    return running;
  };
  return { tracked: tracked as ReadonlyMap<string, ProcessTableRow>, record };
}

/**
 * Process table reads for every working root process. A poll reads the table once for all of them,
 * so the `ps` cost does not grow with open sessions. Jobs start only while a process works, so an
 * idle process is left out of polls; its turn ends with one last read.
 */
export function makeProcessDescendantPoll(
  readProcessTable: Effect.Effect<ReadonlyArray<ProcessTableRow>>,
): ProcessDescendantPoll {
  const live = new Set<ReturnType<typeof makeProcessDescendantTracker>>();
  // Polls, session closes and exits read one at a time, so each table is newer than all recorded,
  // and a pid and start time it no longer lists never run again.
  const reading = Semaphore.makeUnsafe(1);
  const read = <A>(apply: (rows: ReadonlyArray<ProcessTableRow>) => A) =>
    reading.withPermits(1)(readProcessTable.pipe(Effect.map(apply)));
  return {
    // A table read before a process started may list an older process under its pid, so a poll
    // records only into the processes working when its read began.
    poll: Effect.suspend(() =>
      live.size === 0
        ? Effect.void
        : reading
            .withPermits(1)(
              Effect.suspend(() => {
                const working = [...live];
                return readProcessTable.pipe(
                  Effect.map((rows) => {
                    for (const tracker of working)
                      if (live.has(tracker)) tracker.record(rows, true);
                  }),
                );
              }),
            )
            .pipe(Effect.withSpan("ProcessDescendants.poll")),
    ),
    track: (rootPid: number) => {
      const tracker = makeProcessDescendantTracker(rootPid);
      let exited = false;
      const refresh = Effect.suspend(() =>
        exited
          ? Effect.void
          : read((rows) => {
              if (!exited) tracker.record(rows, true);
            }),
      );
      return {
        tracked: tracker.tracked,
        refresh,
        work: () => {
          if (!exited) live.add(tracker);
        },
        settle: Effect.suspend(() => {
          live.delete(tracker);
          return exited ? Effect.void : refresh;
        }),
        leftovers: Effect.sync(() => {
          exited = true;
          live.delete(tracker);
        }).pipe(
          Effect.andThen(
            read((rows) => {
              const running = tracker.record(rows, false);
              return [...tracker.tracked.values()].filter((row) =>
                running.has(processIdentity(row)),
              );
            }),
          ),
        ),
      };
    },
    hasLive: () => live.size > 0,
  };
}

/**
 * Shares one periodic process-table poll fiber across every tracked root process. The first
 * working tracker starts it; it stops once no tracker is live.
 */
export function makeSharedProcessDescendantPoll(
  readProcessTable: Effect.Effect<ReadonlyArray<ProcessTableRow>>,
  runFork: <A, E>(effect: Effect.Effect<A, E>) => Fiber.Fiber<A, E>,
  interval: Duration.Input = DEFAULT_POLL_INTERVAL,
): SharedProcessDescendantPoll {
  const descendantPoll = makeProcessDescendantPoll(readProcessTable);
  let polling: Fiber.Fiber<unknown> | undefined;
  const startPolling = () => {
    polling ??= runFork(
      descendantPoll.poll.pipe(
        Effect.repeat({
          schedule: Schedule.spaced(interval),
          // Cleared in the same step that ends the poll, so the next tracked process starts a new one.
          while: () => {
            if (descendantPoll.hasLive()) return true;
            polling = undefined;
            return false;
          },
        }),
      ),
    );
  };
  return {
    poll: descendantPoll.poll,
    hasLive: descendantPoll.hasLive,
    track: (rootPid, signal) => {
      const tracker = descendantPoll.track(rootPid);
      const work = () => {
        tracker.work();
        startPolling();
      };
      const stop = tracker.leftovers.pipe(
        Effect.map((rows) => {
          for (const row of rows) killProcessDescendant(row, signal);
        }),
      );
      return {
        tracked: tracker.tracked,
        refresh: tracker.refresh,
        work,
        settle: tracker.settle,
        stop,
      };
    },
  };
}
