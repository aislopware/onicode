import { AuthTerminalOperateScope, type EnvironmentId } from "@onicode/contracts";
import { useAtomValue } from "@effect/atom-react";
import { useNavigation } from "@react-navigation/native";

import { environmentServerConfigsAtom } from "../../state/server";
import { useEnvironmentScope, useEnvironmentScopeDenied } from "../../state/session";
import { useOpenThreadInspector } from "../threads/thread-inspector-navigation";
import { loopbackUrlForDevice } from "./browserTabs";

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
 * Onicode has no tunnel, or this connection may not use it (the tunnel is raw TCP into the host,
 * so the host opens it only for terminal operators).
 */
export function useLocalPreviewBlocker(
  environmentId: EnvironmentId,
): "update" | "permission" | null {
  const noTunnel = useAtomValue(environmentServerConfigsAtom, (configs) => {
    const config = configs.get(environmentId);
    return config !== undefined && config.environment.capabilities.previewTunnel !== true;
  });
  const denied = useEnvironmentScopeDenied(environmentId, AuthTerminalOperateScope);
  return noTunnel ? "update" : denied ? "permission" : null;
}

/**
 * The address to offer `href` at on this device, or null when it is not one of the environment's
 * loopback pages or the device can't open it.
 */
export function useLocalPreviewUrl(environmentId: EnvironmentId) {
  const tunnel = useAtomValue(
    environmentServerConfigsAtom,
    (configs) => configs.get(environmentId)?.environment.capabilities.previewTunnel === true,
  );
  const canTunnel = useEnvironmentScope(environmentId, AuthTerminalOperateScope) && tunnel;
  return (href: string) => (canTunnel ? loopbackUrlForDevice(href) : null);
}
