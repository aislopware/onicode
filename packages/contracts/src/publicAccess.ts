import * as Schema from "effect/Schema";

/**
 * Public access publishes guest links and webhook addresses to the internet through Tailscale
 * Funnel. Only those routes are reachable from outside the tailnet; the app and API never are.
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
