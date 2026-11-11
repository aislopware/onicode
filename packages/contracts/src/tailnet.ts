import * as Schema from "effect/Schema";

import { EnvironmentId, TrimmedNonEmptyString } from "./baseSchemas.ts";

/**
 * The Tailscale app capability an ACL grant gives a device so an Onicode server
 * signs it in by its tailnet identity. A value of `{ "access": "read" }` limits
 * the session to read scopes; any other value grants a standard client session.
 */
export const TAILNET_APP_CAPABILITY = "onicode.com/cap/environment" as const;

export const TailnetDeviceOwner = Schema.Struct({
  loginName: TrimmedNonEmptyString,
  displayName: TrimmedNonEmptyString,
});
export type TailnetDeviceOwner = typeof TailnetDeviceOwner.Type;

/** An Onicode server that answered on a tailnet device. */
export const TailnetDeviceOnicode = Schema.Struct({
  environmentId: EnvironmentId,
  label: TrimmedNonEmptyString,
  serverVersion: TrimmedNonEmptyString,
  httpBaseUrl: TrimmedNonEmptyString,
  /** The server signs in tailnet devices its policy allows, with no pairing code. */
  tailnetSignIn: Schema.Boolean,
});
export type TailnetDeviceOnicode = typeof TailnetDeviceOnicode.Type;

export const TailnetDevice = Schema.Struct({
  /** Tailscale's stable node ID. */
  id: TrimmedNonEmptyString,
  /** The machine name the Tailscale apps show. */
  name: TrimmedNonEmptyString,
  dnsName: Schema.optionalKey(TrimmedNonEmptyString),
  /** Tailnet addresses, IPv4 first. */
  addresses: Schema.Array(TrimmedNonEmptyString),
  os: Schema.optionalKey(TrimmedNonEmptyString),
  online: Schema.Boolean,
  /** The machine running the server that listed the devices. */
  self: Schema.Boolean,
  /** Null for a tagged device, which belongs to its tags rather than a user. */
  owner: Schema.NullOr(TailnetDeviceOwner),
  tags: Schema.Array(TrimmedNonEmptyString),
  onicode: Schema.optionalKey(TailnetDeviceOnicode),
});
export type TailnetDevice = typeof TailnetDevice.Type;

export const TailnetDevicesResult = Schema.Struct({
  /** False when the server's machine has no running, signed-in Tailscale. */
  available: Schema.Boolean,
  tailnetName: Schema.optionalKey(TrimmedNonEmptyString),
  devices: Schema.Array(TailnetDevice),
});
export type TailnetDevicesResult = typeof TailnetDevicesResult.Type;
