import { useIsFocused } from "@react-navigation/native";
import { describeHostHardware, type HostHardwareGauge } from "@onicode/client-runtime/hostHardware";
import type {
  EnvironmentId,
  EnvironmentMachineKind,
  HostHardwareSnapshot,
} from "@onicode/contracts";
import { useState } from "react";
import { Platform, useWindowDimensions, View } from "react-native";

import { EnvironmentMachineSymbol } from "../../components/EnvironmentMachineSymbol";
import { SymbolView } from "../../components/ui/AppSymbol";
import { AppText as Text } from "../../components/ui/AppText";
import { ListRowLayout } from "../../components/ui/ListRowLayout";
import { cn } from "../../lib/cn";
import { useEnvironmentQuery } from "../../state/query";
import { serverEnvironment } from "../../state/server";
import { SettingsSection } from "./components/SettingsSection";

const GAUGE_SYMBOLS = { cpu: "cpu", memory: "memorychip", storage: "internaldrive" } as const;

function HardwareGaugeRow({ gauge }: { readonly gauge: HostHardwareGauge }) {
  const { fontScale } = useWindowDimensions();
  const value = (
    <Text
      textRole="footnote"
      numberOfLines={1}
      className={cn(
        "tabular-nums text-foreground-muted",
        gauge.level === "high" && "text-warning-foreground",
        gauge.level === "critical" && "text-danger-foreground",
      )}
    >
      {gauge.value ?? "Usage unavailable"}
    </Text>
  );
  return (
    <View
      accessible
      accessibilityLabel={[gauge.title, gauge.detail, gauge.value].filter(Boolean).join(", ")}
      className="flex-row items-start gap-3 px-4 py-3"
    >
      <View className="size-6 items-center justify-center">
        <SymbolView
          name={GAUGE_SYMBOLS[gauge.kind]}
          size={Platform.OS === "android" ? 24 : 22}
          tintColorClassName="accent-icon"
          type="monochrome"
          weight="regular"
        />
      </View>
      <View className="min-w-0 flex-1 gap-1">
        <View className="flex-row items-baseline justify-between gap-3">
          <Text textRole="body" numberOfLines={1} className="min-w-0 shrink text-foreground">
            {gauge.title}
          </Text>
          {/* Large text leaves the value no room beside the title. */}
          {fontScale >= 1.5 ? null : value}
        </View>
        <Text textRole="footnote" className="text-foreground-muted">
          {gauge.detail}
        </Text>
        {fontScale >= 1.5 ? value : null}
        <View className="mt-1.5 h-1 overflow-hidden rounded-full bg-subtle">
          <View
            className={cn(
              "h-1 rounded-full",
              gauge.level === "critical"
                ? "bg-danger-foreground"
                : gauge.level === "high"
                  ? "bg-warning-foreground"
                  : "bg-foreground-muted",
            )}
            style={{ width: `${Math.round((gauge.fraction ?? 0) * 100)}%` }}
          />
        </View>
      </View>
    </View>
  );
}

export function EnvironmentHardwareSection(props: {
  readonly environmentId: EnvironmentId;
  readonly machine: EnvironmentMachineKind;
}) {
  // The query polls, so it runs only while this screen is the one on top.
  const focused = useIsFocused();
  const { data, error } = useEnvironmentQuery(
    focused
      ? serverEnvironment.hostHardware({ environmentId: props.environmentId, input: {} })
      : null,
  );
  // Keeps the last reading on screen while another screen covers this one.
  const [lastReading, setLastReading] = useState<HostHardwareSnapshot | null>(null);
  if (data !== null && data !== lastReading) setLastReading(data);
  const hardware = data ?? lastReading;
  const view = hardware === null ? null : describeHostHardware(hardware);

  return (
    <SettingsSection title="Hardware">
      {view ? (
        <>
          <ListRowLayout
            leading={
              <EnvironmentMachineSymbol
                kind={props.machine}
                size={Platform.OS === "android" ? 24 : 22}
                tintColorClassName="accent-icon"
              />
            }
            label={view.name}
            description={view.facts.join(" · ")}
          />
          {view.gauges.map((gauge) => (
            <HardwareGaugeRow key={gauge.key} gauge={gauge} />
          ))}
        </>
      ) : (
        <View className="p-4">
          <Text selectable={error !== null} className="text-sm text-foreground-muted">
            {error ?? "Reading hardware…"}
          </Text>
        </View>
      )}
    </SettingsSection>
  );
}
