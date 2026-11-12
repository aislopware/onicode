import {
  PushRegisterDeviceRequest as PushRegisterDeviceRequestSchema,
  type EnvironmentId,
  type PushDevicePreferences,
  type PushGrant,
  type PushRegisterDeviceRequest,
  type PushRegistrationInfo,
  type PushTestResult,
} from "@onicode/contracts";
import {
  PushIssueGrantsPayloadJson,
  type PushIssueGrantsPayload,
} from "@onicode/contracts/pushGateway";
import type {
  PushAttestDeviceRequest,
  PushAttestDeviceResponse,
  PushChallengeResponse,
  PushGatewayAttestationRejectedError,
  PushGatewayCredentialInvalidError,
  PushGrantTarget,
  PushIssueGrantsResponse,
  PushPlatform,
} from "@onicode/contracts/pushGateway";
import * as Clock from "effect/Clock";
import * as Effect from "effect/Effect";
import * as Ref from "effect/Ref";
import * as Schema from "effect/Schema";
import * as Semaphore from "effect/Semaphore";

const DAY_MS = 24 * 60 * 60_000;
/** Renew the device credential this long before the gateway stops accepting it. */
export const CREDENTIAL_RENEWAL_MS = 30 * DAY_MS;

export class PushDeviceKeyError extends Schema.TaggedError<PushDeviceKeyError>()(
  "PushDeviceKeyError",
  { operation: Schema.String, cause: Schema.Defect() },
) {
  override get message(): string {
    return `The device key could not ${this.operation}.`;
  }
}

export class PushGatewayRequestError extends Schema.TaggedError<PushGatewayRequestError>()(
  "PushGatewayRequestError",
  { message: Schema.String },
) {}

export class PushEnvironmentRequestError extends Schema.TaggedError<PushEnvironmentRequestError>()(
  "PushEnvironmentRequestError",
  { message: Schema.String },
) {}

export class PushUnavailableError extends Schema.TaggedError<PushUnavailableError>()(
  "PushUnavailableError",
  { reason: Schema.Literals(["attestation-unavailable", "attestation-rejected"]) },
) {
  override get message(): string {
    return this.reason === "attestation-rejected"
      ? "The push gateway does not accept this app install."
      : "This device cannot prove it is a genuine app install.";
  }
}

export type PushGatewayError =
  | PushGatewayAttestationRejectedError
  | PushGatewayCredentialInvalidError
  | PushGatewayRequestError;

export interface PushGatewayClient {
  readonly challenge: Effect.Effect<PushChallengeResponse, PushGatewayError>;
  readonly attest: (
    request: PushAttestDeviceRequest,
  ) => Effect.Effect<PushAttestDeviceResponse, PushGatewayError>;
  readonly issueGrants: (request: {
    readonly credential: string;
    readonly payload: string;
    readonly proof: string;
  }) => Effect.Effect<PushIssueGrantsResponse, PushGatewayError>;
}

export interface HardwareAttestation {
  readonly keyId: string;
  /** iOS: the App Attest attestation object. */
  readonly attestation?: string;
  /** Android: the attested key's DER certificates, leaf first. */
  readonly certificateChain?: ReadonlyArray<string>;
}

export interface PushDeviceKeys {
  readonly hardwareAttestationSupported: boolean;
  readonly attestHardwareKey: (
    challenge: string,
  ) => Effect.Effect<HardwareAttestation, PushDeviceKeyError>;
  readonly signWithHardwareKey: (
    keyId: string,
    payload: string,
  ) => Effect.Effect<string, PushDeviceKeyError>;
  /** Returns the new key's uncompressed P-256 point, base64. */
  readonly createSoftwareKey: Effect.Effect<string, PushDeviceKeyError>;
  readonly signWithSoftwareKey: (payload: string) => Effect.Effect<string, PushDeviceKeyError>;
}

export interface PushEnvironmentClient {
  readonly registration: Effect.Effect<PushRegistrationInfo, PushEnvironmentRequestError>;
  readonly register: (
    deviceId: string,
    request: PushRegisterDeviceRequest,
  ) => Effect.Effect<void, PushEnvironmentRequestError>;
  readonly registerLiveActivity: (
    deviceId: string,
    grant: PushGrant,
  ) => Effect.Effect<void, PushEnvironmentRequestError>;
  readonly unregister: (deviceId: string) => Effect.Effect<void, PushEnvironmentRequestError>;
  readonly sendTest: (
    deviceId: string,
  ) => Effect.Effect<PushTestResult, PushEnvironmentRequestError>;
}

export type DeviceKey =
  | { readonly kind: "hardware"; readonly keyId: string }
  | { readonly kind: "software" };

export interface StoredCredential {
  readonly gatewayUrl: string;
  readonly credential: string;
  readonly expiresAt: number;
  readonly key: DeviceKey;
}

export interface StoredGrant {
  readonly token: string;
  readonly grant: string;
  readonly issuedAt: number;
  readonly expiresAt: number;
}

type RegistrationGrantKind = "notification" | "liveActivityStart";

export interface StoredEnvironment {
  readonly environmentKey: string;
  readonly gatewayUrls: ReadonlyArray<string>;
  readonly grants: Partial<Record<RegistrationGrantKind, StoredGrant>>;
}

export interface PushRegistrationState {
  readonly credential: StoredCredential | null;
  readonly environments: Readonly<Record<string, StoredEnvironment>>;
}

export const EMPTY_PUSH_REGISTRATION_STATE: PushRegistrationState = {
  credential: null,
  environments: {},
};

export type EnvironmentPushStatus =
  | { readonly _tag: "registered" }
  | { readonly _tag: "gateway-not-allowed" }
  | { readonly _tag: "unreachable"; readonly message: string }
  | { readonly _tag: "failed"; readonly message: string };

export interface PushTokens {
  readonly notification: string | null;
  /** iOS only: the push-to-start token for the AgentActivity Live Activity. */
  readonly liveActivityStart: string | null;
}

export interface PushRegistrarConfig {
  readonly platform: PushPlatform;
  readonly appId: string;
  readonly gatewayUrl: string;
  readonly deviceId: string;
  readonly label: string;
  /** Development builds may fall back to an unattested key where the hardware cannot attest. */
  readonly allowUnattested: boolean;
}

/** A grant is refreshed when its token changed or a third of its lifetime remains. */
export function grantIsFresh(
  grant: Omit<StoredGrant, "grant"> | undefined,
  token: string,
  now: number,
): boolean {
  return (
    grant !== undefined &&
    grant.token === token &&
    grant.expiresAt - now > (grant.expiresAt - grant.issuedAt) / 3
  );
}

function credentialIsUsable(
  credential: StoredCredential | null,
  gatewayUrl: string,
  now: number,
): credential is StoredCredential {
  return (
    credential !== null &&
    credential.gatewayUrl === gatewayUrl &&
    credential.expiresAt - now > CREDENTIAL_RENEWAL_MS
  );
}

const encodeGrantsPayloadJson = Schema.encodeEffect(PushIssueGrantsPayloadJson);
const encodeGrantsPayload = (payload: PushIssueGrantsPayload) =>
  encodeGrantsPayloadJson(payload).pipe(Effect.orDie);
const PushRegisterDeviceRequestJson = Schema.fromJsonString(PushRegisterDeviceRequestSchema);
const encodeRegistrationJson = Schema.encodeEffect(PushRegisterDeviceRequestJson);
const encodeRegistration = (request: PushRegisterDeviceRequest) =>
  encodeRegistrationJson(request).pipe(Effect.orDie);

function toPushGrant(grant: StoredGrant | undefined): PushGrant | null {
  return grant === undefined ? null : { grant: grant.grant, expiresAt: grant.expiresAt };
}

export function makePushRegistrar(input: {
  readonly config: PushRegistrarConfig;
  readonly gateway: PushGatewayClient;
  readonly keys: PushDeviceKeys;
  readonly load: Effect.Effect<PushRegistrationState>;
  readonly save: (state: PushRegistrationState) => Effect.Effect<void>;
}) {
  const { config, gateway, keys } = input;

  return Effect.gen(function* () {
    const state = yield* Ref.make(yield* input.load);
    const lock = yield* Semaphore.make(1);
    // Each launch registers once per environment, so a server that lost the device gets it back.
    const registered = new Map<string, string>();

    const update = Effect.fnUntraced(function* (
      transform: (current: PushRegistrationState) => PushRegistrationState,
    ) {
      const next = transform(yield* Ref.get(state));
      yield* Ref.set(state, next);
      yield* input.save(next);
      return next;
    });

    const attestUnattested = Effect.fn("PushRegistrar.attestUnattested")(function* () {
      const { challenge } = yield* gateway.challenge;
      const publicKey = yield* keys.createSoftwareKey;
      const response = yield* gateway.attest({
        platform: config.platform,
        appId: config.appId,
        challenge,
        publicKey,
      });
      return { response, key: { kind: "software" } as const };
    });

    const attestHardware = Effect.fn("PushRegistrar.attestHardware")(function* () {
      const { challenge } = yield* gateway.challenge;
      const attested = yield* keys.attestHardwareKey(challenge);
      const request: PushAttestDeviceRequest =
        config.platform === "ios"
          ? {
              platform: "ios",
              appId: config.appId,
              challenge,
              keyId: attested.keyId,
              attestation: attested.attestation ?? "",
            }
          : {
              platform: "android",
              appId: config.appId,
              challenge,
              certificateChain: attested.certificateChain ?? [],
            };
      const response = yield* gateway.attest(request);
      return { response, key: { kind: "hardware", keyId: attested.keyId } as const };
    });

    const attest = Effect.fn("PushRegistrar.attest")(function* () {
      const attested = keys.hardwareAttestationSupported
        ? yield* attestHardware().pipe(
            Effect.catchTags({
              PushDeviceKeyError: (error) =>
                config.allowUnattested ? attestUnattested() : Effect.fail(error),
              PushGatewayAttestationRejectedError: () =>
                config.allowUnattested
                  ? attestUnattested()
                  : Effect.fail(new PushUnavailableError({ reason: "attestation-rejected" })),
            }),
          )
        : config.allowUnattested
          ? yield* attestUnattested()
          : yield* new PushUnavailableError({ reason: "attestation-unavailable" });
      const credential: StoredCredential = {
        gatewayUrl: config.gatewayUrl,
        credential: attested.response.credential,
        expiresAt: attested.response.expiresAt,
        key: attested.key,
      };
      yield* update((current) => ({ ...current, credential }));
      return credential;
    });

    const ensureCredential = Effect.fn("PushRegistrar.ensureCredential")(function* () {
      const current = (yield* Ref.get(state)).credential;
      return credentialIsUsable(current, config.gatewayUrl, yield* Clock.currentTimeMillis)
        ? current
        : yield* attest();
    });

    const sign = (key: DeviceKey, payload: string) =>
      key.kind === "hardware"
        ? keys.signWithHardwareKey(key.keyId, payload)
        : keys.signWithSoftwareKey(payload);

    const requestGrants = Effect.fn("PushRegistrar.requestGrants")(function* (
      credential: StoredCredential,
      environmentKey: string,
      targets: ReadonlyArray<PushGrantTarget>,
    ) {
      const issuedAt = yield* Clock.currentTimeMillis;
      const payload = yield* encodeGrantsPayload({ issuedAt, environmentKey, targets });
      const proof = yield* sign(credential.key, payload);
      const response = yield* gateway.issueGrants({
        credential: credential.credential,
        payload,
        proof,
      });
      return { issuedAt, grants: response.grants };
    });

    const issueGrants = Effect.fn("PushRegistrar.issueGrants")(function* (
      environmentKey: string,
      targets: ReadonlyArray<PushGrantTarget>,
    ) {
      const credential = yield* ensureCredential();
      return yield* requestGrants(credential, environmentKey, targets).pipe(
        Effect.catchTags({
          PushGatewayCredentialInvalidError: () =>
            attest().pipe(
              Effect.flatMap((renewed) => requestGrants(renewed, environmentKey, targets)),
            ),
        }),
      );
    });

    const readRegistration = Effect.fn("PushRegistrar.readRegistration")(function* (
      environmentId: EnvironmentId,
      client: PushEnvironmentClient,
    ) {
      const info = yield* client.registration;
      const next = yield* update((current) => {
        const previous = current.environments[environmentId];
        // A new environment key invalidates every grant sealed for the old one.
        const grants = previous?.environmentKey === info.environmentKey ? previous.grants : {};
        return {
          ...current,
          environments: {
            ...current.environments,
            [environmentId]: {
              environmentKey: info.environmentKey,
              gatewayUrls: info.gatewayUrls,
              grants,
            },
          },
        };
      });
      return next.environments[environmentId]!;
    });

    const syncGrants = Effect.fn("PushRegistrar.syncGrants")(function* (
      environmentId: EnvironmentId,
      environment: StoredEnvironment,
      wanted: Partial<Record<RegistrationGrantKind, string>>,
    ) {
      const now = yield* Clock.currentTimeMillis;
      const stale = (Object.entries(wanted) as Array<[RegistrationGrantKind, string]>).filter(
        ([kind, token]) => !grantIsFresh(environment.grants[kind], token, now),
      );
      const kept = Object.fromEntries(
        Object.keys(wanted).flatMap((kind) => {
          const grant = environment.grants[kind as RegistrationGrantKind];
          return grant === undefined ? [] : [[kind, grant]];
        }),
      ) as Partial<Record<RegistrationGrantKind, StoredGrant>>;
      if (stale.length === 0) return kept;
      const issued = yield* issueGrants(
        environment.environmentKey,
        stale.map(([kind, token]) => ({ kind, token })),
      );
      const grants = { ...kept };
      for (const grant of issued.grants) {
        if (grant.kind === "liveActivityUpdate") continue;
        grants[grant.kind] = {
          token: wanted[grant.kind]!,
          grant: grant.grant,
          issuedAt: issued.issuedAt,
          expiresAt: grant.expiresAt,
        };
      }
      yield* update((current) => {
        const stored = current.environments[environmentId];
        return stored === undefined
          ? current
          : {
              ...current,
              environments: { ...current.environments, [environmentId]: { ...stored, grants } },
            };
      });
      return grants;
    });

    const syncEnvironmentUnlocked = Effect.fn("PushRegistrar.syncEnvironment")(function* (request: {
      readonly environmentId: EnvironmentId;
      readonly server: PushEnvironmentClient;
      readonly tokens: PushTokens;
      readonly preferences: PushDevicePreferences;
    }) {
      const environment = yield* readRegistration(request.environmentId, request.server);
      if (!environment.gatewayUrls.includes(config.gatewayUrl)) {
        return { _tag: "gateway-not-allowed" } as const;
      }
      const wanted: Partial<Record<RegistrationGrantKind, string>> = {};
      if (request.tokens.notification) wanted.notification = request.tokens.notification;
      if (
        config.platform === "ios" &&
        request.preferences.activity &&
        request.tokens.liveActivityStart
      ) {
        wanted.liveActivityStart = request.tokens.liveActivityStart;
      }
      const grants = yield* syncGrants(request.environmentId, environment, wanted);
      const body: PushRegisterDeviceRequest = {
        platform: config.platform,
        label: config.label,
        gatewayUrl: config.gatewayUrl,
        notification: toPushGrant(grants.notification),
        liveActivityStart: toPushGrant(grants.liveActivityStart),
        preferences: request.preferences,
      };
      const signature = yield* encodeRegistration(body);
      if (registered.get(request.environmentId) !== signature) {
        yield* request.server.register(config.deviceId, body);
        registered.set(request.environmentId, signature);
      }
      return { _tag: "registered" } as const;
    });

    const syncEnvironment = (request: Parameters<typeof syncEnvironmentUnlocked>[0]) =>
      lock.withPermit(syncEnvironmentUnlocked(request)).pipe(
        Effect.catch(
          (
            error,
          ): Effect.Effect<EnvironmentPushStatus, PushUnavailableError | PushDeviceKeyError> => {
            switch (error._tag) {
              case "PushEnvironmentRequestError":
                return Effect.succeed({ _tag: "unreachable", message: error.message });
              case "PushGatewayRequestError":
              case "PushGatewayAttestationRejectedError":
              case "PushGatewayCredentialInvalidError":
                return Effect.succeed({ _tag: "failed", message: error.message });
              default:
                return Effect.fail(error);
            }
          },
        ),
      );

    const registerLiveActivity = Effect.fn("PushRegistrar.registerLiveActivity")(
      function* (request: {
        readonly environmentId: EnvironmentId;
        readonly server: PushEnvironmentClient;
        readonly token: string;
      }) {
        const cached = (yield* Ref.get(state)).environments[request.environmentId];
        const environment =
          cached ?? (yield* readRegistration(request.environmentId, request.server));
        if (!environment.gatewayUrls.includes(config.gatewayUrl)) return null;
        const issued = yield* lock.withPermit(
          issueGrants(environment.environmentKey, [
            { kind: "liveActivityUpdate", token: request.token },
          ]),
        );
        const grant = issued.grants.find((candidate) => candidate.kind === "liveActivityUpdate");
        if (grant === undefined) {
          return yield* new PushGatewayRequestError({
            message: "The gateway issued no update grant.",
          });
        }
        yield* request.server.registerLiveActivity(config.deviceId, {
          grant: grant.grant,
          expiresAt: grant.expiresAt,
        });
        return grant.expiresAt;
      },
    );

    const unregisterEnvironment = Effect.fn("PushRegistrar.unregisterEnvironment")(
      function* (request: {
        readonly environmentId: EnvironmentId;
        readonly server: PushEnvironmentClient | null;
      }) {
        registered.delete(request.environmentId);
        yield* update((current) => {
          const { [request.environmentId]: _removed, ...environments } = current.environments;
          return { ...current, environments };
        });
        if (request.server !== null) {
          yield* request.server.unregister(config.deviceId).pipe(Effect.ignore);
        }
      },
    );

    const knownEnvironmentIds = Ref.get(state).pipe(
      Effect.map((current) => Object.keys(current.environments) as Array<EnvironmentId>),
    );

    return {
      syncEnvironment,
      registerLiveActivity,
      unregisterEnvironment,
      knownEnvironmentIds,
    };
  });
}

export type PushRegistrar = Effect.Success<ReturnType<typeof makePushRegistrar>>;

/**
 * Which environments to register and which to drop. Disabled connections keep their
 * registration; only removed ones, or every one after opting out, are dropped.
 */
export function planPushSync(input: {
  readonly enabled: boolean;
  readonly saved: ReadonlySet<EnvironmentId>;
  readonly connected: ReadonlyArray<EnvironmentId>;
  readonly known: ReadonlyArray<EnvironmentId>;
}): {
  readonly sync: ReadonlyArray<EnvironmentId>;
  readonly unregister: ReadonlyArray<EnvironmentId>;
} {
  return {
    sync: input.enabled ? input.connected : [],
    unregister: input.known.filter((id) => !input.enabled || !input.saved.has(id)),
  };
}
