import { describe, expect, it } from "@effect/vitest";
import {
  EnvironmentId,
  type PushDevicePreferences,
  type PushRegisterDeviceRequest,
} from "@onicode/contracts";
import {
  PushGatewayAttestationRejectedError,
  PushGatewayCredentialInvalidError,
  type PushAttestDeviceRequest,
  type PushIssueGrantsPayload,
} from "@onicode/contracts/pushGateway";
import * as Effect from "effect/Effect";
import * as TestClock from "effect/testing/TestClock";

import {
  CREDENTIAL_RENEWAL_MS,
  EMPTY_PUSH_REGISTRATION_STATE,
  grantIsFresh,
  makePushRegistrar,
  planPushSync,
  PushEnvironmentRequestError,
  type PushDeviceKeys,
  type PushEnvironmentClient,
  type PushGatewayClient,
  type PushRegistrarConfig,
  type PushRegistrationState,
} from "./pushRegistration";

const DAY = 24 * 60 * 60_000;
const START = 1_800_000_000_000;
const GATEWAY = "https://push.example.test";
const KEY_A = "A".repeat(43);
const KEY_B = "B".repeat(43);
const ENV_A = EnvironmentId.make("environment-a");
const ENV_B = EnvironmentId.make("environment-b");
const PREFERENCES: PushDevicePreferences = {
  notifyOnApproval: true,
  notifyOnInput: true,
  notifyOnCompletion: true,
  notifyOnFailure: true,
  activity: true,
};
const NOTIFICATION_TOKEN = "a".repeat(64);
const START_TOKEN = "b".repeat(64);

function fakeGateway(options: { rejectHardware?: boolean } = {}) {
  const calls = {
    attest: [] as Array<PushAttestDeviceRequest>,
    grants: [] as Array<PushIssueGrantsPayload & { credential: string }>,
  };
  let credentials = 0;
  let grants = 0;
  const invalidCredentials = new Set<string>();
  const client: PushGatewayClient = {
    challenge: Effect.succeed({ challenge: "challenge", expiresAt: START + 300_000 }),
    attest: (request) =>
      Effect.gen(function* () {
        calls.attest.push(request);
        if (options.rejectHardware && !("publicKey" in request)) {
          return yield* new PushGatewayAttestationRejectedError({ reason: "chain-untrusted" });
        }
        const now = yield* Effect.clockWith((clock) => clock.currentTimeMillis);
        credentials += 1;
        return { credential: `credential-${credentials}`, expiresAt: now + 180 * DAY };
      }),
    issueGrants: (request) =>
      Effect.gen(function* () {
        if (invalidCredentials.has(request.credential)) {
          return yield* new PushGatewayCredentialInvalidError({ reason: "expired" });
        }
        const payload = JSON.parse(request.payload) as PushIssueGrantsPayload;
        calls.grants.push({ ...payload, credential: request.credential });
        const now = yield* Effect.clockWith((clock) => clock.currentTimeMillis);
        return {
          grants: payload.targets.map((target) => {
            grants += 1;
            return {
              kind: target.kind,
              grant: `grant-${grants}`,
              expiresAt: now + (target.kind === "liveActivityUpdate" ? 12 * 60 * 60_000 : 60 * DAY),
            };
          }),
        };
      }),
  };
  return { client, calls, invalidCredentials };
}

const keys = (hardware = true): PushDeviceKeys => ({
  hardwareAttestationSupported: hardware,
  attestHardwareKey: () => Effect.succeed({ keyId: "a2V5", attestation: "YXR0ZXN0" }),
  signWithHardwareKey: () => Effect.succeed("YXNzZXJ0"),
  createSoftwareKey: Effect.succeed("BAAA"),
  signWithSoftwareKey: () => Effect.succeed("c2lnbg=="),
});

function fakeEnvironment(environmentKey = KEY_A, gatewayUrls = [GATEWAY]) {
  const calls = {
    registration: 0,
    register: [] as Array<PushRegisterDeviceRequest>,
    liveActivity: [] as Array<string>,
    unregister: 0,
  };
  const current = { environmentKey, gatewayUrls, reachable: true };
  const client: PushEnvironmentClient = {
    registration: Effect.suspend(() => {
      calls.registration += 1;
      return current.reachable
        ? Effect.succeed({
            environmentKey: current.environmentKey,
            gatewayUrls: current.gatewayUrls,
          })
        : Effect.fail(new PushEnvironmentRequestError({ message: "offline" }));
    }),
    register: (_deviceId, request) =>
      Effect.sync(() => {
        calls.register.push(request);
      }),
    registerLiveActivity: (_deviceId, grant) =>
      Effect.sync(() => {
        calls.liveActivity.push(grant.grant);
      }),
    unregister: () =>
      Effect.sync(() => {
        calls.unregister += 1;
      }),
    sendTest: () => Effect.succeed({ outcome: "delivered" }),
  };
  return { client, calls, current };
}

const CONFIG: PushRegistrarConfig = {
  platform: "ios",
  appId: "dev.aislopware.onicode",
  gatewayUrl: GATEWAY,
  deviceId: "device-1234",
  label: "Test iPhone",
  allowUnattested: false,
};

function setup(
  options: {
    readonly config?: Partial<PushRegistrarConfig>;
    readonly keys?: PushDeviceKeys;
    readonly gateway?: ReturnType<typeof fakeGateway>;
    readonly stored?: { current: PushRegistrationState };
  } = {},
) {
  const gateway = options.gateway ?? fakeGateway();
  const stored = options.stored ?? { current: EMPTY_PUSH_REGISTRATION_STATE };
  return Effect.gen(function* () {
    yield* TestClock.setTime(START);
    const registrar = yield* makePushRegistrar({
      config: { ...CONFIG, ...options.config },
      gateway: gateway.client,
      keys: options.keys ?? keys(),
      load: Effect.sync(() => stored.current),
      save: (state) =>
        Effect.sync(() => {
          stored.current = state;
        }),
    });
    return { registrar, gateway, stored };
  });
}

const sync = (
  registrar: Effect.Success<ReturnType<typeof setup>>["registrar"],
  client: PushEnvironmentClient,
  tokens: { notification: string | null; liveActivityStart: string | null } = {
    notification: NOTIFICATION_TOKEN,
    liveActivityStart: START_TOKEN,
  },
  environmentId = ENV_A,
  preferences = PREFERENCES,
) => registrar.syncEnvironment({ environmentId, server: client, tokens, preferences });

describe("grantIsFresh", () => {
  it("refreshes on a new token or with a third of the lifetime left", () => {
    const grant = { token: "t", grant: "g", issuedAt: 0, expiresAt: 90 };
    expect(grantIsFresh(grant, "t", 59)).toBe(true);
    expect(grantIsFresh(grant, "t", 61)).toBe(false);
    expect(grantIsFresh(grant, "other", 0)).toBe(false);
    expect(grantIsFresh(undefined, "t", 0)).toBe(false);
  });
});

describe("planPushSync", () => {
  const ids = (...values: Array<string>) => values.map((value) => EnvironmentId.make(value));
  it("registers connected environments and drops only removed ones", () => {
    expect(
      planPushSync({
        enabled: true,
        saved: new Set(ids("connected", "disabled")),
        connected: ids("connected"),
        known: ids("connected", "disabled", "removed"),
      }),
    ).toEqual({ sync: ids("connected"), unregister: ids("removed") });
  });

  it("drops every registration after opting out", () => {
    expect(
      planPushSync({
        enabled: false,
        saved: new Set(ids("a", "b")),
        connected: ids("a", "b"),
        known: ids("a", "b"),
      }),
    ).toEqual({ sync: [], unregister: ids("a", "b") });
  });
});

describe("makePushRegistrar", () => {
  it.effect("attests once and registers alert and push-to-start grants", () =>
    Effect.gen(function* () {
      const { registrar, gateway } = yield* setup();
      const environment = fakeEnvironment();

      expect(yield* sync(registrar, environment.client)).toEqual({ _tag: "registered" });

      expect(gateway.calls.attest).toEqual([
        {
          platform: "ios",
          appId: CONFIG.appId,
          challenge: "challenge",
          keyId: "a2V5",
          attestation: "YXR0ZXN0",
        },
      ]);
      expect(gateway.calls.grants).toEqual([
        {
          issuedAt: START,
          environmentKey: KEY_A,
          credential: "credential-1",
          targets: [
            { kind: "notification", token: NOTIFICATION_TOKEN },
            { kind: "liveActivityStart", token: START_TOKEN },
          ],
        },
      ]);
      expect(environment.calls.register).toEqual([
        {
          platform: "ios",
          label: "Test iPhone",
          gatewayUrl: GATEWAY,
          notification: { grant: "grant-1", expiresAt: START + 60 * DAY },
          liveActivityStart: { grant: "grant-2", expiresAt: START + 60 * DAY },
          preferences: PREFERENCES,
        },
      ]);
    }),
  );

  it.effect("reuses the credential and grants, registering again only after a relaunch", () =>
    Effect.gen(function* () {
      const { registrar, gateway, stored } = yield* setup();
      const environment = fakeEnvironment();
      yield* sync(registrar, environment.client);
      yield* sync(registrar, environment.client);
      expect(environment.calls.register).toHaveLength(1);

      const relaunched = yield* setup({ gateway, stored });
      yield* sync(relaunched.registrar, environment.client);

      expect(gateway.calls.attest).toHaveLength(1);
      expect(gateway.calls.grants).toHaveLength(1);
      expect(environment.calls.register).toHaveLength(2);
    }),
  );

  it.effect("renews the credential within 30 days of expiry", () =>
    Effect.gen(function* () {
      const { registrar, gateway } = yield* setup();
      const environment = fakeEnvironment();
      yield* sync(registrar, environment.client);

      yield* TestClock.setTime(START + 180 * DAY - CREDENTIAL_RENEWAL_MS - 1);
      yield* sync(registrar, environment.client, {
        notification: "c".repeat(64),
        liveActivityStart: null,
      });
      expect(gateway.calls.attest).toHaveLength(1);

      yield* TestClock.setTime(START + 180 * DAY - CREDENTIAL_RENEWAL_MS + 1);
      yield* sync(registrar, environment.client, {
        notification: "d".repeat(64),
        liveActivityStart: null,
      });
      expect(gateway.calls.attest).toHaveLength(2);
      expect(gateway.calls.grants.at(-1)?.credential).toBe("credential-2");
    }),
  );

  it.effect("re-attests and retries once when the gateway refuses the credential", () =>
    Effect.gen(function* () {
      const { registrar, gateway } = yield* setup();
      gateway.invalidCredentials.add("credential-1");

      expect(yield* sync(registrar, fakeEnvironment().client)).toEqual({ _tag: "registered" });
      expect(gateway.calls.attest).toHaveLength(2);
      expect(gateway.calls.grants.map((call) => call.credential)).toEqual(["credential-2"]);
    }),
  );

  it.effect("refreshes a grant when its token changes or it nears expiry", () =>
    Effect.gen(function* () {
      const { registrar, gateway } = yield* setup();
      const environment = fakeEnvironment();
      const tokens = { notification: NOTIFICATION_TOKEN, liveActivityStart: null };
      yield* sync(registrar, environment.client, tokens);

      yield* TestClock.setTime(START + 39 * DAY);
      yield* sync(registrar, environment.client, tokens);
      expect(gateway.calls.grants).toHaveLength(1);

      yield* TestClock.setTime(START + 41 * DAY);
      yield* sync(registrar, environment.client, tokens);
      expect(gateway.calls.grants).toHaveLength(2);

      const rotated = "e".repeat(64);
      yield* sync(registrar, environment.client, {
        notification: rotated,
        liveActivityStart: null,
      });
      expect(gateway.calls.grants.at(-1)?.targets).toEqual([
        { kind: "notification", token: rotated },
      ]);
      expect(environment.calls.register.at(-1)?.notification?.grant).toBe("grant-3");
    }),
  );

  it.effect("drops the push-to-start grant when activity updates are turned off", () =>
    Effect.gen(function* () {
      const { registrar, gateway } = yield* setup();
      const environment = fakeEnvironment();
      yield* sync(registrar, environment.client);
      yield* sync(registrar, environment.client, undefined, ENV_A, {
        ...PREFERENCES,
        activity: false,
      });

      expect(gateway.calls.grants).toHaveLength(1);
      expect(environment.calls.register.at(-1)).toMatchObject({
        notification: { grant: "grant-1" },
        liveActivityStart: null,
        preferences: { activity: false },
      });
    }),
  );

  it.effect("never asks Android for Live Activity grants", () =>
    Effect.gen(function* () {
      const { registrar, gateway } = yield* setup({ config: { platform: "android" } });
      yield* sync(registrar, fakeEnvironment().client, {
        notification: "fcm-token-".repeat(4),
        liveActivityStart: START_TOKEN,
      });
      expect(gateway.calls.grants[0]?.targets.map((target) => target.kind)).toEqual([
        "notification",
      ]);
    }),
  );

  it.effect("registers each environment with grants bound to its own key", () =>
    Effect.gen(function* () {
      const { registrar, gateway } = yield* setup();
      yield* sync(registrar, fakeEnvironment(KEY_A).client, undefined, ENV_A);
      yield* sync(registrar, fakeEnvironment(KEY_B).client, undefined, ENV_B);

      expect(gateway.calls.attest).toHaveLength(1);
      expect(gateway.calls.grants.map((call) => call.environmentKey)).toEqual([KEY_A, KEY_B]);
    }),
  );

  it.effect("reissues grants when an environment's key changes", () =>
    Effect.gen(function* () {
      const { registrar, gateway } = yield* setup();
      const environment = fakeEnvironment(KEY_A);
      yield* sync(registrar, environment.client);
      environment.current.environmentKey = KEY_B;
      yield* sync(registrar, environment.client);

      expect(gateway.calls.grants.map((call) => call.environmentKey)).toEqual([KEY_A, KEY_B]);
    }),
  );

  it.effect("does not register with an environment that delivers through another gateway", () =>
    Effect.gen(function* () {
      const { registrar, gateway } = yield* setup();
      const environment = fakeEnvironment(KEY_A, ["https://other.example.test"]);

      expect(yield* sync(registrar, environment.client)).toEqual({ _tag: "gateway-not-allowed" });
      expect(gateway.calls.attest).toHaveLength(0);
      expect(environment.calls.register).toHaveLength(0);
      expect(
        yield* registrar.registerLiveActivity({
          environmentId: ENV_A,
          server: environment.client,
          token: "f".repeat(64),
        }),
      ).toBeNull();
    }),
  );

  it.effect("reports an environment it cannot reach", () =>
    Effect.gen(function* () {
      const { registrar } = yield* setup();
      const environment = fakeEnvironment();
      environment.current.reachable = false;

      expect(yield* sync(registrar, environment.client)).toEqual({
        _tag: "unreachable",
        message: "offline",
      });
    }),
  );

  it.effect("unregisters and registers again from scratch on the next sync", () =>
    Effect.gen(function* () {
      const { registrar, stored } = yield* setup();
      const environment = fakeEnvironment();
      yield* sync(registrar, environment.client);

      yield* registrar.unregisterEnvironment({ environmentId: ENV_A, server: environment.client });
      expect(environment.calls.unregister).toBe(1);
      expect(stored.current.environments).toEqual({});
      expect(yield* registrar.knownEnvironmentIds).toEqual([]);

      yield* sync(registrar, environment.client);
      expect(environment.calls.register).toHaveLength(2);
    }),
  );

  it.effect("registers a Live Activity with the cached environment key", () =>
    Effect.gen(function* () {
      const { registrar, gateway } = yield* setup();
      const environment = fakeEnvironment();
      yield* sync(registrar, environment.client);

      const token = "f".repeat(64);
      yield* registrar.registerLiveActivity({
        environmentId: ENV_A,
        server: environment.client,
        token,
      });

      expect(environment.calls.registration).toBe(1);
      expect(gateway.calls.grants.at(-1)?.targets).toEqual([{ kind: "liveActivityUpdate", token }]);
      expect(environment.calls.liveActivity).toEqual(["grant-3"]);
    }),
  );

  it.effect("falls back to an unattested key only where the build allows it", () =>
    Effect.gen(function* () {
      const refused = yield* setup({ gateway: fakeGateway({ rejectHardware: true }) });
      const error = yield* Effect.flip(sync(refused.registrar, fakeEnvironment().client));
      expect(error).toMatchObject({ _tag: "PushUnavailableError", reason: "attestation-rejected" });

      const development = yield* setup({
        gateway: fakeGateway({ rejectHardware: true }),
        config: { allowUnattested: true },
      });
      expect(yield* sync(development.registrar, fakeEnvironment().client)).toEqual({
        _tag: "registered",
      });
      expect(development.gateway.calls.attest.at(-1)).toEqual({
        platform: "ios",
        appId: CONFIG.appId,
        challenge: "challenge",
        publicKey: "BAAA",
      });
      expect(development.stored.current.credential?.key).toEqual({ kind: "software" });

      const simulator = yield* setup({ keys: keys(false), config: { allowUnattested: true } });
      yield* sync(simulator.registrar, fakeEnvironment().client);
      expect(simulator.gateway.calls.attest).toHaveLength(1);
      expect("publicKey" in simulator.gateway.calls.attest[0]!).toBe(true);
    }),
  );
});
