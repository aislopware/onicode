import { useAtomValue } from "@effect/atom-react";
import type { EnvironmentId } from "@onicode/contracts";
import { createContext, use, useEffect, useState, type ReactNode } from "react";
import { Animated, Platform, Pressable, View } from "react-native";
import { useGT, useMessages } from "gt-react-native";

import { AppActivityIndicator } from "../../components/ui/AppActivityIndicator";
import { SymbolView } from "../../components/ui/AppSymbol";
import { AppText as Text } from "../../components/ui/AppText";
import { useAndroidControlSizing } from "../../components/ui/useAndroidControlSizing";
import { useDeadlinePassed } from "../../lib/clock";
import { type HostPressureAlert, hostPressureAlertAtom } from "../../state/presentation";
import { useWorkspaceState } from "../../state/workspace";
import { hostPressureAlertKind, hostPressureAlertTitle } from "./HostPressureBanner";
import {
  workspaceConnectionStatusPresentation,
  type WorkspaceConnectionStatusPresentation,
} from "./workspace-connection-status";

const STATUS_SHOW_DELAY_MS = 800;
const FADE_IN_MS = 250;

type ConnectionStatus = WorkspaceConnectionStatusPresentation & {
  /** Only while the status first appears. A header that shows it later shows it at once. */
  readonly fadeIn: boolean;
};

const ALERT_SYMBOLS = { cpu: "cpu", memory: "memorychip", disk: "internaldrive" } as const;

const ConnectionStatusContext = createContext<ConnectionStatus | null>(null);
/** Set while a connected machine is short of memory or disk, or overloaded. */
const HostPressureAlertContext = createContext<HostPressureAlert | null>(null);

/**
 * The delay hides brief reconnects, so it is the workspace's, not each header's: every header
 * shows the status from the same moment. Headers read it from context instead of subscribing
 * themselves, because a hidden Android tab drops its subscriptions and showed a stale status for
 * a moment when it came back, while context still reaches it.
 */
export function WorkspaceConnectionStatusProvider(props: { readonly children: ReactNode }) {
  const { state } = useWorkspaceState();
  const m = useMessages();
  const presentation = workspaceConnectionStatusPresentation(state, m);
  const hasStatus = presentation !== null;
  // When the current status run began; a gap in the status restarts the delay.
  const [statusSince, setStatusSince] = useState(() => (hasStatus ? Date.now() : null));
  if (hasStatus !== (statusSince !== null)) {
    // oxlint-disable-next-line react/purity -- Reads the clock only in the render-phase adjustment that runs when the status appears.
    setStatusSince(hasStatus ? Date.now() : null);
  }
  const since = hasStatus ? statusSince : null;
  const fadeStarted = useDeadlinePassed(since === null ? null : since + STATUS_SHOW_DELAY_MS);
  const fadeEnded = useDeadlinePassed(
    since === null ? null : since + STATUS_SHOW_DELAY_MS + FADE_IN_MS,
  );

  const hostPressureAlert = useAtomValue(hostPressureAlertAtom);
  const label = presentation?.label;
  const showsProgress = presentation?.showsProgress ?? false;
  const status =
    label === undefined || since === null || !fadeStarted
      ? null
      : { label, showsProgress, fadeIn: !fadeEnded };
  return (
    <ConnectionStatusContext value={status}>
      <HostPressureAlertContext value={hostPressureAlert}>
        {props.children}
      </HostPressureAlertContext>
    </ConnectionStatusContext>
  );
}

/** Native-driver animation blanks views re-hosted in RNSScreenStackHeaderSubview; use the JS style path. */
function StatusFadeIn(props: {
  readonly children: ReactNode;
  readonly animate: boolean;
  readonly grow?: boolean;
  readonly maxWidth?: number;
}) {
  const { animate } = props;
  const [opacity] = useState(() => new Animated.Value(0));
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- runs the Animated opacity fade-in and stops it on cleanup
  useEffect(() => {
    if (!animate) return;
    const animation = Animated.timing(opacity, {
      duration: FADE_IN_MS,
      toValue: 1,
      useNativeDriver: false,
    });
    animation.start();
    return () => animation.stop();
  }, [animate, opacity]);

  return (
    <Animated.View
      style={[
        {
          alignItems: "center",
          flexDirection: "row",
          maxWidth: props.maxWidth,
          // A hidden tab never runs the fade, so its value would still be 0 when the tab returns.
          opacity: animate ? opacity : 1,
        },
        props.grow ? { flex: 1, minWidth: 0 } : null,
      ]}
    >
      {props.children}
    </Animated.View>
  );
}

export function WorkspaceConnectionTitle(props: {
  readonly brand: ReactNode;
  /** Called with the machine a host warning is about, and with nothing for a connection status. */
  readonly onPress?: (environmentId?: EnvironmentId) => void;
  readonly grow?: boolean;
  readonly size?: "navbar" | "pageTitle";
  readonly statusOffset?: number;
  readonly maxWidth?: number;
}) {
  const status = use(ConnectionStatusContext);
  const hostPressureAlert = use(HostPressureAlertContext);
  const gt = useGT();
  const size = props.size ?? "navbar";
  const { scale } = useAndroidControlSizing();

  if (status === null) {
    if (hostPressureAlert !== null) {
      return (
        <View
          style={[
            { alignItems: "center", flexDirection: "row", gap: 8 * scale, minWidth: 0 },
            props.grow ? { flex: 1 } : null,
          ]}
        >
          {/* The brand may fill the row; the warning keeps its place beside it. */}
          <View style={{ flexShrink: 1, minWidth: 0 }}>{props.brand}</View>
          <Pressable
            accessibilityHint={gt("Opens environment settings")}
            accessibilityLabel={hostPressureAlertTitle(gt, hostPressureAlert)}
            accessibilityRole="button"
            disabled={props.onPress === undefined}
            hitSlop={10}
            onPress={() => props.onPress?.(hostPressureAlert.strained[0]!.environmentId)}
            className="items-center justify-center rounded-full bg-warning"
            style={{ height: 26 * scale, width: 26 * scale }}
          >
            <SymbolView
              name={ALERT_SYMBOLS[hostPressureAlertKind(hostPressureAlert)]}
              size={Math.round(15 * scale)}
              tintColorClassName="accent-warning-foreground"
              type="monochrome"
            />
          </Pressable>
        </View>
      );
    }
    return props.grow ? (
      <View style={{ alignItems: "center", flex: 1, flexDirection: "row", minWidth: 0 }}>
        {props.brand}
      </View>
    ) : (
      <>{props.brand}</>
    );
  }

  return (
    <StatusFadeIn animate={status.fadeIn} grow={props.grow} maxWidth={props.maxWidth}>
      <Pressable
        accessibilityHint={gt("Opens environment settings")}
        accessibilityLabel={status.label}
        accessibilityRole="button"
        disabled={props.onPress === undefined}
        hitSlop={8}
        onPress={() => props.onPress?.()}
        className="flex-row items-center gap-2"
        style={[
          { flexShrink: 1, marginLeft: props.statusOffset ?? 0 },
          Platform.OS === "android" && { gap: 7 * scale },
        ]}
      >
        {status.showsProgress ? (
          <AppActivityIndicator
            colorClassName={"accent-icon-muted"}
            size={Platform.OS === "android" ? Math.round(20 * scale) : "small"}
          />
        ) : (
          <SymbolView
            name="wifi.slash"
            size={Math.round((size === "pageTitle" ? 17 : 15) * scale)}
            tintColorClassName={"accent-icon-muted"}
            type="monochrome"
          />
        )}
        <Text
          className="font-oni-bold text-foreground-muted"
          numberOfLines={1}
          style={{ flexShrink: 1, fontSize: (size === "pageTitle" ? 20 : 16) * scale }}
        >
          {status.label}
        </Text>
      </Pressable>
    </StatusFadeIn>
  );
}
