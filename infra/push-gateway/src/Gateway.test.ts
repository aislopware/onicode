import { assert, describe, it } from "@effect/vitest";
import type { PushMessage } from "@onicode/contracts/pushGateway";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as TestClock from "effect/testing/TestClock";

import * as Apns from "./Apns.ts";
import * as AndroidRevocations from "./attestation/AndroidRevocations.ts";
import { sha256, toBase64Url, utf8 } from "./bytes.ts";
import * as Config from "./Config.ts";
import * as Fcm from "./Fcm.ts";
import * as Gateway from "./Gateway.ts";
import * as RateLimiter from "./RateLimiter.ts";

const IOS_APP = "dev.example.app";
const ANDROID_APP = "dev.example.droid";
const APNS_TOKEN = "ab".repeat(32);
const FCM_TOKEN = "fcm-registration-token:".padEnd(64, "x");

const ENV: Config.GatewayEnv = {
  PUSH_SEALING_KEYS: JSON.stringify({ current: "k1", keys: { k1: btoa("0".repeat(32)) } }),
  APNS_TEAM_ID: "TEAM123456",
  APNS_KEY_ID: "KEY1234567",
  APNS_PRIVATE_KEY: "unused",
  IOS_BUNDLE_IDS: IOS_APP,
  ANDROID_APPS: JSON.stringify([
    { packageName: ANDROID_APP, signingCertificateDigests: ["0".repeat(64)] },
  ]),
  FCM_SERVICE_ACCOUNT: JSON.stringify({ project_id: "p", client_email: "e", private_key: "k" }),
  UNATTESTED_APP_IDS: `${IOS_APP},${ANDROID_APP}`,
};

function setup(options?: {
  readonly env?: Partial<Config.GatewayEnv>;
  readonly limited?: ReadonlyArray<RateLimiter.RateLimitBucket>;
  readonly limiterDown?: boolean;
}) {
  const apnsCalls: Array<Parameters<Apns.Apns["Service"]["send"]>[0]> = [];
  const fcmCalls: Array<Parameters<Fcm.Fcm["Service"]["send"]>[0]> = [];
  const limited = new Set(options?.limited ?? []);
  const limitKeys: Array<`${RateLimiter.RateLimitBucket}:${string}`> = [];
  const binding = (bucket: RateLimiter.RateLimitBucket) => ({
    limit: async ({ key }: { readonly key: string }) => {
      limitKeys.push(`${bucket}:${key}`);
      if (options?.limiterDown === true) throw new Error("The rate limit binding is down.");
      return { success: !limited.has(bucket) };
    },
  });
  const layer = Gateway.layer.pipe(
    Layer.provide([
      Layer.succeed(
        Apns.Apns,
        Apns.Apns.of({
          send: (input) => Effect.sync(() => (apnsCalls.push(input), "delivered" as const)),
        }),
      ),
      Layer.succeed(
        Fcm.Fcm,
        Fcm.Fcm.of({
          send: (input) => Effect.sync(() => (fcmCalls.push(input), "delivered" as const)),
        }),
      ),
      Layer.succeed(
        AndroidRevocations.AndroidRevocations,
        AndroidRevocations.AndroidRevocations.of({ serials: Effect.succeed(new Set<string>()) }),
      ),
      RateLimiter.layer({
        challenge: binding("challenge"),
        attest: binding("attest"),
        grants: binding("grants"),
        deliverDevice: binding("deliverDevice"),
        deliverEnvironment: binding("deliverEnvironment"),
      }),
      Config.layer({ ...ENV, ...options?.env }),
    ]),
  );
  return { layer, apnsCalls, fcmCalls, limitKeys };
}

/** WebCrypto signs ECDSA as r‖s; devices send DER. */
function rawToDer(raw: Uint8Array): Uint8Array<ArrayBuffer> {
  const integer = (bytes: Uint8Array) => {
    let start = 0;
    while (start < bytes.length - 1 && bytes[start] === 0) start += 1;
    const body = bytes.subarray(start);
    const padded = body[0]! & 0x80 ? [0, ...body] : [...body];
    return [0x02, padded.length, ...padded];
  };
  const body = [...integer(raw.subarray(0, 32)), ...integer(raw.subarray(32))];
  return Uint8Array.from([0x30, body.length, ...body]);
}

const base64 = (bytes: Uint8Array) => btoa(String.fromCharCode(...bytes));

const makeDevice = Effect.promise(async () => {
  const pair = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, [
    "sign",
    "verify",
  ]);
  const publicKey = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  return {
    publicKey: base64(publicKey),
    prove: async (payload: string) =>
      base64(
        rawToDer(
          new Uint8Array(
            await crypto.subtle.sign(
              { name: "ECDSA", hash: "SHA-256" },
              pair.privateKey,
              utf8(payload),
            ),
          ),
        ),
      ),
  };
});

const makeEnvironment = Effect.promise(async () => {
  const pair = (await crypto.subtle.generateKey({ name: "Ed25519" }, true, [
    "sign",
    "verify",
  ])) as CryptoKeyPair;
  const key = toBase64Url(new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey)));
  return {
    key,
    sign: async (payload: string) =>
      toBase64Url(
        new Uint8Array(
          await crypto.subtle.sign({ name: "Ed25519" }, pair.privateKey, utf8(payload)),
        ),
      ),
  };
});

const NOTIFICATION = {
  id: "alert-1",
  title: "Fix the build",
  body: "Approval: onicode",
  environmentId: "env-1",
  threadId: "thread-1",
  deepLink: "/threads/env-1/thread-1",
} as const;

const activityState = (activeCount: number) =>
  ({
    title: "Onicode",
    subtitle: "1 agent",
    activeCount,
    updatedAt: "2026-11-12T00:00:00.000Z",
    activities: [
      {
        environmentId: "env-1",
        threadId: "thread-1",
        projectTitle: "onicode",
        threadTitle: "Fix the build",
        modelTitle: "fable",
        phase: activeCount > 0 ? "running" : "completed",
        status: activeCount > 0 ? "Working" : "Done",
        updatedAt: "2026-11-12T00:00:00.000Z",
        deepLink: "/threads/env-1/thread-1",
      },
    ],
  }) as const;

/** A registered device: attested without hardware, holding grants for one environment. */
const register = Effect.fn(function* (
  platform: "ios" | "android",
  kinds: ReadonlyArray<"notification" | "liveActivityStart" | "liveActivityUpdate"> = [
    "notification",
  ],
) {
  const gateway = yield* Gateway.Gateway;
  const device = yield* makeDevice;
  const environment = yield* makeEnvironment;
  const { challenge } = yield* gateway.challenge("203.0.113.7");
  const { credential } = yield* gateway.attest(
    {
      platform,
      appId: platform === "ios" ? IOS_APP : ANDROID_APP,
      challenge,
      publicKey: device.publicKey,
    },
    "203.0.113.7",
  );
  const issue = Effect.fn(function* (
    targets: ReadonlyArray<{ kind: (typeof kinds)[number]; token: string }>,
    signer: typeof device = device,
  ) {
    const payload = JSON.stringify({ issuedAt: 0, environmentKey: environment.key, targets });
    const proof = yield* Effect.promise(() => signer.prove(payload));
    return yield* (yield* Gateway.Gateway).issueGrants({ credential, payload, proof });
  });
  const token = platform === "ios" ? APNS_TOKEN : FCM_TOKEN;
  const { grants } = yield* issue(kinds.map((kind) => ({ kind, token })));
  const deliver = Effect.fn(function* (
    grant: string,
    message: Omit<PushMessage, "deviceId" | "environmentId">,
    overrides?: {
      readonly issuedAt?: number;
      readonly signer?: typeof environment;
      readonly digestOf?: string;
    },
  ) {
    const payload = JSON.stringify({
      issuedAt: overrides?.issuedAt ?? 0,
      grantDigest: toBase64Url(
        yield* Effect.promise(() => sha256(utf8(overrides?.digestOf ?? grant))),
      ),
      message: { deviceId: "device-1", environmentId: "env-1", ...message },
    });
    const signature = yield* Effect.promise(() => (overrides?.signer ?? environment).sign(payload));
    return yield* (yield* Gateway.Gateway).deliver({ grant, payload, signature });
  });
  return { gateway, device, environment, credential, grants, issue, deliver };
});

const failureOf = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
  Effect.flip(effect).pipe(Effect.map((error) => JSON.parse(JSON.stringify(error)) as unknown));

describe("Gateway", () => {
  it.effect(
    "delivers an iOS alert to the token a grant seals, on the sandbox it attested for",
    () => {
      const { layer, apnsCalls } = setup();
      return Effect.gen(function* () {
        const { grants, deliver } = yield* register("ios");
        const response = yield* deliver(grants[0]!.grant, { notification: NOTIFICATION });

        assert.deepStrictEqual(response, { outcome: "delivered" });
        assert.strictEqual(apnsCalls.length, 1);
        assert.strictEqual(apnsCalls[0]!.token, APNS_TOKEN);
        assert.strictEqual(apnsCalls[0]!.bundleId, IOS_APP);
        assert.strictEqual(apnsCalls[0]!.environment, "development");
        assert.strictEqual(apnsCalls[0]!.request.pushType, "alert");
      }).pipe(Effect.provide(layer));
    },
  );

  it.effect(
    "lets a thread's newer iOS alert replace its older one, and stacks a machine's threads",
    () => {
      const { layer, apnsCalls } = setup();
      return Effect.gen(function* () {
        const { grants, deliver } = yield* register("ios");
        const alert = (id: string, threadId: string) =>
          deliver(grants[0]!.grant, { notification: { ...NOTIFICATION, id, threadId } });
        yield* alert("approval", "thread-1");
        yield* alert("completion", "thread-1");
        yield* alert("elsewhere", "thread-2");
        yield* alert("test-alert", "");

        const [approval, completion, elsewhere, test] = apnsCalls.map((call) => call.request);
        assert.strictEqual(approval!.collapseId, completion!.collapseId);
        assert.notStrictEqual(approval!.collapseId, elsewhere!.collapseId);
        assert.strictEqual(test!.collapseId, "test-alert");
        assert.isAtMost(approval!.collapseId!.length, 64);
        assert.deepStrictEqual(
          apnsCalls.map(
            (call) => (call.request.payload as { aps: Record<string, unknown> }).aps["thread-id"],
          ),
          ["env-1", "env-1", "env-1", "env-1"],
        );
      }).pipe(Effect.provide(layer));
    },
  );

  it.effect("starts, updates and ends a Live Activity through the matching grants", () => {
    const { layer, apnsCalls } = setup();
    return Effect.gen(function* () {
      const { grants, deliver } = yield* register("ios", [
        "liveActivityStart",
        "liveActivityUpdate",
      ]);
      yield* deliver(grants[0]!.grant, { activity: { state: activityState(1) } });
      yield* deliver(grants[1]!.grant, { activity: { state: activityState(1) } });
      yield* deliver(grants[1]!.grant, { activity: { state: activityState(0) } });
      yield* deliver(grants[1]!.grant, { activity: { state: null } });

      const events = apnsCalls.map(
        (call) => (call.request.payload as { aps: { event: string } }).aps.event,
      );
      assert.deepStrictEqual(events, ["start", "update", "end", "end"]);
      // Work beginning is not news: the start's mandatory banner stays silent.
      assert.deepStrictEqual(
        (apnsCalls[0]!.request.payload as { aps: { alert: unknown } }).aps.alert,
        { title: "Onicode", body: "1 agent" },
      );
      assert.isTrue(apnsCalls.every((call) => call.request.pushType === "liveactivity"));
      // A start grant has no activity to end, and nothing to start once work is over.
      assert.deepStrictEqual(
        yield* failureOf(deliver(grants[0]!.grant, { activity: { state: activityState(0) } })),
        { _tag: "PushGatewayInvalidRequestError", reason: "message-not-allowed" },
      );
    }).pipe(Effect.provide(layer));
  });

  it.effect("delivers an Android alert and activity as one FCM data message", () => {
    const { layer, fcmCalls } = setup();
    return Effect.gen(function* () {
      const { grants, deliver } = yield* register("android");
      yield* deliver(grants[0]!.grant, {
        notification: NOTIFICATION,
        activity: { state: activityState(1), expiresAt: 5_000 },
      });

      assert.strictEqual(fcmCalls.length, 1);
      assert.strictEqual(fcmCalls[0]!.token, FCM_TOKEN);
      assert.strictEqual(fcmCalls[0]!.packageName, ANDROID_APP);
      assert.include(fcmCalls[0]!.request.data, {
        device_id: "device-1",
        environment_id: "env-1",
        alert_title: "Fix the build",
        alert_path: "/threads/env-1/thread-1",
        active: "true",
        activity_expires_at: "5000",
      });
    }).pipe(Effect.provide(layer));
  });

  it.effect("refuses a delivery that the grant's environment did not sign for that grant", () => {
    const { layer, apnsCalls } = setup();
    return Effect.gen(function* () {
      const first = yield* register("ios");
      const stranger = yield* makeEnvironment;
      const grant = first.grants[0]!.grant;
      const message = { notification: NOTIFICATION };
      const grantError = (reason: string) => ({ _tag: "PushGatewayGrantInvalidError", reason });

      assert.deepStrictEqual(
        yield* failureOf(first.deliver(grant, message, { signer: stranger })),
        grantError("signature-invalid"),
      );
      assert.deepStrictEqual(
        yield* failureOf(first.deliver(grant, message, { digestOf: "another-grant" })),
        grantError("digest-mismatch"),
      );
      assert.deepStrictEqual(
        yield* failureOf(first.deliver(`${grant.slice(0, -4)}AAAA`, message)),
        grantError("malformed"),
      );
      // A credential is sealed for another purpose and must not open as a grant.
      assert.deepStrictEqual(
        yield* failureOf(first.deliver(first.credential, message)),
        grantError("malformed"),
      );
      assert.deepStrictEqual(
        yield* failureOf(first.deliver(grant, message, { issuedAt: 6 * 60_000 })),
        { _tag: "PushGatewayInvalidRequestError", reason: "payload-stale" },
      );
      assert.deepStrictEqual(
        yield* failureOf(first.deliver(grant, { activity: { state: null } })),
        { _tag: "PushGatewayInvalidRequestError", reason: "message-not-allowed" },
      );

      yield* TestClock.adjust("61 days");
      assert.deepStrictEqual(
        yield* failureOf(first.deliver(grant, message, { issuedAt: 61 * 24 * 60 * 60_000 })),
        grantError("expired"),
      );
      assert.strictEqual(apnsCalls.length, 0);
    }).pipe(Effect.provide(layer));
  });

  it.effect("issues grants only to the device key the credential attests", () => {
    const { layer } = setup();
    return Effect.gen(function* () {
      const ios = yield* register("ios");
      const android = yield* register("android");
      const other = yield* makeDevice;
      const target = { kind: "notification", token: APNS_TOKEN } as const;

      assert.deepStrictEqual(yield* failureOf(ios.issue([target], other)), {
        _tag: "PushGatewayCredentialInvalidError",
        reason: "proof-invalid",
      });
      assert.deepStrictEqual(
        yield* failureOf(ios.issue([{ kind: "notification", token: "not-a-hex-token".repeat(4) }])),
        { _tag: "PushGatewayInvalidRequestError", reason: "token-malformed" },
      );
      assert.deepStrictEqual(
        yield* failureOf(android.issue([{ kind: "liveActivityStart", token: FCM_TOKEN }])),
        { _tag: "PushGatewayInvalidRequestError", reason: "message-not-allowed" },
      );
      assert.deepStrictEqual(
        yield* failureOf(
          ios.gateway.issueGrants({
            credential: ios.grants[0]!.grant,
            payload: "{}",
            proof: "AAAA",
          }),
        ),
        { _tag: "PushGatewayCredentialInvalidError", reason: "malformed" },
      );
    }).pipe(Effect.provide(layer));
  });

  it.effect("attests without hardware only for the app ids it lists", () => {
    const { layer } = setup({ env: { UNATTESTED_APP_IDS: ANDROID_APP } });
    return Effect.gen(function* () {
      const gateway = yield* Gateway.Gateway;
      const device = yield* makeDevice;
      const { challenge } = yield* gateway.challenge("203.0.113.7");
      const request = { platform: "ios", challenge, publicKey: device.publicKey } as const;

      assert.deepStrictEqual(
        yield* failureOf(gateway.attest({ ...request, appId: IOS_APP }, "203.0.113.7")),
        { _tag: "PushGatewayAttestationRejectedError", reason: "device-untrusted" },
      );
      assert.deepStrictEqual(
        yield* failureOf(gateway.attest({ ...request, appId: "dev.example.other" }, "203.0.113.7")),
        { _tag: "PushGatewayInvalidRequestError", reason: "app-not-served" },
      );
      assert.deepStrictEqual(
        yield* failureOf(
          gateway.attest({ ...request, appId: IOS_APP, challenge: "AAAA" }, "203.0.113.7"),
        ),
        { _tag: "PushGatewayAttestationRejectedError", reason: "challenge-invalid" },
      );
      yield* TestClock.adjust("6 minutes");
      assert.deepStrictEqual(
        yield* failureOf(
          gateway.attest({ ...request, platform: "android", appId: ANDROID_APP }, "203.0.113.7"),
        ),
        { _tag: "PushGatewayAttestationRejectedError", reason: "challenge-expired" },
      );
    }).pipe(Effect.provide(layer));
  });

  it.effect("stops honouring an install without hardware once its app id is delisted", () => {
    const listed = setup();
    const delisted = setup({ env: { UNATTESTED_APP_IDS: "" } });
    return Effect.gen(function* () {
      const { grants, issue, deliver } = yield* register("ios").pipe(Effect.provide(listed.layer));

      assert.deepStrictEqual(
        yield* failureOf(
          issue([{ kind: "notification", token: APNS_TOKEN }]).pipe(Effect.provide(delisted.layer)),
        ),
        { _tag: "PushGatewayCredentialInvalidError", reason: "revoked" },
      );
      assert.deepStrictEqual(
        yield* failureOf(
          deliver(grants[0]!.grant, { notification: NOTIFICATION }).pipe(
            Effect.provide(delisted.layer),
          ),
        ),
        { _tag: "PushGatewayGrantInvalidError", reason: "revoked" },
      );
      assert.strictEqual(delisted.apnsCalls.length, 0);
    });
  });

  it.effect("stops a rate-limited delivery before it reaches the provider", () => {
    const { layer, apnsCalls } = setup({ limited: ["deliverDevice"] });
    return Effect.gen(function* () {
      const { grants, deliver } = yield* register("ios");
      assert.deepStrictEqual(
        yield* failureOf(deliver(grants[0]!.grant, { notification: NOTIFICATION })),
        { _tag: "PushGatewayRateLimitedError", retryAfterSeconds: 60 },
      );
      assert.strictEqual(apnsCalls.length, 0);
    }).pipe(Effect.provide(layer));
  });

  it.effect("keeps delivering while the rate limiter itself is down", () => {
    const { layer, apnsCalls } = setup({ limiterDown: true });
    return Effect.gen(function* () {
      const { grants, deliver } = yield* register("ios");
      yield* deliver(grants[0]!.grant, { notification: NOTIFICATION });
      assert.strictEqual(apnsCalls.length, 1);
    }).pipe(Effect.provide(layer));
  });

  it.effect("limits by what an abuser cannot rotate: the IPv6 subscriber and the token", () => {
    const { layer, limitKeys } = setup();
    return Effect.gen(function* () {
      const gateway = yield* Gateway.Gateway;
      yield* gateway.challenge("2001:db8:1:2:aaaa::1");
      yield* gateway.challenge("2001:DB8:1:2::9");
      yield* gateway.challenge("2001:db8:1:3::9");
      yield* gateway.challenge("::1");
      assert.deepStrictEqual(limitKeys.splice(0), [
        "challenge:2001:db8:1:2::/64",
        "challenge:2001:db8:1:2::/64",
        "challenge:2001:db8:1:3::/64",
        "challenge:0:0:0:0::/64",
      ]);

      // Two installs that claim the same token share its delivery budget.
      const first = yield* register("ios");
      const second = yield* register("ios");
      limitKeys.splice(0);
      yield* first.deliver(first.grants[0]!.grant, { notification: NOTIFICATION });
      yield* second.deliver(second.grants[0]!.grant, { notification: NOTIFICATION });
      const deviceKeys = limitKeys.filter((key) => key.startsWith("deliverDevice:"));
      assert.strictEqual(deviceKeys.length, 2);
      assert.strictEqual(deviceKeys[0], deviceKeys[1]);
    }).pipe(Effect.provide(layer));
  });

  it.effect("refuses everything while disabled", () => {
    const { layer } = setup({ env: { PUSH_DISABLED: "true" } });
    return Effect.gen(function* () {
      const gateway = yield* Gateway.Gateway;
      assert.deepStrictEqual(yield* failureOf(gateway.challenge("203.0.113.7")), {
        _tag: "PushGatewayUnavailableError",
        reason: "disabled",
      });
    }).pipe(Effect.provide(layer));
  });
});
