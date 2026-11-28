import type {
  HostHardwareSnapshot,
  HostHardwareVolumeUse,
  HostResourcesSnapshot,
} from "@onicode/contracts";
import { mFallback, msg } from "gt-i18n";
import type { MFunctionType } from "gt-i18n/types";

import { formatConjunctionList } from "./listFormat.ts";

type HostHardwareLevel = "normal" | "high" | "critical";

export interface HostHardwareGauge {
  readonly kind: "cpu" | "memory" | "storage";
  /** Unique within one view. */
  readonly key: string;
  readonly title: string;
  readonly detail: string;
  /** What is left, such as "175 GB available"; null when the server could not measure it. */
  readonly value: string | null;
  /** The share in use, 0 to 1. */
  readonly fraction: number | null;
  readonly level: HostHardwareLevel;
}

export interface HostHardwareView {
  readonly name: string;
  readonly facts: ReadonlyArray<string>;
  readonly gauges: ReadonlyArray<HostHardwareGauge>;
}

const UNITS = ["B", "KB", "MB", "GB", "TB", "PB"] as const;

/** Binary units under their everyday names, without trailing zeros: 48 GB, 16.8 GB, 1.82 TB. */
export function formatCapacity(bytes: number): string {
  let value = Math.max(0, bytes);
  let unit = 0;
  const rounded = () => Number(value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2));
  // Pick the unit after rounding, so 1023.9 GB reads "1 TB", not "1024 GB".
  while (rounded() >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${rounded()} ${UNITS[unit]}`;
}

function capitalize(text: string, locale: string): string {
  return text.charAt(0).toLocaleUpperCase(locale) + text.slice(1);
}

function formatUptime(seconds: number, resolve: MFunctionType): string {
  if (seconds >= 86_400) {
    return resolve(
      msg("{count, plural, one {Up {count} day} other {Up {count} days}}", {
        count: Math.floor(seconds / 86_400),
      }),
    );
  }
  if (seconds >= 3_600) {
    return resolve(
      msg("{count, plural, one {Up {count} hour} other {Up {count} hours}}", {
        count: Math.floor(seconds / 3_600),
      }),
    );
  }
  return resolve(
    msg("{count, plural, one {Up {count} minute} other {Up {count} minutes}}", {
      count: Math.max(1, Math.floor(seconds / 60)),
    }),
  );
}

const GiB = 1024 ** 3;

interface LowLimit {
  readonly share: number;
  readonly capBytes: number;
}

/** A share alone would call a 1 TB disk low with 100 GB free, so each limit is capped in bytes. */
const LOW_LIMITS = {
  memory: {
    high: { share: 0.1, capBytes: 4 * GiB },
    critical: { share: 0.05, capBytes: 2 * GiB },
  },
  storage: {
    high: { share: 0.1, capBytes: 25 * GiB },
    critical: { share: 0.05, capBytes: 10 * GiB },
  },
} as const;

function lowLevel(
  totalBytes: number,
  availableBytes: number,
  limits: { readonly high: LowLimit; readonly critical: LowLimit },
): HostHardwareLevel {
  if (totalBytes <= 0) return "normal";
  const below = (limit: LowLimit) =>
    availableBytes < Math.min(totalBytes * limit.share, limit.capBytes);
  return below(limits.critical) ? "critical" : below(limits.high) ? "high" : "normal";
}

/** Runnable tasks per logical CPU; above 1, work waits for a processor. */
const LOAD_LIMITS = { high: 1.5, critical: 3 } as const;

/**
 * The lower of the 1- and 5-minute averages, so a build that fills every core for a minute or
 * two raises nothing, and a warning clears within a minute or so of the load passing.
 */
function sustainedLoad(usage: HostResourcesSnapshot): number | null {
  const load = usage.loadAverage;
  return load === undefined ? null : Math.min(load.oneMinute, load.fiveMinutes);
}

function loadLevel(load: number | null, cpus: number): HostHardwareLevel {
  if (load === null || cpus <= 0) return "normal";
  const perCpu = load / cpus;
  return perCpu >= LOAD_LIMITS.critical
    ? "critical"
    : perCpu >= LOAD_LIMITS.high
      ? "high"
      : "normal";
}

function formatLoad(load: number): string {
  return String(Number(load.toFixed(load >= 10 ? 0 : 1)));
}

function usedFraction(total: number, available: number): number | null {
  return total > 0 ? Math.min(1, Math.max(0, 1 - available / total)) : null;
}

function describeCores(
  hardware: HostHardwareSnapshot,
  resolve: MFunctionType,
  locale: string,
): string {
  const { logicalCores, physicalCores, coreGroups } = hardware.cpu;
  const cores = physicalCores ?? logicalCores;
  // A container quota or CPU affinity leaves the server fewer CPUs than the machine has.
  const usableCpus = hardware.usage.cpuCount;
  const usable = usableCpus > 0 && usableCpus < logicalCores ? usableCpus : null;
  if (coreGroups.length > 0) {
    const groups = formatConjunctionList(
      coreGroups.map((group) =>
        resolve(
          msg(
            "{name, select, performance {{cores} performance} efficiency {{cores} efficiency} other {{cores} {name}}}",
            {
              name: group.name.toLowerCase(),
              cores: group.cores,
              $context: "cores of one kind in a hybrid CPU, such as 10 performance",
            },
          ),
        ),
      ),
      locale,
      resolve,
    );
    return usable === null
      ? resolve(
          msg("{cores, plural, one {{cores} core ({groups})} other {{cores} cores ({groups})}}", {
            cores,
            groups,
          }),
        )
      : resolve(
          msg(
            "{cores, plural, one {{cores} core ({groups}), {usable} usable} other {{cores} cores ({groups}), {usable} usable}}",
            { cores, groups, usable, $context: "usable: CPUs this server may use" },
          ),
        );
  }
  if (physicalCores !== null && logicalCores > physicalCores) {
    return usable === null
      ? resolve(
          msg(
            "{cores, plural, one {{cores} core, {threads, plural, one {{threads} thread} other {{threads} threads}}} other {{cores} cores, {threads, plural, one {{threads} thread} other {{threads} threads}}}}",
            { cores, threads: logicalCores },
          ),
        )
      : resolve(
          msg(
            "{cores, plural, one {{cores} core, {threads, plural, one {{threads} thread} other {{threads} threads}}, {usable} usable} other {{cores} cores, {threads, plural, one {{threads} thread} other {{threads} threads}}, {usable} usable}}",
            {
              cores,
              threads: logicalCores,
              usable,
              $context: "usable: CPUs this server may use",
            },
          ),
        );
  }
  return usable === null
    ? resolve(msg("{cores, plural, one {{cores} core} other {{cores} cores}}", { cores }))
    : resolve(
        msg(
          "{cores, plural, one {{cores} core, {usable} usable} other {{cores} cores, {usable} usable}}",
          { cores, usable, $context: "usable: CPUs this server may use" },
        ),
      );
}

const VOLUME_USE_LABELS: Record<HostHardwareVolumeUse, string> = {
  home: msg("home", { $context: "what a disk holds: the home folder" }),
  data: msg("Onicode data", { $context: "what a disk holds" }),
  worktrees: msg("worktrees", { $context: "what a disk holds" }),
  projects: msg("projects", { $context: "what a disk holds" }),
};

function describeVolumeUses(
  uses: ReadonlyArray<HostHardwareVolumeUse>,
  resolve: MFunctionType,
  locale: string,
): string {
  return capitalize(
    formatConjunctionList(
      uses.map((use) => resolve(VOLUME_USE_LABELS[use])),
      locale,
      resolve,
    ),
    locale,
  );
}

/** One layout for desktop and mobile: the machine's name, a few facts, and a gauge per resource. */
export function describeHostHardware(
  hardware: HostHardwareSnapshot,
  resolve: MFunctionType = mFallback,
  locale = "en",
): HostHardwareView {
  const { usage } = hardware;
  const name = hardware.model ?? hardware.hostname ?? hardware.os;
  const facts = [
    hardware.os,
    hardware.hostname,
    hardware.uptimeSeconds > 0 ? formatUptime(hardware.uptimeSeconds, resolve) : null,
  ].filter((fact): fact is string => fact !== null && fact !== name);

  const memoryTotal = usage.totalMemoryBytes > 0 ? usage.totalMemoryBytes : hardware.memoryBytes;
  const memoryFraction =
    usage.totalMemoryBytes > 0
      ? usedFraction(usage.totalMemoryBytes, usage.availableMemoryBytes)
      : null;
  // A container's memory limit shows as a smaller usable total; sizes round, so allow 1%.
  const memoryLimited = memoryTotal < hardware.memoryBytes * 0.99;

  // The load average counts the whole machine, so it is measured against all of its CPUs.
  const load = sustainedLoad(usage);
  const loadCpus = hardware.cpu.logicalCores;
  const cpuLevel = loadLevel(load, loadCpus);
  const memoryLevel = lowLevel(
    usage.totalMemoryBytes,
    usage.availableMemoryBytes,
    LOW_LIMITS.memory,
  );
  const memoryUsed = formatCapacity(memoryTotal - usage.availableMemoryBytes);

  const gauges: Array<HostHardwareGauge> = [
    {
      kind: "cpu",
      key: "cpu",
      title: hardware.cpu.model ?? resolve(msg("Processor", { $context: "hardware gauge title" })),
      detail: [
        describeCores(hardware, resolve, locale),
        load === null
          ? null
          : resolve(msg("Load {load}", { load: formatLoad(load), $context: "CPU load average" })),
      ]
        .filter((part) => part !== null)
        .join(" · "),
      value:
        usage.cpuUtilization === null
          ? null
          : resolve(msg("{percent}% in use", { percent: Math.round(usage.cpuUtilization * 100) })),
      // Busy is fine; agents and builds keep every core busy. Work queueing for them is not.
      fraction: usage.cpuUtilization,
      level: cpuLevel,
    },
    {
      kind: "memory",
      key: "memory",
      title: resolve(msg("{capacity} memory", { capacity: formatCapacity(hardware.memoryBytes) })),
      detail:
        memoryFraction === null
          ? resolve(msg("Usage unavailable"))
          : memoryLimited
            ? resolve(
                msg("{used} used of {usable} usable", {
                  used: memoryUsed,
                  usable: formatCapacity(memoryTotal),
                }),
              )
            : resolve(msg("{used} used", { used: memoryUsed })),
      value:
        memoryFraction === null
          ? null
          : resolve(
              msg("{capacity} available", {
                capacity: formatCapacity(usage.availableMemoryBytes),
              }),
            ),
      fraction: memoryFraction,
      level: memoryLevel,
    },
    ...hardware.volumes.map((volume): HostHardwareGauge => {
      const fraction = usedFraction(volume.totalBytes, volume.availableBytes);
      const used = resolve(
        msg("{used} used", { used: formatCapacity(volume.totalBytes - volume.availableBytes) }),
      );
      const total = formatCapacity(volume.totalBytes);
      const available = formatCapacity(volume.availableBytes);
      const level = lowLevel(volume.totalBytes, volume.availableBytes, LOW_LIMITS.storage);
      return {
        kind: "storage",
        key: `storage:${volume.mountPoint}`,
        title: resolve(msg("{capacity} storage", { capacity: total })),
        // One volume needs no name; several are told apart by where they mount and what they hold.
        detail:
          hardware.volumes.length === 1
            ? used
            : `${used} · ${volume.mountPoint} · ${describeVolumeUses(volume.uses, resolve, locale)}`,
        value: resolve(msg("{capacity} available", { capacity: available })),
        fraction,
        level,
      };
    }),
  ];

  return { name, facts, gauges };
}
