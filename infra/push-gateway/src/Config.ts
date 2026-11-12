import { PushAppId } from "@onicode/contracts/pushGateway";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Redacted from "effect/Redacted";
import * as Schema from "effect/Schema";

import type { AndroidApp, AndroidBootPolicy } from "./attestation/AndroidKey.ts";
import { makeSealer, type Sealer, SealingKeys } from "./Sealing.ts";

/** The Worker's vars and secrets. Every value is optional so a gateway can serve one platform. */
export const GatewayEnv = Schema.Struct({
  /** `true` refuses every request, for an incident. */
  PUSH_DISABLED: Schema.optional(Schema.String),
  PUSH_SEALING_KEYS: Schema.optional(Schema.String),
  APNS_TEAM_ID: Schema.optional(Schema.String),
  APNS_KEY_ID: Schema.optional(Schema.String),
  APNS_PRIVATE_KEY: Schema.optional(Schema.String),
  /** Comma-separated bundle identifiers the APNs key may push to. */
  IOS_BUNDLE_IDS: Schema.optional(Schema.String),
  ANDROID_APPS: Schema.optional(Schema.String),
  ANDROID_BOOT_POLICY: Schema.optional(Schema.String),
  FCM_SERVICE_ACCOUNT: Schema.optional(Schema.String),
  /** Comma-separated app ids that may register without attestation. Leave unset in production. */
  UNATTESTED_APP_IDS: Schema.optional(Schema.String),
});
export type GatewayEnv = typeof GatewayEnv.Type;

const AndroidApps = Schema.fromJsonString(
  Schema.Array(
    Schema.Struct({
      packageName: PushAppId,
      signingCertificateDigests: Schema.Array(
        Schema.String.check(Schema.isPattern(/^[0-9a-f]{64}$/)),
      ).check(Schema.isMinLength(1)),
    }),
  ).check(Schema.isMinLength(1)),
);

const FcmServiceAccount = Schema.fromJsonString(
  Schema.Struct({
    project_id: Schema.NonEmptyString,
    client_email: Schema.NonEmptyString,
    private_key: Schema.NonEmptyString,
  }),
);

const BootPolicy = Schema.Literals(["verified", "locked", "any"]);

export class GatewayConfigError extends Schema.TaggedError<GatewayConfigError>()(
  "GatewayConfigError",
  { setting: Schema.String, detail: Schema.String },
) {
  override get message(): string {
    return `Push gateway setting ${this.setting} is invalid: ${this.detail}`;
  }
}

export interface IosConfig {
  readonly teamId: string;
  readonly bundleIds: ReadonlySet<string>;
  readonly apnsKeyId: string;
  readonly apnsPrivateKey: Redacted.Redacted<string>;
}

export interface AndroidConfig {
  readonly apps: ReadonlyMap<string, AndroidApp>;
  readonly bootPolicy: AndroidBootPolicy;
  readonly fcm: {
    readonly projectId: string;
    readonly clientEmail: string;
    readonly privateKey: Redacted.Redacted<string>;
  };
}

export class GatewayConfig extends Context.Service<
  GatewayConfig,
  {
    readonly disabled: boolean;
    readonly sealer: Sealer;
    readonly ios: IosConfig | null;
    readonly android: AndroidConfig | null;
    readonly unattestedAppIds: ReadonlySet<string>;
  }
>()("@onicode/push-gateway/Config/GatewayConfig") {}

const list = (value: string | undefined): ReadonlySet<string> =>
  new Set(
    (value ?? "")
      .split(",")
      .map((entry) => entry.trim())
      .filter((entry) => entry.length > 0),
  );

const decodeSetting = <A>(setting: string, schema: Schema.Codec<A, string>, value: string) =>
  Schema.decodeEffect(schema)(value).pipe(
    Effect.mapError((cause) => new GatewayConfigError({ setting, detail: cause.message })),
  );

const fromEnv = Effect.fn("GatewayConfig.fromEnv")(function* (env: GatewayEnv) {
  if (env.PUSH_SEALING_KEYS === undefined) {
    return yield* new GatewayConfigError({ setting: "PUSH_SEALING_KEYS", detail: "not set" });
  }
  const sealingKeys = yield* decodeSetting(
    "PUSH_SEALING_KEYS",
    Schema.fromJsonString(SealingKeys),
    env.PUSH_SEALING_KEYS,
  );
  const sealer = yield* Effect.try({
    try: () => makeSealer(sealingKeys),
    catch: (cause) =>
      new GatewayConfigError({
        setting: "PUSH_SEALING_KEYS",
        detail: cause instanceof Error ? cause.message : "unusable",
      }),
  });

  const bundleIds = list(env.IOS_BUNDLE_IDS);
  let ios: IosConfig | null = null;
  if (bundleIds.size > 0) {
    const { APNS_TEAM_ID: teamId, APNS_KEY_ID: apnsKeyId, APNS_PRIVATE_KEY: key } = env;
    if (!teamId || !apnsKeyId || !key) {
      return yield* new GatewayConfigError({
        setting: "IOS_BUNDLE_IDS",
        detail: "needs APNS_TEAM_ID, APNS_KEY_ID and APNS_PRIVATE_KEY",
      });
    }
    ios = { teamId, bundleIds, apnsKeyId, apnsPrivateKey: Redacted.make(key) };
  }

  let android: AndroidConfig | null = null;
  if (env.ANDROID_APPS !== undefined) {
    const apps = yield* decodeSetting("ANDROID_APPS", AndroidApps, env.ANDROID_APPS);
    if (env.FCM_SERVICE_ACCOUNT === undefined) {
      return yield* new GatewayConfigError({
        setting: "ANDROID_APPS",
        detail: "needs FCM_SERVICE_ACCOUNT",
      });
    }
    const account = yield* decodeSetting(
      "FCM_SERVICE_ACCOUNT",
      FcmServiceAccount,
      env.FCM_SERVICE_ACCOUNT,
    );
    const bootPolicy = yield* decodeSetting(
      "ANDROID_BOOT_POLICY",
      BootPolicy,
      env.ANDROID_BOOT_POLICY ?? "verified",
    );
    android = {
      apps: new Map(apps.map((app) => [app.packageName, app])),
      bootPolicy,
      fcm: {
        projectId: account.project_id,
        clientEmail: account.client_email,
        privateKey: Redacted.make(account.private_key),
      },
    };
  }

  return GatewayConfig.of({
    disabled: env.PUSH_DISABLED === "true",
    sealer,
    ios,
    android,
    unattestedAppIds: list(env.UNATTESTED_APP_IDS),
  });
});

export const layer = (env: GatewayEnv) => Layer.effect(GatewayConfig, fromEnv(env));
