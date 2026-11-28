import { type HostPressureKind, severeHostPressureKinds } from "@onicode/contracts";
import type { useGT } from "gt-react-native";

import type { HostPressureAlert, StrainedHost } from "../../state/presentation";

type GT = ReturnType<typeof useGT>;

/** One title however many machines are strained, for the header's warning pill. */
export function hostPressureAlertTitle(gt: GT, alert: HostPressureAlert): string {
  const only = alert.strained.length === 1 ? alert.strained[0]! : null;
  return only === null
    ? machinesTitle(gt, alert.strained)
    : kindTitle(gt, severeHostPressureKinds(only.pressure)[0]!, alert.named ? only.label : null);
}

/** What the first strained machine is short of, for the pill's symbol. */
export function hostPressureAlertKind(alert: HostPressureAlert): "memory" | "disk" | "cpu" {
  return titleGroup(severeHostPressureKinds(alert.strained[0]!.pressure)[0]!);
}

/** Overload covers both CPU and storage stalls, which share one title. */
const titleGroup = (kind: HostPressureKind) => (kind === "io" ? "cpu" : kind);

export function machinesTitle(gt: GT, strained: ReadonlyArray<StrainedHost>): string {
  const count = strained.length;
  const groups = new Set(
    strained.map((entry) => titleGroup(severeHostPressureKinds(entry.pressure)[0]!)),
  );
  if (groups.size > 1) return gt("{count} machines need attention", { count });
  switch ([...groups][0]!) {
    case "memory":
      return gt("Memory is slowing agents on {count} machines", { count });
    case "disk":
      return gt("{count} machines are almost out of disk space", { count });
    default:
      return gt("{count} machines are overloaded", { count });
  }
}

export function kindTitle(gt: GT, kind: HostPressureKind, name: string | null): string {
  switch (kind) {
    case "memory":
      return name === null
        ? gt("Memory is slowing agents down")
        : gt("Memory is slowing agents on {name}", { name });
    case "disk":
      return name === null
        ? gt("Almost out of disk space")
        : gt("{name} is almost out of disk space", { name });
    case "cpu":
    case "io":
      return name === null ? gt("Machine overloaded") : gt("{name} is overloaded", { name });
  }
}
