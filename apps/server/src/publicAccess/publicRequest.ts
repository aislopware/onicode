import * as Context from "effect/Context";

/** Set on requests that arrived through Funnel, which must never count as this machine's own. */
export const IsPublicRequest = Context.Reference<boolean>("onicode/publicAccess/IsPublicRequest", {
  defaultValue: () => false,
});
