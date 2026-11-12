import type * as Electron from "electron";

type DisplayMediaRequest = Parameters<
  Parameters<Electron.Session["setDisplayMediaRequestHandler"]>[0] & {}
>[0];

/** Returns the streams for a request this feature armed, or null to leave it to the next. */
export type DisplayMediaClaim = (request: DisplayMediaRequest) => Electron.Streams | null;

const claimsBySession = new WeakMap<Electron.Session, Set<DisplayMediaClaim>>();

/**
 * A session takes a single display-media handler, so features that capture from the same
 * renderer share it. A request no feature armed is refused: pages must not capture on their own.
 */
export function claimDisplayMediaRequests(session: Electron.Session, claim: DisplayMediaClaim) {
  let claims = claimsBySession.get(session);
  if (!claims) {
    const registered = new Set<DisplayMediaClaim>();
    claims = registered;
    claimsBySession.set(session, registered);
    session.setDisplayMediaRequestHandler((request, callback) => {
      for (const candidate of registered) {
        const streams = candidate(request);
        if (streams) return callback(streams);
      }
      callback({});
    });
  }
  claims.add(claim);
}
