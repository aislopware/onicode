import * as Schema from "effect/Schema";

import { PushEnvironmentKey, PushPlatform, PushSealedToken } from "./pushGateway.ts";

/** The installation id a mobile client generates once and keeps. */
export const PushDeviceId = Schema.String.check(Schema.isPattern(/^[A-Za-z0-9._:-]{8,128}$/));
export type PushDeviceId = typeof PushDeviceId.Type;

const EpochMillis = Schema.Int.check(Schema.isGreaterThanOrEqualTo(0));

/** A gateway-sealed push token. The environment can deliver through it but never reads the token. */
export const PushGrant = Schema.Struct({
  grant: PushSealedToken,
  expiresAt: EpochMillis,
});
export type PushGrant = typeof PushGrant.Type;

export const PushDevicePreferences = Schema.Struct({
  notifyOnApproval: Schema.Boolean,
  notifyOnInput: Schema.Boolean,
  notifyOnCompletion: Schema.Boolean,
  notifyOnFailure: Schema.Boolean,
  /** The Live Activity on iOS, the ongoing notification on Android. */
  activity: Schema.Boolean,
});
export type PushDevicePreferences = typeof PushDevicePreferences.Type;

/** What a device needs before it can ask its gateway for grants this environment can use. */
export const PushRegistrationInfo = Schema.Struct({
  environmentKey: PushEnvironmentKey,
  /** The gateways this environment delivers through. A device on another gateway cannot register. */
  gatewayUrls: Schema.Array(Schema.String),
});
export type PushRegistrationInfo = typeof PushRegistrationInfo.Type;

export const PushRegisterDeviceRequest = Schema.Struct({
  platform: PushPlatform,
  label: Schema.String.check(Schema.isMinLength(1), Schema.isMaxLength(120)),
  gatewayUrl: Schema.String.check(Schema.isMaxLength(512)),
  /** Null when the device may not show alerts. */
  notification: Schema.NullOr(PushGrant),
  /** iOS only: lets the environment start the Live Activity while the app is closed. */
  liveActivityStart: Schema.NullOr(PushGrant),
  preferences: PushDevicePreferences,
});
export type PushRegisterDeviceRequest = typeof PushRegisterDeviceRequest.Type;

export const PushDevice = Schema.Struct({
  deviceId: PushDeviceId,
  platform: PushPlatform,
  label: Schema.String,
  preferences: PushDevicePreferences,
  alerts: Schema.Boolean,
  registeredAt: Schema.DateTimeUtcFromString,
  lastDeliveredAt: Schema.NullOr(Schema.DateTimeUtcFromString),
  /** Why the latest delivery failed, cleared by the next one that works. */
  lastFailure: Schema.NullOr(Schema.String),
  /** Registered by the session asking. */
  current: Schema.Boolean,
});
export type PushDevice = typeof PushDevice.Type;

export const PushDeviceList = Schema.Struct({ devices: Schema.Array(PushDevice) });
export type PushDeviceList = typeof PushDeviceList.Type;

export const PushTestResult = Schema.Struct({
  outcome: Schema.Literals(["delivered", "unregistered", "failed"]),
  detail: Schema.optionalKey(Schema.String),
});
export type PushTestResult = typeof PushTestResult.Type;
