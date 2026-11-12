import { createRemoteDesktopEnvironmentAtoms } from "@onicode/client-runtime/state/remote-desktop";
import { Atom } from "effect/reactivity";

import { connectionAtomRuntime } from "../connection/runtime";
import { createRemoteDesktopHost, type RemoteDesktopHostSession } from "../remoteDesktop/host";
import { appAtomRegistry } from "../rpc/atomRegistry";

/** Who is watching this machine's screen right now. */
export const remoteDesktopHostSessionsAtom = Atom.make<ReadonlyArray<RemoteDesktopHostSession>>(
  [],
).pipe(Atom.keepAlive, Atom.withLabel("remote-desktop:host-sessions"));

let host: ReturnType<typeof createRemoteDesktopHost> | null = null;

/** This desktop app's host, or null in a client that has no screen to share. */
export function remoteDesktopHost() {
  const bridge = window.desktopBridge?.remoteDesktop;
  if (!bridge) return null;
  host ??= createRemoteDesktopHost(bridge, (sessions) =>
    appAtomRegistry.set(remoteDesktopHostSessionsAtom, sessions),
  );
  return host;
}

export const remoteDesktopEnvironment = createRemoteDesktopEnvironmentAtoms(
  connectionAtomRuntime,
  (request) =>
    remoteDesktopHost()?.accept(request) ??
    Promise.reject(new Error("This client cannot share a screen.")),
);
