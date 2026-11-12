import { useAtomMount, useAtomValue } from "@effect/atom-react";
import type { EnvironmentId, RemoteDesktopHostCapabilities } from "@onicode/contracts";
import { Atom } from "effect/reactivity";
import { useEffect, useState } from "react";

import { useClientSettings } from "../../hooks/useSettings";
import { usePrimaryEnvironmentId } from "../../state/environments";
import { remoteDesktopEnvironment, remoteDesktopHost } from "../../state/remoteDesktop";
import { primaryServerConfigAtom } from "../../state/server";

const CAPABILITIES_REFRESH_MS = 5_000;
const NOT_HOSTING_ATOM = Atom.make(null).pipe(Atom.withLabel("remote-desktop:not-hosting"));

/** Shares this machine's screen through its own environment while the setting is on. */
export function RemoteDesktopHosting() {
  const enabled = useClientSettings((settings) => settings.remoteDesktopHostEnabled);
  const environmentId = usePrimaryEnvironmentId();
  const supported = useAtomValue(primaryServerConfigAtom)?.remoteDesktop === true;
  const bridge = window.desktopBridge?.remoteDesktop;
  if (!enabled || !supported || environmentId === null || !bridge) return null;
  return <ActiveHost bridge={bridge} environmentId={environmentId} />;
}

function ActiveHost(props: {
  readonly bridge: NonNullable<Window["desktopBridge"]>["remoteDesktop"];
  readonly environmentId: EnvironmentId;
}) {
  const { bridge } = props;
  const [capabilities, setCapabilities] = useState<RemoteDesktopHostCapabilities | null>(null);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- tells the main process this window hosts, and re-reads displays and OS permissions, which change outside React
  useEffect(() => {
    let active = true;
    const refresh = () => {
      void bridge.capabilities().then((next) => {
        if (!active) return;
        // A new object for the same answer would register the host again.
        setCapabilities((current) =>
          JSON.stringify(current) === JSON.stringify(next) ? current : next,
        );
      });
    };
    void bridge.setHosting(true);
    refresh();
    // macOS announces neither a permission granted in System Settings nor every display change,
    // and a host is often hidden or unattended, so it cannot wait for its window to be focused.
    const timer = setInterval(refresh, CAPABILITIES_REFRESH_MS);
    return () => {
      active = false;
      clearInterval(timer);
      void bridge.setHosting(false);
      remoteDesktopHost()?.stop();
    };
  }, [bridge]);
  useAtomMount(
    capabilities === null
      ? NOT_HOSTING_ATOM
      : remoteDesktopEnvironment.host({
          environmentId: props.environmentId,
          input: {
            displays: capabilities.displays,
            control: capabilities.inputControl === true,
          },
        }),
  );
  return null;
}
