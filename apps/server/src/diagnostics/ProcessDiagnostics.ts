import type {
  ResourceTelemetryProcessCategory,
  ServerProcessDiagnosticsEntry,
  ServerProcessDiagnosticsResult,
  ServerProcessSignal,
  ServerSignalProcessResult,
} from "@onicode/contracts";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

import * as ProcessLauncher from "../process/ProcessLauncher.ts";
import * as ResourceTelemetry from "../resourceTelemetry/ResourceTelemetry.ts";

export class ProcessSignalFailed extends Schema.TaggedError<ProcessSignalFailed>()(
  "ProcessSignalFailed",
  {
    pid: Schema.Number,
    signal: Schema.String,
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    return `Failed to signal process ${this.pid} with ${this.signal}.`;
  }
}

export class ProcessDiagnostics extends Context.Service<
  ProcessDiagnostics,
  {
    readonly read: Effect.Effect<ServerProcessDiagnosticsResult>;
    readonly signal: (input: {
      readonly pid: number;
      readonly startTimeMs: number;
      readonly signal: ServerProcessSignal;
    }) => Effect.Effect<ServerSignalProcessResult>;
  }
>()("onicode/diagnostics/ProcessDiagnostics") {}

function formatElapsed(runTimeMs: number): string {
  const totalSeconds = Math.max(0, Math.floor(runTimeMs / 1_000));
  const hours = Math.floor(totalSeconds / 3_600);
  const minutes = Math.floor((totalSeconds % 3_600) / 60);
  const seconds = totalSeconds % 60;
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`
    : `${minutes}:${String(seconds).padStart(2, "0")}`;
}

function canSignalCategory(category: ResourceTelemetryProcessCategory): boolean {
  return (
    category === "server-child" || category === "provider-root" || category === "terminal-root"
  );
}

/** @public */
export const make = Effect.fn("makeProcessDiagnostics")(function* () {
  const telemetry = yield* ResourceTelemetry.ResourceTelemetry;
  const refreshedTelemetry = telemetry.refresh.pipe(Effect.catch(() => telemetry.latest));
  const read: ProcessDiagnostics["Service"]["read"] = refreshedTelemetry.pipe(
    Effect.map((snapshot) => {
      // The Linux launcher is not offered, and what it hosts sits where a direct child would.
      const launchers = new Set(
        snapshot.processes.flatMap((entry) =>
          ProcessLauncher.isLauncher(entry.identity.pid) ? [entry.identity.pid] : [],
        ),
      );
      const parents = new Map(snapshot.processes.map((entry) => [entry.identity.pid, entry.ppid]));
      const hostedDepth = (entry: (typeof snapshot.processes)[number]) => {
        let pid: number | undefined = entry.ppid;
        for (let step = 0; step < entry.depth && pid !== undefined; step++) {
          if (launchers.has(pid)) return 1;
          pid = parents.get(pid);
        }
        return 0;
      };
      const processes = snapshot.processes
        .filter((entry) => canSignalCategory(entry.category) && !launchers.has(entry.identity.pid))
        .map((entry): ServerProcessDiagnosticsEntry => ({
          pid: entry.identity.pid,
          startTimeMs: entry.identity.startTimeMs,
          ppid: entry.ppid,
          pgid: Option.none(),
          status: entry.status || "Unknown",
          cpuPercent: entry.cpuPercent,
          rssBytes: entry.residentBytes,
          elapsed: formatElapsed(entry.runTimeMs),
          command: entry.command || entry.name || "unknown",
          depth: Math.max(0, entry.depth - 1 - hostedDepth(entry)),
          childPids: entry.childPids,
        }));
      return {
        serverPid: process.pid,
        readAt: snapshot.readAt,
        processCount: processes.length,
        totalRssBytes: processes.reduce((total, entry) => total + entry.rssBytes, 0),
        totalCpuPercent: processes.reduce((total, entry) => total + entry.cpuPercent, 0),
        processes,
        error: Option.map(snapshot.health.native.lastError, (message) => ({ message })),
      };
    }),
  );

  const signal: ProcessDiagnostics["Service"]["signal"] = Effect.fn("ProcessDiagnostics.signal")(
    function* (input) {
      if (input.pid === process.pid) {
        return {
          pid: input.pid,
          signal: input.signal,
          signaled: false,
          message: Option.some("Refusing to signal the Onicode server process."),
        };
      }
      // Its commands and agents would all die with it.
      if (ProcessLauncher.isLauncher(input.pid)) {
        return {
          pid: input.pid,
          signal: input.signal,
          signaled: false,
          message: Option.some("Refusing to signal the Onicode process launcher."),
        };
      }
      const current = yield* telemetry.refresh.pipe(Effect.option);
      if (Option.isNone(current)) {
        return {
          pid: input.pid,
          signal: input.signal,
          signaled: false,
          message: Option.some(
            `Could not refresh process ${input.pid}; refusing to signal a stale identity.`,
          ),
        };
      }
      const selected = current.value.processes.find(
        (entry) =>
          entry.identity.pid === input.pid && entry.identity.startTimeMs === input.startTimeMs,
      );
      if (!selected) {
        return {
          pid: input.pid,
          signal: input.signal,
          signaled: false,
          message: Option.some(
            `Process ${input.pid} no longer matches the selected process identity.`,
          ),
        };
      }
      if (!canSignalCategory(selected.category)) {
        return {
          pid: input.pid,
          signal: input.signal,
          signaled: false,
          message: Option.some(
            `Process ${input.pid} is not a signalable Onicode backend descendant.`,
          ),
        };
      }
      return yield* Effect.try({
        try: () => {
          process.kill(input.pid, input.signal);
          return {
            pid: input.pid,
            signal: input.signal,
            signaled: true,
            message: Option.none(),
          };
        },
        catch: (cause) =>
          new ProcessSignalFailed({
            pid: input.pid,
            signal: input.signal,
            cause,
          }),
      }).pipe(
        Effect.catch((error) =>
          Effect.succeed({
            pid: input.pid,
            signal: input.signal,
            signaled: false,
            message: Option.some(
              error instanceof Error ? error.message : "Failed to signal process.",
            ),
          }),
        ),
      );
    },
  );

  return ProcessDiagnostics.of({ read, signal });
});

export const layer = Layer.effect(ProcessDiagnostics, make());
