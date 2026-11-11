import { groupTailnetDevices } from "@onicode/client-runtime/connection";
import type { EnvironmentId, TailnetDevice } from "@onicode/contracts";
import { useRef, useState } from "react";
import { Platform, type TextInputInstance, View } from "react-native";
import { useResolveClassNames } from "uniwind";

import { cn } from "../../lib/cn";
import { StatusPill } from "../../components/StatusPill";
import { AppActivityIndicator } from "../../components/ui/AppActivityIndicator";
import { AppText as Text } from "../../components/ui/AppText";
import { Button } from "../../components/ui/Button";
import { ListRowLayout } from "../../components/ui/ListRowLayout";
import { ListSection } from "../../components/ui/ListSection";
import { RowPressable } from "../../components/ui/RowPressable";
import { SearchField } from "../../components/ui/SearchField";
import { useEnvironments } from "../../state/environments";
import { useEnvironmentQuery } from "../../state/query";
import { tailnetEnvironment, useTailnetDevicesSource } from "../../state/tailnet";
import { signInWithTailnet } from "../../state/use-remote-environment-registry";
import { ConnectionStatusDot } from "./ConnectionStatusDot";
import { connectionTone } from "./connectionTone";

const ONICODE_PORT = 3773;

export interface TailnetPairingPrefill {
  readonly host: string;
  readonly notice: string;
}

type DeviceAction =
  | { readonly kind: "none" }
  | { readonly kind: "signIn"; readonly httpBaseUrl: string; readonly label: string }
  | { readonly kind: "pair"; readonly prefill: TailnetPairingPrefill };

function deviceAction(
  device: TailnetDevice,
  savedIds: ReadonlySet<EnvironmentId>,
  routeFor: EnvironmentId | undefined,
): DeviceAction {
  const onicode = device.onicode;
  const address = device.addresses[0];
  if (!device.online) return { kind: "none" };
  if (onicode === undefined) {
    if (address === undefined) return { kind: "none" };
    return {
      kind: "pair",
      prefill: {
        host: `http://${address}:${ONICODE_PORT}`,
        notice: `Onicode didn't answer on ${device.name}. Enter its pairing code, or change the address.`,
      },
    };
  }
  // Adding a route only takes the environment it is for; a saved one is already here.
  if (
    routeFor === undefined
      ? savedIds.has(onicode.environmentId)
      : onicode.environmentId !== routeFor
  )
    return { kind: "none" };
  if (onicode.tailnetSignIn)
    return { kind: "signIn", httpBaseUrl: onicode.httpBaseUrl, label: onicode.label };
  return {
    kind: "pair",
    prefill: {
      host: onicode.httpBaseUrl,
      notice: `${onicode.label} needs a pairing code. Turn on Tailscale sign-in in that machine's Sharing & access to skip this.`,
    },
  };
}

/** Devices on a connected environment's tailnet, for adding the Onicode servers among them. */
export function TailnetDevicesSection(props: {
  readonly routeFor: EnvironmentId | undefined;
  readonly busy: boolean;
  readonly onSigningIn: (signingIn: boolean) => void;
  readonly onSignedIn: () => void;
  readonly onError: (message: string) => void;
  readonly onNeedsPairing: (prefill: TailnetPairingPrefill) => void;
}) {
  const source = useTailnetDevicesSource();
  const { environments } = useEnvironments();
  const devices = useEnvironmentQuery(
    source === null
      ? null
      : tailnetEnvironment.devices({ environmentId: source.environmentId, input: {} }),
  );
  const searchRef = useRef<TextInputInstance>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [pendingDeviceId, setPendingDeviceId] = useState<string | null>(null);
  if (source === null) return null;

  const savedIds = new Set(environments.map((environment) => environment.environmentId));
  const data = devices.data;

  const signIn = async (device: TailnetDevice, httpBaseUrl: string, label: string) => {
    setPendingDeviceId(device.id);
    props.onSigningIn(true);
    const outcome = await signInWithTailnet(httpBaseUrl, props.routeFor);
    setPendingDeviceId(null);
    props.onSigningIn(false);
    if (outcome.ok) props.onSignedIn();
    else if (outcome.refused)
      props.onNeedsPairing({
        host: httpBaseUrl,
        notice: `${label} didn't accept this device's Tailscale identity. Enter a pairing code from that machine.`,
      });
    else if (outcome.error !== null) props.onError(outcome.error);
  };

  const header = (
    <View className="flex-row items-center justify-between gap-3 pl-4">
      <Text className="min-w-0 flex-1 text-sm font-oni-medium text-foreground-muted">
        Or pick a device on {data?.tailnetName ?? "your tailnet"}
      </Text>
      <Button
        label="Refresh"
        tone="text"
        size="compact"
        loading={devices.isPending}
        onPress={devices.refresh}
      />
    </View>
  );

  if (data === null || !data.available) {
    return (
      <View collapsable={false} className="gap-2">
        {header}
        <Text
          className={cn(
            "px-4 text-sm leading-normal",
            devices.error === null ? "text-foreground-muted" : "text-danger-foreground",
          )}
        >
          {data !== null
            ? `Tailscale isn't running on ${source.label}.`
            : (devices.error ?? `Asking ${source.label} for devices…`)}
        </Text>
      </View>
    );
  }

  const groups = groupTailnetDevices(data.devices, searchQuery);
  return (
    <View collapsable={false} className="gap-2">
      {header}
      <View className="flex-row">
        <SearchField
          autoFocus={false}
          inputRef={searchRef}
          accessibilityLabel="Search devices"
          clearAccessibilityLabel="Clear device search"
          placeholder="Search devices"
          value={searchQuery}
          onChangeText={setSearchQuery}
        />
      </View>
      <View collapsable={false} className="gap-4 pt-2">
        {groups.length === 0 ? (
          <Text className="px-4 text-sm text-foreground-muted">
            {data.devices.length === 0 ? "No devices on this tailnet." : "No devices match."}
          </Text>
        ) : (
          groups.map((group) => (
            <ListSection key={group.key} title={group.title}>
              {group.devices.map((device) => {
                const action = deviceAction(device, savedIds, props.routeFor);
                return (
                  <TailnetDeviceRow
                    key={device.id}
                    device={device}
                    added={
                      device.onicode !== undefined && savedIds.has(device.onicode.environmentId)
                    }
                    loading={pendingDeviceId === device.id}
                    disabled={action.kind === "none" || props.busy}
                    onPress={() => {
                      if (props.busy) return;
                      if (action.kind === "signIn")
                        void signIn(device, action.httpBaseUrl, action.label);
                      else if (action.kind === "pair") props.onNeedsPairing(action.prefill);
                    }}
                  />
                );
              })}
            </ListSection>
          ))
        )}
      </View>
      <Text className="px-4 text-sm leading-normal text-foreground-muted">
        Listed by {source.label}. Machines that allow Tailscale sign-in add without a pairing code.
      </Text>
    </View>
  );
}

const ADDED_TONE = { ...connectionTone("connected"), label: "Added" };
const ONICODE_TONE = {
  label: "Onicode",
  pillClassName: "bg-subtle",
  textClassName: "text-foreground-secondary",
};

function TailnetDeviceRow(props: {
  readonly device: TailnetDevice;
  readonly added: boolean;
  readonly loading: boolean;
  readonly disabled: boolean;
  readonly onPress: () => void;
}) {
  const rippleStyle = useResolveClassNames("bg-subtle-strong");
  const { device } = props;
  const address = device.addresses[0];
  const description = [address, device.os].filter(Boolean).join(" · ");
  const badge = props.added ? ADDED_TONE : device.onicode ? ONICODE_TONE : null;
  return (
    <RowPressable
      accessibilityRole="button"
      accessibilityLabel={[
        device.name,
        description,
        badge?.label,
        device.online ? undefined : "Offline",
      ]
        .filter(Boolean)
        .join(", ")}
      accessibilityState={{ disabled: props.disabled, busy: props.loading }}
      disabled={props.disabled || props.loading}
      onPress={props.onPress}
      android_ripple={{ color: rippleStyle.backgroundColor }}
      unstable_pressDelay={Platform.OS === "android" ? 50 : undefined}
      className="bg-grouped-card"
    >
      <ListRowLayout
        leading={
          <ConnectionStatusDot
            state={device.online ? "connected" : "available"}
            pulse={false}
            size={8}
          />
        }
        label={device.name}
        description={description || undefined}
        disabled={!device.online}
        accessory={
          props.loading ? (
            <AppActivityIndicator colorClassName="accent-icon" />
          ) : badge ? (
            <StatusPill {...badge} size="compact" />
          ) : undefined
        }
      />
    </RowPressable>
  );
}
