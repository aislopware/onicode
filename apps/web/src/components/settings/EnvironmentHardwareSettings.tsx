import { describeHostHardware, type HostHardwareGauge } from "@onicode/client-runtime/hostHardware";
import { resolveEnvironmentMachineKind } from "@onicode/contracts";
import { useGT, useLocale, useMessages } from "gt-react";
import { CpuIcon, HardDriveIcon, MemoryStickIcon } from "lucide-react";

import { cn } from "~/lib/utils";
import type { EnvironmentPresentation } from "~/state/environments";
import { useEnvironmentQuery } from "~/state/query";
import { serverEnvironment } from "~/state/server";
import { EnvironmentRow } from "./EnvironmentRow";
import { SettingsSection } from "./settingsLayout";
import { useSearchableSetting } from "./settingsSearch";

const GAUGE_ICONS = { cpu: CpuIcon, memory: MemoryStickIcon, storage: HardDriveIcon };

function HardwareGauge({ gauge }: { readonly gauge: HostHardwareGauge }) {
  const gt = useGT();
  const Icon = GAUGE_ICONS[gauge.kind];
  const percent = gauge.fraction === null ? null : Math.round(gauge.fraction * 100);
  return (
    <li className="flex items-center gap-x-4 py-2">
      <Icon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
      <div className="min-w-0 flex-1">
        <p className="truncate text-xs font-medium text-foreground">{gauge.title}</p>
        <p className="text-xs text-muted-foreground">{gauge.detail}</p>
      </div>
      <div className="w-36 shrink-0 space-y-1.5">
        <p
          className={cn(
            "truncate text-right text-xs tabular-nums text-muted-foreground",
            gauge.level === "critical" && "text-destructive",
            gauge.level === "high" && "text-warning-foreground",
          )}
        >
          {gauge.value ?? gt("Usage unavailable")}
        </p>
        <div
          role="meter"
          aria-label={gt("{resource} in use", { resource: gauge.title })}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={percent ?? undefined}
          aria-valuetext={gauge.value ?? gt("Unavailable")}
          className="h-1 overflow-hidden rounded-full bg-foreground/10"
        >
          <div
            className={cn(
              "h-full rounded-full transition-[width] duration-300 motion-reduce:transition-none",
              gauge.level === "critical"
                ? "bg-destructive"
                : gauge.level === "high"
                  ? "bg-warning"
                  : "bg-foreground/55",
            )}
            style={{ width: `${percent ?? 0}%` }}
          />
        </div>
      </div>
    </li>
  );
}

function EnvironmentHardware({ environment }: { readonly environment: EnvironmentPresentation }) {
  const gt = useGT();
  const messages = useMessages();
  const locale = useLocale();
  const supported = environment.serverConfig?.environment.capabilities.hostHardware === true;
  const { data, error } = useEnvironmentQuery(
    supported
      ? serverEnvironment.hostHardware({ environmentId: environment.environmentId, input: {} })
      : null,
    messages,
  );
  const view = data === null ? null : describeHostHardware(data, messages, locale);
  return (
    <EnvironmentRow
      kind={resolveEnvironmentMachineKind(environment.serverConfig)}
      label={environment.label}
      subtitle={
        view
          ? [view.name, ...view.facts].join(" · ")
          : !supported
            ? gt("Update Onicode on this machine to see its hardware.")
            : (error ?? gt("Reading hardware…"))
      }
      detail={
        view ? (
          <ul className="mt-1.5 divide-y divide-border">
            {view.gauges.map((gauge) => (
              <HardwareGauge key={gauge.key} gauge={gauge} />
            ))}
          </ul>
        ) : null
      }
    />
  );
}

/** Each connected machine's model, processor, memory, and the volumes Onicode keeps work on. */
export function EnvironmentHardwareSettings({
  environments,
}: {
  readonly environments: ReadonlyArray<EnvironmentPresentation>;
}) {
  const setting = useSearchableSetting();
  const connected = environments.filter(
    (environment) => environment.connection.phase === "connected",
  );
  if (connected.length === 0) return null;
  return (
    <SettingsSection {...setting("environment-hardware")}>
      {connected.map((environment) => (
        <EnvironmentHardware key={environment.environmentId} environment={environment} />
      ))}
    </SettingsSection>
  );
}
