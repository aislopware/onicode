import { useAtomValue } from "@effect/atom-react";
import type { EnvironmentPresentation } from "@onicode/client-runtime/connection";
import {
  createEnvironmentPresentationAtoms,
  createEnvironmentSummaryAtoms,
} from "@onicode/client-runtime/state/presentation";
import { type EnvironmentId, type HostPressure, severeHostPressureKinds } from "@onicode/contracts";
import * as Option from "effect/Option";
import { Atom } from "effect/reactivity";

import { environmentCatalog } from "../connection/catalog";
import { environmentServerConfigsAtom, serverEnvironment } from "./server";
import { createThreadListEnvironmentsAtom } from "./thread-list-environments";

export const environmentPresentations = createEnvironmentPresentationAtoms({
  catalogValueAtom: environmentCatalog.catalogValueAtom,
  stateAtom: environmentCatalog.stateAtom,
  serverConfigValueAtom: serverEnvironment.configValueAtom,
});

export const { connectedEnvironmentIdsAtom } = createEnvironmentSummaryAtoms({
  catalogValueAtom: environmentCatalog.catalogValueAtom,
  presentationAtom: environmentPresentations.presentationAtom,
});

export interface StrainedHost {
  readonly environmentId: EnvironmentId;
  readonly label: string;
  readonly pressure: HostPressure;
  readonly canForceStop: boolean;
}

export interface HostPressureAlert {
  readonly strained: ReadonlyArray<StrainedHost>;
  /** Whether the user has other machines, so a lone strained one is named. */
  readonly named: boolean;
}

const sameStrainedHost = (left: StrainedHost, right: StrainedHost) =>
  left.environmentId === right.environmentId &&
  left.label === right.label &&
  left.pressure === right.pressure &&
  left.canForceStop === right.canForceStop;

/** Connected machines under severe pressure; a machine that is merely busy is fine. */
export const hostPressureAlertAtom = Atom.make((get): HostPressureAlert | null => {
  const configs = get(environmentServerConfigsAtom);
  const presentations = get(environmentPresentations.presentationsAtom);
  const strained = [...configs].flatMap(([environmentId, config]): ReadonlyArray<StrainedHost> => {
    const pressure = config.hostPressure;
    // A disconnected machine's last sample is stale, not a current warning.
    const connected = presentations.get(environmentId)?.connection.phase === "connected";
    return connected && pressure !== undefined && severeHostPressureKinds(pressure).length > 0
      ? [
          {
            environmentId,
            label: config.environment.label,
            pressure,
            canForceStop: config.environment.capabilities.threadForceStop === true,
          },
        ]
      : [];
  });
  if (strained.length === 0) return null;
  const next = { strained, named: configs.size > 1 };
  const previous = Option.getOrNull(get.self<HostPressureAlert | null>());
  // The provider puts this in context for every header, so an unchanged warning keeps its object.
  return previous !== null &&
    previous.named === next.named &&
    previous.strained.length === strained.length &&
    previous.strained.every((entry, index) => sameStrainedHost(entry, strained[index]!))
    ? previous
    : next;
}).pipe(Atom.withLabel("mobile-host-pressure-alert"));

export const threadListEnvironmentsAtom = createThreadListEnvironmentsAtom(
  environmentServerConfigsAtom,
  connectedEnvironmentIdsAtom,
);

const EMPTY_ENVIRONMENT_PRESENTATION_ATOM = Atom.make<EnvironmentPresentation | null>(null).pipe(
  Atom.withLabel("mobile-environment-presentation:empty"),
);

export function useEnvironmentPresentation(environmentId: EnvironmentId | null) {
  const catalog = useAtomValue(environmentCatalog.catalogValueAtom);
  const presentation = useAtomValue(
    environmentId === null
      ? EMPTY_ENVIRONMENT_PRESENTATION_ATOM
      : environmentPresentations.presentationAtom(environmentId),
  );
  return {
    isReady: catalog.isReady,
    presentation,
  };
}
