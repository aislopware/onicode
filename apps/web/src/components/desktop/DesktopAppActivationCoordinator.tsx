import type { DesktopAppActivationRequest } from "@onicode/contracts";
import { useEffect, useEffectEvent, useRef } from "react";

import { useOpenLocalWorkspace } from "../../hooks/useOpenLocalWorkspace";

export function DesktopAppActivationCoordinator() {
  const openLocalWorkspace = useOpenLocalWorkspace();
  const queueRef = useRef(Promise.resolve());
  const activation = window.desktopBridge?.appActivation;
  // With no environment to wait for, requests get their failure now instead of timing out.
  const ready = activation !== undefined && openLocalWorkspace.state !== "pending";

  const processRequest = useEffectEvent(async (request: DesktopAppActivationRequest) =>
    openLocalWorkspace.open(request),
  );

  // A request sent as readiness drops must still find a listener, so it gets an answer, not a timeout.
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- subscribes to desktop activation requests over the bridge
  useEffect(() => {
    if (activation === undefined) return;
    return activation.onRequest((request) => {
      queueRef.current = queueRef.current.then(async () => {
        const response = await processRequest(request);
        await activation.complete(response);
      });
      queueRef.current = queueRef.current.catch(() => undefined);
    });
  }, [activation]);

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
