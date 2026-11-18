import type { EnvironmentId } from "@onicode/contracts";
import { Atom, type AtomRegistry } from "effect/reactivity";

import { environmentCatalog } from "../connection/catalog";
import { appAtomRegistry } from "../rpc/atomRegistry";

export const primaryEnvironmentIdAtom = Atom.make((get) => {
  for (const [environmentId, entry] of get(environmentCatalog.catalogValueAtom).entries) {
    if (entry.target._tag === "PrimaryConnectionTarget") {
      return environmentId;
    }
  }
  return null;
}).pipe(Atom.withLabel("web-primary-environment-id"));

export const hostEnvironmentIdAtom = Atom.make<string | null>(null).pipe(
  Atom.keepAlive,
  Atom.withLabel("web-host-environment-id"),
);

export const hostEnvironmentLookupSettledAtom = Atom.make(false).pipe(
  Atom.keepAlive,
  Atom.withLabel("web-host-environment-lookup-settled"),
);

// This computer's environment: the primary, else a connection to the service sharing the app's Onicode home.
export function createLocalEnvironmentIdAtom(input: {
  readonly primaryEnvironmentIdAtom: Atom.Atom<EnvironmentId | null>;
  readonly hostEnvironmentIdAtom: Atom.Atom<string | null>;
  readonly catalogValueAtom: Atom.Atom<{ readonly entries: ReadonlyMap<EnvironmentId, unknown> }>;
}) {
  return Atom.make((get) => {
    const primaryEnvironmentId = get(input.primaryEnvironmentIdAtom);
    if (primaryEnvironmentId !== null) return primaryEnvironmentId;
    const hostEnvironmentId = get(input.hostEnvironmentIdAtom);
    for (const environmentId of get(input.catalogValueAtom).entries.keys()) {
      if (environmentId === hostEnvironmentId) return environmentId;
    }
    return null;
  }).pipe(Atom.withLabel("web-local-environment-id"));
}

export const localEnvironmentIdAtom = createLocalEnvironmentIdAtom({
  primaryEnvironmentIdAtom,
  hostEnvironmentIdAtom,
  catalogValueAtom: environmentCatalog.catalogValueAtom,
});

/** The connections are known, and so is whether this computer runs a service of its own. */
export const localEnvironmentDiscoverySettledAtom = Atom.make(
  (get) =>
    get(environmentCatalog.catalogValueAtom).isReady && get(hostEnvironmentLookupSettledAtom),
).pipe(Atom.withLabel("web-local-environment-discovery-settled"));

export function startHostEnvironmentLookup(
  input: {
    readonly registry: AtomRegistry.AtomRegistry;
    readonly catalogValueAtom: Atom.Atom<{
      readonly entries: ReadonlyMap<EnvironmentId, unknown>;
    }>;
    readonly read: (() => Promise<string | null>) | undefined;
  } = {
    registry: appAtomRegistry,
    catalogValueAtom: environmentCatalog.catalogValueAtom,
    read: window.desktopBridge?.getHostEnvironmentId,
  },
): void {
  const { registry, read } = input;
  if (read === undefined) {
    registry.set(hostEnvironmentLookupSettledAtom, true);
    return;
  }
  let latestRead = 0;
  const lookup = () => {
    const thisRead = ++latestRead;
    registry.set(hostEnvironmentLookupSettledAtom, false);
    void read().then(
      (environmentId) => {
        if (thisRead !== latestRead) return;
        registry.set(hostEnvironmentIdAtom, environmentId);
        registry.set(hostEnvironmentLookupSettledAtom, true);
      },
      () => {
        if (thisRead === latestRead) registry.set(hostEnvironmentLookupSettledAtom, true);
      },
    );
  };
  lookup();
  // A service started after this window records its ID before it can be paired, so read again.
  registry.subscribe(input.catalogValueAtom, (catalog) => {
    const hostEnvironmentId = registry.get(hostEnvironmentIdAtom);
    if (![...catalog.entries.keys()].some((environmentId) => environmentId === hostEnvironmentId)) {
      lookup();
    }
  });
}
