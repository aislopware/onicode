import { createServerEnvironmentAtoms } from "@onicode/client-runtime/state/server";
import {
  createOutdatedServerDescriptionCommand,
  createOutdatedServerUpdateCommand,
} from "@onicode/client-runtime/state/outdatedServerUpdate";
import { createEnvironmentServerConfigsAtom } from "@onicode/client-runtime/state/shell";
import { Atom } from "effect/reactivity";

import { environmentCatalog } from "../connection/catalog";
import { connectionAtomRuntime } from "../connection/runtime";
import { environmentSession } from "./session";

export const serverEnvironment = createServerEnvironmentAtoms(connectionAtomRuntime, {
  initialConfigValueAtom: environmentSession.initialConfigValueAtom,
  usageLimitSources: true,
  hostPressure: true,
  usageLimitsCommand: true,
  providerCatalogsOnDemand: true,
  providerCatalogDigests: true,
  providerStatusDeltas: true,
  omitRefreshedProviders: true,
});
export const describeOutdatedServer = createOutdatedServerDescriptionCommand(connectionAtomRuntime);
export const updateOutdatedServer = createOutdatedServerUpdateCommand(connectionAtomRuntime);
export const environmentServerConfigsAtom = createEnvironmentServerConfigsAtom({
  catalogValueAtom: environmentCatalog.catalogValueAtom,
  serverConfigValueAtom: serverEnvironment.configValueAtom,
});
/** Whether any server has workflows on in Labs, which lists the Workflows tab. */
export const workflowsEnabledAtom = Atom.make((get) =>
  [...get(environmentServerConfigsAtom).values()].some(
    (config) => config.settings.workflowsEnabled,
  ),
).pipe(Atom.withLabel("workflows-enabled"));
