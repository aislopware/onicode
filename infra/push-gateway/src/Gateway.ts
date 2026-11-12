import {
  PUSH_GATEWAY_CLOCK_SKEW_MS,
  type PushAttestDeviceRequest,
  type PushAttestDeviceResponse,
  type PushChallengeResponse,
  PushDeliverPayloadJson,
  type PushDeliverRequest,
  type PushDeliverResponse,
  PushEnvironmentKey,
  PushGatewayAttestationRejectedError,
  PushGatewayCredentialInvalidError,
  PushGatewayGrantInvalidError,
  PushGatewayInvalidRequestError,
  type PushGatewayProviderError,
  type PushGatewayRateLimitedError,
  PushGatewayUnavailableError,
  PushGrantKind,
  PushIssueGrantsPayloadJson,
  type PushIssueGrantsRequest,
  type PushIssueGrantsResponse,
  PushPlatform,
} from "@onicode/contracts/pushGateway";
import * as Clock from "effect/Clock";
import * as Context from "effect/Context";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

import * as Apns from "./Apns.ts";
import { verifyAndroidKeyAttestation } from "./attestation/AndroidKey.ts";
import * as AndroidRevocations from "./attestation/AndroidRevocations.ts";
import { verifyAppAttestAssertion, verifyAppAttestation } from "./attestation/AppAttest.ts";
import {
  rejected,
  type AttestationRejection,
  type AttestationResult,
} from "./attestation/result.ts";
import { ecdsaDerToRaw, fromBase64, sha256, toBase64Url, utf8 } from "./bytes.ts";
import * as Config from "./Config.ts";
import * as Fcm from "./Fcm.ts";
import { apnsLiveActivityRequest, apnsNotificationRequest, fcmRequest } from "./payloads.ts";
import * as RateLimiter from "./RateLimiter.ts";

const CHALLENGE_TTL_MS = 5 * 60_000;
// An App Attest key lives as long as the install, so re-attesting twice a year is enough.
const CREDENTIAL_TTL_MS = 180 * 24 * 60 * 60_000;
// The app renews these on launch, well before they lapse.
const GRANT_TTL_MS = 60 * 24 * 60 * 60_000;
// iOS ends a Live Activity after eight hours and clears it four hours later.
const LIVE_ACTIVITY_GRANT_TTL_MS = 12 * 60 * 60_000;
const P256_POINT_BYTES = 65;
const APNS_TOKEN = /^[0-9a-f]{64,512}$/i;

const Challenge = Schema.Struct({ n: Schema.String, x: Schema.Number });

const ApnsEnvironment = Schema.Literals(["development", "production"]);

/** What an attested install proved: its key, and on iOS which APNs environment its token is for. */
const Credential = Schema.Struct({
  p: PushPlatform,
  a: Schema.String,
  k: Schema.String,
  /** `unattested` is honoured only while the gateway still lists the app id. */
  f: Schema.Literals(["app-attest", "ecdsa", "unattested"]),
  e: Schema.optionalKey(ApnsEnvironment),
  x: Schema.Number,
});
type Credential = typeof Credential.Type;

/** One push token, usable only by the environment whose key is sealed beside it. */
const Grant = Schema.Struct({
  k: PushGrantKind,
  p: PushPlatform,
  a: Schema.String,
  e: Schema.optionalKey(ApnsEnvironment),
  t: Schema.String,
  s: PushEnvironmentKey,
  /** Issued to an install that proved nothing about its hardware. */
  u: Schema.optionalKey(Schema.Literal(true)),
  x: Schema.Number,
});
type Grant = typeof Grant.Type;

const decodeChallenge = Schema.decodeUnknownOption(Challenge);
const decodeCredential = Schema.decodeUnknownOption(Credential);
const decodeGrant = Schema.decodeUnknownOption(Grant);
const decodeIssueGrantsPayload = Schema.decodeUnknownOption(PushIssueGrantsPayloadJson);
const decodeDeliverPayload = Schema.decodeUnknownOption(PushDeliverPayloadJson);

export class Gateway extends Context.Service<
  Gateway,
  {
    readonly challenge: (
      clientAddress: string,
    ) => Effect.Effect<
      PushChallengeResponse,
      PushGatewayRateLimitedError | PushGatewayUnavailableError
    >;
    readonly attest: (
      request: PushAttestDeviceRequest,
      clientAddress: string,
    ) => Effect.Effect<
      PushAttestDeviceResponse,
      | PushGatewayInvalidRequestError
      | PushGatewayAttestationRejectedError
      | PushGatewayRateLimitedError
      | PushGatewayUnavailableError
    >;
    readonly issueGrants: (
      request: PushIssueGrantsRequest,
    ) => Effect.Effect<
      PushIssueGrantsResponse,
      | PushGatewayInvalidRequestError
      | PushGatewayCredentialInvalidError
      | PushGatewayRateLimitedError
      | PushGatewayUnavailableError
    >;
    readonly deliver: (
      request: PushDeliverRequest,
    ) => Effect.Effect<
      PushDeliverResponse,
      | PushGatewayInvalidRequestError
      | PushGatewayGrantInvalidError
      | PushGatewayRateLimitedError
      | PushGatewayProviderError
      | PushGatewayUnavailableError
    >;
  }
>()("@onicode/push-gateway/Gateway") {}

const invalid = (reason: PushGatewayInvalidRequestError["reason"]) =>
  new PushGatewayInvalidRequestError({ reason });

const isFresh = (issuedAt: number, now: number) =>
  Math.abs(now - issuedAt) <= PUSH_GATEWAY_CLOCK_SKEW_MS;

async function verifyEcdsa(
  publicKey: Uint8Array<ArrayBuffer>,
  derSignature: Uint8Array,
  data: Uint8Array<ArrayBuffer>,
): Promise<boolean> {
  const signature = ecdsaDerToRaw(derSignature, 32);
  if (signature === null) return false;
  try {
    const key = await crypto.subtle.importKey(
      "raw",
      publicKey,
      { name: "ECDSA", namedCurve: "P-256" },
      false,
      ["verify"],
    );
    return await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, key, signature, data);
  } catch {
    return false;
  }
}

async function verifyEd25519(
  publicKey: Uint8Array<ArrayBuffer>,
  signature: Uint8Array<ArrayBuffer>,
  data: Uint8Array<ArrayBuffer>,
): Promise<boolean> {
  try {
    const key = await crypto.subtle.importKey("raw", publicKey, { name: "Ed25519" }, false, [
      "verify",
    ]);
    return await crypto.subtle.verify({ name: "Ed25519" }, key, signature, data);
  } catch {
    return false;
  }
}

export const layer = Layer.effect(
  Gateway,
  Effect.gen(function* () {
    const config = yield* Config.GatewayConfig;
    const limiter = yield* RateLimiter.RateLimiter;
    const revocations = yield* AndroidRevocations.AndroidRevocations;
    const apns = yield* Apns.Apns;
    const fcm = yield* Fcm.Fcm;
    const { sealer } = config;

    const ensureEnabled = config.disabled
      ? Effect.fail(new PushGatewayUnavailableError({ reason: "disabled" }))
      : Effect.void;

    /** The app id as the platform's attestation names it, or why this gateway does not serve it. */
    const servedApp = (platform: PushPlatform, appId: string) => {
      if (platform === "ios") {
        if (config.ios === null) {
          return Effect.fail(
            new PushGatewayUnavailableError({ reason: "platform-not-configured" }),
          );
        }
        return config.ios.bundleIds.has(appId)
          ? Effect.void
          : Effect.fail(invalid("app-not-served"));
      }
      if (config.android === null) {
        return Effect.fail(new PushGatewayUnavailableError({ reason: "platform-not-configured" }));
      }
      return config.android.apps.has(appId) ? Effect.void : Effect.fail(invalid("app-not-served"));
    };

    const verified = <A>(result: Promise<AttestationResult<A>>) =>
      // The certificate and CBOR parsers throw on input they do not expect.
      Effect.promise(() => result.catch(() => rejected("attestation-malformed"))).pipe(
        Effect.flatMap((outcome) =>
          outcome.ok
            ? Effect.succeed(outcome.value)
            : Effect.fail(new PushGatewayAttestationRejectedError({ reason: outcome.reason })),
        ),
      );
    const reject = (reason: AttestationRejection) =>
      Effect.fail(new PushGatewayAttestationRejectedError({ reason }));

    const challenge: Gateway["Service"]["challenge"] = Effect.fn("Gateway.challenge")(
      function* (clientAddress) {
        yield* ensureEnabled;
        yield* limiter.check("challenge", RateLimiter.addressRateKey(clientAddress));
        const expiresAt = (yield* Clock.currentTimeMillis) + CHALLENGE_TTL_MS;
        const nonce = toBase64Url(crypto.getRandomValues(new Uint8Array(16)));
        return {
          challenge: yield* Effect.promise(() =>
            sealer.seal("challenge", { n: nonce, x: expiresAt }),
          ),
          expiresAt,
        };
      },
    );

    const attest: Gateway["Service"]["attest"] = Effect.fn("Gateway.attest")(
      function* (request, clientAddress) {
        yield* ensureEnabled;
        yield* limiter.check("attest", RateLimiter.addressRateKey(clientAddress));
        yield* servedApp(request.platform, request.appId);
        const now = yield* Clock.currentTimeMillis;

        const issued = decodeChallenge(
          yield* Effect.promise(() => sealer.open("challenge", request.challenge)),
        );
        if (Option.isNone(issued)) return yield* reject("challenge-invalid");
        if (issued.value.x < now) return yield* reject("challenge-expired");
        const challengeHash = yield* Effect.promise(() => sha256(utf8(request.challenge)));

        let credential: Omit<Credential, "x">;
        if ("publicKey" in request) {
          const publicKey = fromBase64(request.publicKey);
          if (!config.unattestedAppIds.has(request.appId)) return yield* reject("device-untrusted");
          if (publicKey?.byteLength !== P256_POINT_BYTES || publicKey[0] !== 0x04) {
            return yield* reject("key-mismatch");
          }
          credential = {
            p: request.platform,
            a: request.appId,
            k: toBase64Url(publicKey),
            f: "unattested",
            ...(request.platform === "ios" ? { e: "development" as const } : {}),
          };
        } else if (request.platform === "ios") {
          const attestation = fromBase64(request.attestation);
          const keyId = fromBase64(request.keyId);
          if (attestation === null || keyId === null) return yield* reject("attestation-malformed");
          const key = yield* verified(
            verifyAppAttestation({
              attestation,
              keyId,
              challengeHash,
              appId: `${config.ios!.teamId}.${request.appId}`,
              now: DateTime.makeUnsafe(now),
            }),
          );
          credential = {
            p: "ios",
            a: request.appId,
            k: toBase64Url(key.publicKey),
            f: "app-attest",
            e: key.environment,
          };
        } else {
          const chain = request.certificateChain.map(fromBase64);
          if (chain.some((certificate) => certificate === null)) {
            return yield* reject("attestation-malformed");
          }
          const key = yield* verified(
            verifyAndroidKeyAttestation({
              chain: chain as ReadonlyArray<Uint8Array<ArrayBuffer>>,
              challengeHash,
              app: config.android!.apps.get(request.appId)!,
              bootPolicy: config.android!.bootPolicy,
              revokedSerials: yield* revocations.serials,
              now: DateTime.makeUnsafe(now),
            }),
          );
          credential = {
            p: "android",
            a: request.appId,
            k: toBase64Url(key.publicKey),
            f: "ecdsa",
          };
        }

        const expiresAt = now + CREDENTIAL_TTL_MS;
        return {
          credential: yield* Effect.promise(() =>
            sealer.seal("credential", { ...credential, x: expiresAt }),
          ),
          expiresAt,
        };
      },
    );

    const issueGrants: Gateway["Service"]["issueGrants"] = Effect.fn("Gateway.issueGrants")(
      function* (request) {
        yield* ensureEnabled;
        const now = yield* Clock.currentTimeMillis;
        const opened = decodeCredential(
          yield* Effect.promise(() => sealer.open("credential", request.credential)),
        );
        if (Option.isNone(opened)) {
          return yield* new PushGatewayCredentialInvalidError({ reason: "malformed" });
        }
        const credential = opened.value;
        if (credential.x < now) {
          return yield* new PushGatewayCredentialInvalidError({ reason: "expired" });
        }
        yield* servedApp(credential.p, credential.a);
        const unattested = credential.f === "unattested";
        if (unattested && !config.unattestedAppIds.has(credential.a)) {
          return yield* new PushGatewayCredentialInvalidError({ reason: "revoked" });
        }

        const publicKey = fromBase64(credential.k)!;
        const proof = fromBase64(request.proof);
        const signed = utf8(request.payload);
        const proven =
          proof !== null &&
          (yield* Effect.promise(() =>
            credential.f === "app-attest"
              ? verifyAppAttestAssertion({
                  assertion: proof,
                  publicKey,
                  clientData: signed,
                  appId: `${config.ios!.teamId}.${credential.a}`,
                })
              : verifyEcdsa(publicKey, proof, signed),
          ));
        if (!proven)
          return yield* new PushGatewayCredentialInvalidError({ reason: "proof-invalid" });

        const deviceId = toBase64Url(
          (yield* Effect.promise(() => sha256(publicKey))).subarray(0, 16),
        );
        yield* limiter.check("grants", deviceId);

        const payload = decodeIssueGrantsPayload(request.payload);
        if (Option.isNone(payload)) return yield* invalid("payload-malformed");
        if (!isFresh(payload.value.issuedAt, now)) return yield* invalid("payload-stale");

        const grants: Array<PushIssueGrantsResponse["grants"][number]> = [];
        for (const target of payload.value.targets) {
          if (credential.p === "android" && target.kind !== "notification") {
            return yield* invalid("message-not-allowed");
          }
          if (credential.p === "ios" && !APNS_TOKEN.test(target.token)) {
            return yield* invalid("token-malformed");
          }
          const expiresAt =
            now +
            (target.kind === "liveActivityUpdate" ? LIVE_ACTIVITY_GRANT_TTL_MS : GRANT_TTL_MS);
          const grant: Grant = {
            k: target.kind,
            p: credential.p,
            a: credential.a,
            ...(credential.e === undefined ? {} : { e: credential.e }),
            t: target.token,
            s: payload.value.environmentKey,
            ...(unattested ? { u: true as const } : {}),
            x: expiresAt,
          };
          grants.push({
            kind: target.kind,
            grant: yield* Effect.promise(() => sealer.seal("grant", grant)),
            expiresAt,
          });
        }
        return { grants };
      },
    );

    const deliver: Gateway["Service"]["deliver"] = Effect.fn("Gateway.deliver")(
      function* (request) {
        yield* ensureEnabled;
        const now = yield* Clock.currentTimeMillis;
        const opened = decodeGrant(
          yield* Effect.promise(() => sealer.open("grant", request.grant)),
        );
        if (Option.isNone(opened)) {
          return yield* new PushGatewayGrantInvalidError({ reason: "malformed" });
        }
        const grant = opened.value;
        if (grant.x < now) return yield* new PushGatewayGrantInvalidError({ reason: "expired" });
        if (grant.u === true && !config.unattestedAppIds.has(grant.a)) {
          return yield* new PushGatewayGrantInvalidError({ reason: "revoked" });
        }

        const environmentKey = fromBase64(grant.s)!;
        const signature = fromBase64(request.signature);
        const signed =
          signature !== null &&
          (yield* Effect.promise(() =>
            verifyEd25519(environmentKey, signature, utf8(request.payload)),
          ));
        if (!signed)
          return yield* new PushGatewayGrantInvalidError({ reason: "signature-invalid" });

        // Keyed on the token: fresh device keys cannot buy one target more deliveries.
        const tokenDigest = yield* Effect.promise(() => sha256(utf8(`${grant.a}/${grant.t}`)));
        yield* limiter.check("deliverDevice", toBase64Url(tokenDigest.subarray(0, 16)));
        yield* limiter.check("deliverEnvironment", grant.s);

        const payload = decodeDeliverPayload(request.payload);
        if (Option.isNone(payload)) return yield* invalid("payload-malformed");
        if (!isFresh(payload.value.issuedAt, now)) return yield* invalid("payload-stale");
        const grantDigest = toBase64Url(yield* Effect.promise(() => sha256(utf8(request.grant))));
        if (payload.value.grantDigest !== grantDigest) {
          return yield* new PushGatewayGrantInvalidError({ reason: "digest-mismatch" });
        }
        yield* servedApp(grant.p, grant.a);
        const { message } = payload.value;
        const nowSeconds = Math.floor(now / 1000);

        if (grant.p === "android") {
          if (message.notification === undefined && message.activity === undefined) {
            return yield* invalid("message-not-allowed");
          }
          const outcome = yield* fcm.send({
            android: config.android!,
            packageName: grant.a,
            token: grant.t,
            request: fcmRequest({
              deviceId: message.deviceId,
              environmentId: message.environmentId,
              notification: message.notification,
              activity: message.activity,
              nowMillis: now,
            }),
          });
          return { outcome };
        }

        let apnsRequest;
        if (grant.k === "notification") {
          const { notification } = message;
          if (notification === undefined) return yield* invalid("message-not-allowed");
          apnsRequest = apnsNotificationRequest({
            notification,
            // A thread keeps one alert: its newest news replaces what it said before. A test alert
            // belongs to no thread. APNs caps the id at 64 bytes, and both forms are 43 characters.
            collapseId:
              notification.threadId === ""
                ? notification.id.slice(0, 64)
                : toBase64Url(
                    yield* Effect.promise(() =>
                      sha256(utf8(`${notification.environmentId}/${notification.threadId}`)),
                    ),
                  ),
            nowSeconds,
          });
        } else {
          const state = message.activity?.state;
          if (state === undefined) return yield* invalid("message-not-allowed");
          const alert = message.notification ?? null;
          if (grant.k === "liveActivityStart") {
            if (state === null || state.activeCount === 0) {
              return yield* invalid("message-not-allowed");
            }
            apnsRequest = apnsLiveActivityRequest({
              event: "start",
              environmentId: message.environmentId,
              state,
              // iOS refuses to start an activity from a push without an alert.
              alert: alert ?? { title: state.title, body: state.subtitle, quiet: true },
              nowSeconds,
            });
          } else {
            apnsRequest = apnsLiveActivityRequest({
              // Nothing left running: show the final rows briefly, then let the card go.
              event: state === null || state.activeCount === 0 ? "end" : "update",
              environmentId: message.environmentId,
              state,
              alert,
              nowSeconds,
            });
          }
        }
        const outcome = yield* apns.send({
          ios: config.ios!,
          environment: grant.e ?? "production",
          bundleId: grant.a,
          token: grant.t,
          request: apnsRequest,
        });
        return { outcome };
      },
    );

    return Gateway.of({ challenge, attest, issueGrants, deliver });
  }),
);
