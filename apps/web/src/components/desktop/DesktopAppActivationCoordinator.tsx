import type { DesktopAppActivationRequest } from "@onicode/contracts";
import { useEffect, useEffectEvent } from "react";

import { setDesktopAppActivationHandler } from "../../desktopAppActivation";
import { useOpenLocalWorkspace } from "../../hooks/useOpenLocalWorkspace";

export function DesktopAppActivationCoordinator() {
  const openLocalWorkspace = useOpenLocalWorkspace();
  const activation = window.desktopBridge?.appActivation;
  // With no environment to wait for, requests get their failure now instead of timing out.
  const ready = activation !== undefined && openLocalWorkspace.state !== "pending";

  const processRequest = useEffectEvent(async (request: DesktopAppActivationRequest) =>
    openLocalWorkspace.open(request),
  );

  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- hands desktop activation requests to this mounted handler
  useEffect(() => {
    setDesktopAppActivationHandler((request) => processRequest(request));
    return () => setDesktopAppActivationHandler(null);
  }, []);

  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- tells the desktop app whether this renderer can take requests now
  useEffect(() => {
    if (!ready || activation === undefined) return;
    let subscribed = true;
    // Skip readiness if React runs cleanup before it is sent.
    queueMicrotask(() => {
      if (subscribed) void activation.setReady(true).catch(() => undefined);
    });
    return () => {
      subscribed = false;
      void activation.setReady(false).catch(() => undefined);
    };
  }, [activation, ready]);

  return null;
}
