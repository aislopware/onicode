import type {
  EnvironmentId,
  HostHardwareSnapshot,
  HostHardwareVolumeUse,
} from "@onicode/contracts";

export type HostHardwareLevel = "normal" | "high" | "critical";

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

function plural(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

function formatUptime(seconds: number): string {
  if (seconds >= 86_400) return `Up ${plural(Math.floor(seconds / 86_400), "day")}`;
  if (seconds >= 3_600) return `Up ${plural(Math.floor(seconds / 3_600), "hour")}`;
  return `Up ${plural(Math.max(1, Math.floor(seconds / 60)), "minute")}`;
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

function usedFraction(total: number, available: number): number | null {
  return total > 0 ? Math.min(1, Math.max(0, 1 - available / total)) : null;
}

function describeCores(hardware: HostHardwareSnapshot): string {
  const { logicalCores, physicalCores, coreGroups } = hardware.cpu;
  const cores = physicalCores ?? logicalCores;
  const parts = [plural(cores, "core")];
  if (coreGroups.length > 0) {
    parts[0] += ` (${coreGroups.map((group) => `${group.cores} ${group.name.toLowerCase()}`).join(", ")})`;
  } else if (physicalCores !== null && logicalCores > physicalCores) {
    parts.push(plural(logicalCores, "thread"));
  }
  // A container quota or CPU affinity leaves the server fewer CPUs than the machine has.
  const usable = hardware.usage.cpuCount;
  if (usable > 0 && usable < logicalCores) parts.push(`${usable} usable`);
  return parts.join(", ");
}

const VOLUME_USE_LABELS: Record<HostHardwareVolumeUse, string> = {
  home: "home",
  data: "Onicode data",
  worktrees: "worktrees",
  projects: "projects",
};

function describeVolumeUses(uses: ReadonlyArray<HostHardwareVolumeUse>): string {
  const text = uses.map((use) => VOLUME_USE_LABELS[use]).join(", ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** One layout for desktop and mobile: the machine's name, a few facts, and a gauge per resource. */
export function describeHostHardware(hardware: HostHardwareSnapshot): HostHardwareView {
  const { usage } = hardware;
  const name = hardware.model ?? hardware.hostname ?? hardware.os;
  const facts = [
    hardware.os,
    hardware.hostname,
    hardware.uptimeSeconds > 0 ? formatUptime(hardware.uptimeSeconds) : null,
  ].filter((fact): fact is string => fact !== null && fact !== name);

  const memoryTotal = usage.totalMemoryBytes > 0 ? usage.totalMemoryBytes : hardware.memoryBytes;
  const memoryFraction =
    usage.totalMemoryBytes > 0
      ? usedFraction(usage.totalMemoryBytes, usage.availableMemoryBytes)
      : null;
  // A container's memory limit shows as a smaller usable total; sizes round, so allow 1%.
  const memoryLimited = memoryTotal < hardware.memoryBytes * 0.99;

  const gauges: Array<HostHardwareGauge> = [
    {
      kind: "cpu",
      key: "cpu",
      title: hardware.cpu.model ?? "Processor",
      detail: describeCores(hardware),
      value:
        usage.cpuUtilization === null ? null : `${Math.round(usage.cpuUtilization * 100)}% in use`,
      fraction: usage.cpuUtilization,
      // Agents and builds saturate the processor for minutes at a time; that is not a warning.
      level: "normal",
    },
    {
      kind: "memory",
      key: "memory",
      title: `${formatCapacity(hardware.memoryBytes)} memory`,
      detail:
        memoryFraction === null
          ? "Usage unavailable"
          : `${formatCapacity(memoryTotal - usage.availableMemoryBytes)} used${
              memoryLimited ? ` of ${formatCapacity(memoryTotal)} usable` : ""
            }`,
      value:
        memoryFraction === null ? null : `${formatCapacity(usage.availableMemoryBytes)} available`,
      fraction: memoryFraction,
      level: lowLevel(usage.totalMemoryBytes, usage.availableMemoryBytes, LOW_LIMITS.memory),
    },
    ...hardware.volumes.map((volume): HostHardwareGauge => {
      const fraction = usedFraction(volume.totalBytes, volume.availableBytes);
      const used = `${formatCapacity(volume.totalBytes - volume.availableBytes)} used`;
      return {
        kind: "storage",
        key: `storage:${volume.mountPoint}`,
        title: `${formatCapacity(volume.totalBytes)} storage`,
        // One volume needs no name; several are told apart by where they mount and what they hold.
        detail:
          hardware.volumes.length === 1
            ? used
            : `${used} · ${volume.mountPoint} · ${describeVolumeUses(volume.uses)}`,
        value: `${formatCapacity(volume.availableBytes)} available`,
        fraction,
        level: lowLevel(volume.totalBytes, volume.availableBytes, LOW_LIMITS.storage),
      };
    }),
  ];

  return { name, facts, gauges };
}

export interface HostHardwareAlert {
  /** Names what is low and how badly; the same warning keeps the same key. */
  readonly key: string;
  readonly level: Exclude<HostHardwareLevel, "normal">;
  /** The resource in the worst state, for the icon. */
  readonly kind: "memory" | "storage";
  readonly title: string;
  readonly description: string;
  /** The machine in the worst state. */
  readonly environmentId: EnvironmentId;
}

/** What to warn about across the connected machines: memory or storage that is running out. */
export function describeHostHardwareAlert(
  machines: ReadonlyArray<{
    readonly environmentId: EnvironmentId;
    readonly label: string;
    readonly hardware: HostHardwareSnapshot;
  }>,
): HostHardwareAlert | null {
  const low = machines
    .map((machine) => ({
      ...machine,
      gauges: describeHostHardware(machine.hardware)
        .gauges.filter(
          (gauge): gauge is HostHardwareGauge & { readonly kind: "memory" | "storage" } =>
            gauge.kind !== "cpu" && gauge.level !== "normal",
        )
        // `filter` returned a new array; Hermes has no `toSorted`.
        .sort((a, b) => Number(b.level === "critical") - Number(a.level === "critical")),
    }))
    .filter((machine) => machine.gauges.length > 0)
    .sort(
      (a, b) =>
        Number(b.gauges[0]!.level === "critical") - Number(a.gauges[0]!.level === "critical"),
    );
  const worst = low[0];
  if (worst === undefined) return null;

  const kinds = (["memory", "storage"] as const).filter((kind) =>
    low.some((machine) => machine.gauges.some((gauge) => gauge.kind === kind)),
  );
  const readings = (machine: (typeof low)[number]) =>
    machine.gauges.map((gauge) => `${gauge.value} of ${gauge.title}`).join(", ");
  return {
    key: low
      .flatMap((machine) =>
        machine.gauges.map((gauge) => `${machine.environmentId}:${gauge.key}:${gauge.level}`),
      )
      .join("|"),
    level: worst.gauges[0]!.level === "critical" ? "critical" : "high",
    kind: worst.gauges[0]!.kind,
    title: `Low ${kinds.join(" and ")} on ${low.length === 1 ? worst.label : plural(low.length, "machine")}`,
    description:
      low.length === 1
        ? readings(worst)
        : low.map((machine) => `${machine.label}: ${readings(machine)}`).join("\n"),
    environmentId: worst.environmentId,
  };
}
