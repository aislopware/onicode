import { managedUpdatesMessage, type ManagedPolicyDocument } from "@onicode/contracts";

/**
 * The update mirror the policy sends updates to, or `undefined` for the public releases. It wins
 * over the environment, which a user can set.
 */
export function managedUpdateMirror(document: ManagedPolicyDocument | undefined) {
  const feedUrl = document?.limits?.updates?.feedUrl;
  return feedUrl === undefined || document === undefined
    ? undefined
    : { organization: document.organization, feedUrl };
}

/**
 * Why the managed policy refuses an update a user or maintainer asked for, or `undefined` when it
 * allows it. The server never updates on its own, so `auto` allows every explicit request.
 */
export function managedUpdateRefusal(
  document: ManagedPolicyDocument | undefined,
): string | undefined {
  const updates = document?.limits?.updates;
  if (document === undefined || updates === undefined) return undefined;
  return updates.mode === "off" ? managedUpdatesMessage(document.organization) : undefined;
}
