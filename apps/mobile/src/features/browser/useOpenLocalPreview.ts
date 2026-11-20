import {
  allowsPreviewTunnel,
  AuthTerminalOperateScope,
  type EnvironmentId,
} from "@onicode/contracts";
import { useAtomValue } from "@effect/atom-react";
import { useNavigation } from "@react-navigation/native";
import { Platform } from "react-native";

import { environmentServerConfigsAtom } from "../../state/server";
import { useEnvironmentScope, useEnvironmentScopeDenied } from "../../state/session";
import { useOpenThreadInspector } from "../threads/thread-inspector-navigation";
import { loopbackUrlForDevice, tunnelReaches } from "./browserTabs";

/** Opens an environment's localhost page on this device: in the inspector pane where there is one. */
export function useOpenLocalPreview() {
  const navigation = useNavigation();
  const openInspector = useOpenThreadInspector();
  return (source: { readonly environmentId: string; readonly threadId: string }, url: string) => {
    if (openInspector(source, { type: "localPreview", ...source, url })) return;
    navigation.navigate("ThreadLocalPreview", { environmentId: source.environmentId, url });
  };
}

/**
 * Why this device can't open the environment's localhost pages, once the host has said so: its
 * Onicode has no tunnel, its setting or a policy turned the tunnel off, or this connection may not
 * use it (the tunnel is raw TCP into the host, so the host opens it only for terminal operators).
 */
export function useLocalPreviewBlocker(
  environmentId: EnvironmentId,
): "update" | "off" | "permission" | null {
  const tunnel = useAtomValue(environmentServerConfigsAtom, (configs) => {
    const config = configs.get(environmentId);
    if (config === undefined) return null;
    if (config.environment.capabilities.previewTunnel !== true) return "update";
    return allowsPreviewTunnel(config) ? null : "off";
  });
  const denied = useEnvironmentScopeDenied(environmentId, AuthTerminalOperateScope);
  return tunnel ?? (denied ? "permission" : null);
}

/**
 * The address to offer `href` at on this device, or null when it is not one of the environment's
 * loopback pages or the device can't open it.
 */
export function useLocalPreviewUrl(environmentId: EnvironmentId) {
  const tunnel = useAtomValue(environmentServerConfigsAtom, (configs) => {
    const config = configs.get(environmentId);
    return config?.environment.capabilities.previewTunnel === true && allowsPreviewTunnel(config);
  });
  const canTunnel = useEnvironmentScope(environmentId, AuthTerminalOperateScope) && tunnel;
  return (href: string) => {
    const url = canTunnel ? loopbackUrlForDevice(href) : null;
    return url !== null && tunnelReaches(url, Platform.OS) ? url : null;
  };
}
