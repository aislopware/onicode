import { type EnvironmentId, managedCapabilityDenial } from "@onicode/contracts";
import { Atom } from "effect/reactivity";

import { environmentPresentations } from "~/state/presentation";
import { serverEnvironment } from "~/state/server";

/**
 * Whether this environment offers "Share with guest": the user may create links there, and both
 * its Guest links beta (Settings > Labs) and public access are on. Guests arrive through Tailscale
 * Funnel, so a policy that keeps clients on the machine hides sharing too.
 */
export const canShareWithGuestAtom = Atom.family((environmentId: EnvironmentId) =>
  Atom.make((get) => {
    const serverConfig = get(environmentPresentations.serverConfigsAtom).get(environmentId);
    return (
      get(serverEnvironment.createGuestShare.permissionAtom(environmentId)) &&
      serverConfig?.settings.guestSharingEnabled === true &&
      serverConfig.settings.publicAccessEnabled &&
      managedCapabilityDenial(serverConfig.managedPolicy, "remoteClients") === undefined
    );
  }).pipe(Atom.withLabel(`guest-share:available:${environmentId}`)),
);
