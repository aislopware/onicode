import * as Schema from "effect/Schema";
import * as HttpApi from "effect/http-api/HttpApi";
import * as HttpApiEndpoint from "effect/http-api/HttpApiEndpoint";
import * as HttpApiGroup from "effect/http-api/HttpApiGroup";
import * as OpenApi from "effect/http-api/OpenApi";

export const PUSH_GATEWAY_MAX_BODY_BYTES = 48 * 1024;
export const PUSH_ACTIVITY_MAX_ROWS = 5;
/** A signed payload older or newer than this is refused, so a captured request cannot be replayed later. */
export const PUSH_GATEWAY_CLOCK_SKEW_MS = 5 * 60_000;

const base64Url = (maxLength: number) =>
  Schema.String.check(Schema.isPattern(/^[A-Za-z0-9_-]+$/), Schema.isMaxLength(maxLength));
const base64 = (maxLength: number) =>
  Schema.String.check(Schema.isPattern(/^[A-Za-z0-9+/]+={0,2}$/), Schema.isMaxLength(maxLength));
const text = (maxLength: number) =>
  Schema.String.check(Schema.isMinLength(1), Schema.isMaxLength(maxLength));
const EpochMillis = Schema.Int.check(Schema.isGreaterThanOrEqualTo(0));

export const PushPlatform = Schema.Literals(["ios", "android"]);
export type PushPlatform = typeof PushPlatform.Type;

/** A bundle identifier on iOS, an application id on Android. */
export const PushAppId = Schema.String.check(
  Schema.isPattern(/^[A-Za-z][A-Za-z0-9_-]*(\.[A-Za-z0-9_-]+)+$/),
  Schema.isMaxLength(155),
);
export type PushAppId = typeof PushAppId.Type;

/** What a grant's token may receive: alerts, a Live Activity start, or updates to one Live Activity. */
export const PushGrantKind = Schema.Literals([
  "notification",
  "liveActivityStart",
  "liveActivityUpdate",
]);
export type PushGrantKind = typeof PushGrantKind.Type;

/** APNs tokens are hex; FCM registration tokens are URL-safe with colons. Both go into provider URLs or bodies verbatim. */
export const PushProviderToken = Schema.String.check(
  Schema.isPattern(/^[A-Za-z0-9_:-]+$/),
  Schema.isMinLength(32),
  Schema.isMaxLength(4096),
);
export type PushProviderToken = typeof PushProviderToken.Type;

export const PushSealedToken = base64Url(8192);
export type PushSealedToken = typeof PushSealedToken.Type;

/** An environment's raw Ed25519 public key. */
export const PushEnvironmentKey = Schema.String.check(Schema.isPattern(/^[A-Za-z0-9_-]{43}$/));
export type PushEnvironmentKey = typeof PushEnvironmentKey.Type;

export const PushActivityPhase = Schema.Literals([
  "starting",
  "running",
  "waiting_for_approval",
  "waiting_for_input",
  "completed",
  "failed",
  "stale",
]);
export type PushActivityPhase = typeof PushActivityPhase.Type;

const PushDeepLink = Schema.String.check(
  Schema.isPattern(/^\/(?!\/)[^\s]*$/),
  Schema.isMaxLength(768),
);
/** A delegated thread's id runs past 128 characters. */
const PushThreadId = text(384);

export const PushActivityRow = Schema.Struct({
  environmentId: text(128),
  threadId: PushThreadId,
  projectTitle: text(120),
  threadTitle: text(120),
  modelTitle: text(120),
  phase: PushActivityPhase,
  status: text(40),
  updatedAt: text(40),
  deepLink: PushDeepLink,
});
export type PushActivityRow = typeof PushActivityRow.Type;

/** One environment's agents as the Live Activity and the Android ongoing notification show them. */
export const PushActivityState = Schema.Struct({
  title: text(120),
  subtitle: text(120),
  activeCount: Schema.Int.check(Schema.isBetween({ minimum: 0, maximum: 9999 })),
  updatedAt: text(40),
  activities: Schema.Array(PushActivityRow).check(Schema.isMaxLength(PUSH_ACTIVITY_MAX_ROWS)),
});
export type PushActivityState = typeof PushActivityState.Type;

export const PushNotification = Schema.Struct({
  /** Stable per transition, so a client drops a repeat. */
  id: base64Url(128),
  title: text(120),
  body: text(240),
  environmentId: text(128),
  /** Empty for a test alert, which belongs to no thread. */
  threadId: Schema.String.check(Schema.isMaxLength(384)),
  deepLink: PushDeepLink,
});
export type PushNotification = typeof PushNotification.Type;

/**
 * What an environment asks the gateway to show. The gateway builds the APNs or FCM payload itself,
 * so a caller cannot send provider fields of its own.
 */
export const PushMessage = Schema.Struct({
  /** The registering client's device id, echoed so a client can drop pushes for an old registration. */
  deviceId: text(128),
  /** The sender, so a client keeps one activity per environment and can end it without any rows. */
  environmentId: text(128),
  notification: Schema.optionalKey(PushNotification),
  /** Present to update the activity; a null state ends it. */
  activity: Schema.optionalKey(
    Schema.Struct({
      state: Schema.NullOr(PushActivityState),
      /** When the state stops being worth showing if no later update arrives. */
      expiresAt: Schema.optionalKey(EpochMillis),
    }),
  ),
});
export type PushMessage = typeof PushMessage.Type;

export const PushChallengeResponse = Schema.Struct({
  challenge: base64Url(256),
  expiresAt: EpochMillis,
});
export type PushChallengeResponse = typeof PushChallengeResponse.Type;

export const PushAttestDeviceRequest = Schema.Union([
  Schema.Struct({
    platform: Schema.Literal("ios"),
    appId: PushAppId,
    challenge: base64Url(256),
    /** App Attest key identifier, as `generateKey` returned it. */
    keyId: base64(64),
    attestation: base64(16 * 1024),
  }),
  Schema.Struct({
    platform: Schema.Literal("android"),
    appId: PushAppId,
    challenge: base64Url(256),
    /** DER certificates from the attested key up to the root. */
    certificateChain: Schema.Array(base64(8 * 1024)).check(
      Schema.isMinLength(2),
      Schema.isMaxLength(8),
    ),
  }),
  Schema.Struct({
    platform: PushPlatform,
    appId: PushAppId,
    challenge: base64Url(256),
    /**
     * An uncompressed P-256 point with no attestation behind it. Simulators and emulators cannot
     * attest, so a gateway accepts this only for app ids it lists as unattested.
     */
    publicKey: base64(128),
  }),
]);
export type PushAttestDeviceRequest = typeof PushAttestDeviceRequest.Type;

export const PushAttestDeviceResponse = Schema.Struct({
  credential: PushSealedToken,
  expiresAt: EpochMillis,
});
export type PushAttestDeviceResponse = typeof PushAttestDeviceResponse.Type;

export const PushGrantTarget = Schema.Struct({
  kind: PushGrantKind,
  token: PushProviderToken,
});
export type PushGrantTarget = typeof PushGrantTarget.Type;

/** The JSON a device signs to ask for grants. */
export const PushIssueGrantsPayload = Schema.Struct({
  issuedAt: EpochMillis,
  environmentKey: PushEnvironmentKey,
  targets: Schema.Array(PushGrantTarget).check(Schema.isMinLength(1), Schema.isMaxLength(4)),
});
export type PushIssueGrantsPayload = typeof PushIssueGrantsPayload.Type;
export const PushIssueGrantsPayloadJson = Schema.fromJsonString(PushIssueGrantsPayload);

export const PushIssueGrantsRequest = Schema.Struct({
  credential: PushSealedToken,
  /** `PushIssueGrantsPayload` as the exact string the device signed. */
  payload: Schema.String.check(Schema.isMaxLength(24 * 1024)),
  /** An App Attest assertion on iOS, a DER ECDSA signature on Android. */
  proof: base64(2048),
});
export type PushIssueGrantsRequest = typeof PushIssueGrantsRequest.Type;

export const PushIssuedGrant = Schema.Struct({
  kind: PushGrantKind,
  grant: PushSealedToken,
  expiresAt: EpochMillis,
});
export type PushIssuedGrant = typeof PushIssuedGrant.Type;

export const PushIssueGrantsResponse = Schema.Struct({
  grants: Schema.Array(PushIssuedGrant),
});
export type PushIssueGrantsResponse = typeof PushIssueGrantsResponse.Type;

/** The JSON an environment signs to deliver one message. */
export const PushDeliverPayload = Schema.Struct({
  issuedAt: EpochMillis,
  /** SHA-256 of the grant string, so a signature cannot be moved onto another grant. */
  grantDigest: base64Url(43),
  message: PushMessage,
});
export type PushDeliverPayload = typeof PushDeliverPayload.Type;
export const PushDeliverPayloadJson = Schema.fromJsonString(PushDeliverPayload);

export const PushDeliverRequest = Schema.Struct({
  grant: PushSealedToken,
  /** `PushDeliverPayload` as the exact string the environment signed. */
  payload: Schema.String.check(Schema.isMaxLength(32 * 1024)),
  /** Ed25519 signature over the payload string, by the key the grant names. */
  signature: base64Url(86),
});
export type PushDeliverRequest = typeof PushDeliverRequest.Type;

export const PushDeliverResponse = Schema.Struct({
  /** `unregistered` means the provider no longer knows the token; the grant is dead. */
  outcome: Schema.Literals(["delivered", "unregistered"]),
});
export type PushDeliverResponse = typeof PushDeliverResponse.Type;

export class PushGatewayInvalidRequestError extends Schema.TaggedError<PushGatewayInvalidRequestError>()(
  "PushGatewayInvalidRequestError",
  {
    reason: Schema.Literals([
      "payload-malformed",
      "payload-stale",
      "message-not-allowed",
      "app-not-served",
      "token-malformed",
    ]),
  },
  { httpApiStatus: 400 },
) {
  override get message(): string {
    return `The push gateway refused the request: ${this.reason}.`;
  }
}

export class PushGatewayAttestationRejectedError extends Schema.TaggedError<PushGatewayAttestationRejectedError>()(
  "PushGatewayAttestationRejectedError",
  {
    reason: Schema.Literals([
      "challenge-invalid",
      "challenge-expired",
      "attestation-malformed",
      "chain-untrusted",
      "certificate-revoked",
      "app-mismatch",
      "key-mismatch",
      "environment-mismatch",
      "device-untrusted",
    ]),
  },
  { httpApiStatus: 403 },
) {
  override get message(): string {
    return `The push gateway could not verify this app install: ${this.reason}.`;
  }
}

export class PushGatewayCredentialInvalidError extends Schema.TaggedError<PushGatewayCredentialInvalidError>()(
  "PushGatewayCredentialInvalidError",
  { reason: Schema.Literals(["malformed", "expired", "proof-invalid", "revoked"]) },
  { httpApiStatus: 401 },
) {
  override get message(): string {
    return `The device credential was refused: ${this.reason}.`;
  }
}

export class PushGatewayGrantInvalidError extends Schema.TaggedError<PushGatewayGrantInvalidError>()(
  "PushGatewayGrantInvalidError",
  {
    reason: Schema.Literals([
      "malformed",
      "expired",
      "signature-invalid",
      "digest-mismatch",
      "revoked",
    ]),
  },
  { httpApiStatus: 401 },
) {
  override get message(): string {
    return `The push grant was refused: ${this.reason}.`;
  }
}

export class PushGatewayRateLimitedError extends Schema.TaggedError<PushGatewayRateLimitedError>()(
  "PushGatewayRateLimitedError",
  { retryAfterSeconds: Schema.Int },
  { httpApiStatus: 429 },
) {
  override get message(): string {
    return `The push gateway is rate limiting this caller; retry in ${this.retryAfterSeconds}s.`;
  }
}

export class PushGatewayProviderError extends Schema.TaggedError<PushGatewayProviderError>()(
  "PushGatewayProviderError",
  {
    provider: Schema.Literals(["apns", "fcm"]),
    /** False when the provider rejected this message for good, so a retry cannot succeed. */
    retryable: Schema.Boolean,
    reason: Schema.String.check(Schema.isMaxLength(120)),
  },
  { httpApiStatus: 502 },
) {
  override get message(): string {
    return `${this.provider.toUpperCase()} did not accept the push: ${this.reason}.`;
  }
}

export class PushGatewayUnavailableError extends Schema.TaggedError<PushGatewayUnavailableError>()(
  "PushGatewayUnavailableError",
  { reason: Schema.Literals(["disabled", "platform-not-configured", "dependency-unavailable"]) },
  { httpApiStatus: 503 },
) {
  override get message(): string {
    return `The push gateway cannot serve this request: ${this.reason}.`;
  }
}

const DeviceGroup = HttpApiGroup.make("devices")
  .add(
    HttpApiEndpoint.post("challenge", "/v1/challenges", {
      success: PushChallengeResponse,
      error: [PushGatewayRateLimitedError, PushGatewayUnavailableError],
    }).annotate(OpenApi.Summary, "Issue an attestation challenge"),
  )
  .add(
    HttpApiEndpoint.post("attest", "/v1/devices", {
      payload: PushAttestDeviceRequest,
      success: PushAttestDeviceResponse,
      error: [
        PushGatewayInvalidRequestError,
        PushGatewayAttestationRejectedError,
        PushGatewayRateLimitedError,
        PushGatewayUnavailableError,
      ],
    }).annotate(OpenApi.Summary, "Verify an app install and issue its device credential"),
  )
  .add(
    HttpApiEndpoint.post("issueGrants", "/v1/grants", {
      payload: PushIssueGrantsRequest,
      success: PushIssueGrantsResponse,
      error: [
        PushGatewayInvalidRequestError,
        PushGatewayCredentialInvalidError,
        PushGatewayRateLimitedError,
        PushGatewayUnavailableError,
      ],
    }).annotate(OpenApi.Summary, "Seal push tokens into grants bound to one environment"),
  )
  .annotate(OpenApi.Description, "Called by the mobile app.");

const DeliveryGroup = HttpApiGroup.make("deliveries")
  .add(
    HttpApiEndpoint.post("deliver", "/v1/deliveries", {
      payload: PushDeliverRequest,
      success: PushDeliverResponse,
      error: [
        PushGatewayInvalidRequestError,
        PushGatewayGrantInvalidError,
        PushGatewayRateLimitedError,
        PushGatewayProviderError,
        PushGatewayUnavailableError,
      ],
    }).annotate(OpenApi.Summary, "Deliver one message to the device a grant names"),
  )
  .annotate(OpenApi.Description, "Called by Onicode environments.");

const HealthGroup = HttpApiGroup.make("health").add(
  HttpApiEndpoint.get("check", "/health", { success: Schema.Struct({ ok: Schema.Literal(true) }) }),
);

export const PushGatewayApi = HttpApi.make("PushGatewayApi")
  .add(HealthGroup, DeviceGroup, DeliveryGroup)
  .annotate(OpenApi.Title, "Onicode Push Gateway")
  .annotate(OpenApi.Version, "1.0.0");
export type PushGatewayApi = typeof PushGatewayApi;
