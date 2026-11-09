import * as Schema from "effect/Schema";

/**
 * Public access publishes webhook addresses to the internet through Tailscale Funnel. Only
 * those routes are reachable from outside the tailnet; the API never is.
 */

/** Why Funnel could not be turned on. Mirrors the Tailscale CLI failures the server can tell apart. */
export const PublicAccessFailure = Schema.Literals([
  "not-installed",
  "not-running",
  "not-logged-in",
  "https-disabled",
  "funnel-not-allowed",
  "port-in-use",
  "permission-denied",
  "policy-denied",
  "unknown",
]);
export type PublicAccessFailure = typeof PublicAccessFailure.Type;

export const PublicAccessStatus = Schema.Union([
  Schema.TaggedStruct("off", {}),
  Schema.TaggedStruct("starting", {}),
  Schema.TaggedStruct("on", {
    /** `https://<node>.<tailnet>.ts.net[:port]`, with no trailing slash. */
    origin: Schema.String,
  }),
  Schema.TaggedStruct("failed", { reason: PublicAccessFailure }),
]);
export type PublicAccessStatus = typeof PublicAccessStatus.Type;
const PUBLIC_ACCESS_FAILURE_MESSAGES: Record<PublicAccessFailure, string> = {
  "not-installed": "Tailscale is not installed on this machine.",
  "not-running": "Tailscale is not running on this machine.",
  "not-logged-in": "This machine is not logged in to Tailscale.",
  "https-disabled":
    "HTTPS certificates are off for this tailnet. Turn them on in the Tailscale admin console under DNS.",
  "funnel-not-allowed":
    'Your tailnet policy does not allow Funnel on this machine. Add the "funnel" node attribute for it in the Tailscale admin console under Access controls.',
  "port-in-use": "Another Tailscale Serve or Funnel route already uses the public port.",
  "permission-denied":
    "Tailscale refused to change Funnel. Run `sudo tailscale set --operator=$USER` once, then try again.",
  "policy-denied": "Your organization does not allow connections from other devices.",
  unknown: "Tailscale could not turn Funnel on.",
};

export const publicAccessFailureMessage = (reason: PublicAccessFailure): string =>
  PUBLIC_ACCESS_FAILURE_MESSAGES[reason];
