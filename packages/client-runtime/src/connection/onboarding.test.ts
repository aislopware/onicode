import {
  AuthStandardClientScopes,
  EnvironmentId,
  type AuthEnvironmentScope,
  ORCHESTRATION_PROTOCOL_VERSION,
} from "@onicode/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Ref from "effect/Ref";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";

import * as RpcHttp from "../rpc/http.ts";
import * as ClientCapabilities from "../platform/capabilities.ts";
import * as Persistence from "../platform/persistence.ts";
import {
  type ConnectionCatalogDocument,
  EMPTY_CONNECTION_CATALOG_DOCUMENT,
  registerConnectionInCatalog,
  removeCatalogValue,
  removeConnectionFromCatalog,
  replaceCatalogValue,
  setConnectionEnabledInCatalog,
  setRoutesInCatalog,
} from "../platform/storageDocument.ts";
import { fetchRemoteSessionState } from "../authorization/remote.ts";
import {
  BearerConnectionCredential,
  BearerConnectionProfile,
  BearerConnectionRegistration,
  PrimaryConnectionRegistration,
} from "./catalog.ts";
import * as Connectivity from "./connectivity.ts";
import * as ConnectionCredentialStore from "./credentialStore.ts";
import * as ConnectionDriver from "./driver.ts";
import {
  BearerConnectionTarget,
  ConnectionBlockedError,
  PrimaryConnectionTarget,
} from "./model.ts";
import * as Onboarding from "./onboarding.ts";
import {
  prepareBearerConnectionUpdate,
  preparePairingRegistration,
  prepareSshRegistration,
} from "./onboarding.ts";
import * as EnvironmentRegistry from "./registry.ts";
import * as ConnectionProfileStore from "./profileStore.ts";
import { connectionRoutes, isLearned, routeHttpBaseUrl } from "./routes.ts";
import * as ConnectionWakeups from "./wakeups.ts";

const layerClientPresentation = Layer.succeed(
  ClientCapabilities.ClientPresentation,
  ClientCapabilities.ClientPresentation.of({
    metadata: {
      label: "Onicode Test",
      deviceType: "desktop",
      os: "Test OS",
    },
  }),
);

function layerPairingHttp(
  calls: Array<{ readonly url: string; readonly init: RequestInit }>,
  options?: {
    readonly failDescriptor?: boolean;
    readonly failExchange?: boolean;
    readonly protocolVersion?: number;
    readonly selfUpdate?: boolean;
    readonly grantScopes?: ReadonlyArray<AuthEnvironmentScope>;
    readonly exchangeGate?: { readonly reached: () => void; readonly release: Promise<void> };
  },
) {
  const grantScopes = options?.grantScopes ?? AuthStandardClientScopes;
  let sessionScopes: ReadonlyArray<string> = [];
  const fetchFn = ((input, init = {}) => {
    const url = String(input);
    calls.push({ url, init });

    if (url.endsWith("/.well-known/onicode/environment")) {
      if (options?.failDescriptor === true) {
        return Promise.resolve(
          Response.json({ message: "descriptor unavailable" }, { status: 503 }),
        );
      }
      return Promise.resolve(
        Response.json({
          environmentId: "environment-paired",
          label: "Paired environment",
          platform: {
            os: "linux",
            arch: "x64",
          },
          serverVersion: "0.0.0-test",
          orchestrationProtocolVersion: options?.protocolVersion ?? ORCHESTRATION_PROTOCOL_VERSION,
          capabilities: {
            repositoryIdentity: true,
            ...(options?.selfUpdate === true ? { serverSelfUpdate: "boot-service" } : {}),
          },
        }),
      );
    }

    if (url.endsWith("/oauth/token")) {
      if (options?.failExchange === true) {
        return Promise.resolve(
          Response.json(
            {
              _tag: "EnvironmentAuthInvalidError",
              code: "auth_invalid",
              reason: "invalid_credential",
              traceId: "trace-pairing-test",
            },
            { status: 401 },
          ),
        );
      }
      const body =
        init.body instanceof Uint8Array ? new TextDecoder().decode(init.body) : String(init.body);
      const requestedScope = new URLSearchParams(body).get("scope");
      sessionScopes = requestedScope === null ? grantScopes : requestedScope.split(" ");
      if (!sessionScopes.every((scope) => grantScopes.some((granted) => granted === scope))) {
        return Promise.resolve(
          Response.json(
            {
              _tag: "EnvironmentRequestInvalidError",
              code: "invalid_request",
              reason: "scope_not_granted",
              traceId: "pairing-scope-test",
            },
            { status: 400 },
          ),
        );
      }
      const granted = Response.json({
        access_token: "bearer-token",
        issued_token_type: "urn:ietf:params:oauth:token-type:access_token",
        token_type: "Bearer",
        expires_in: 3600,
        scope: sessionScopes.join(" "),
      });
      const gate = options?.exchangeGate;
      if (gate === undefined) return Promise.resolve(granted);
      gate.reached();
      return gate.release.then(() => granted);
    }

    if (url.endsWith("/api/auth/session")) {
      return Promise.resolve(
        Response.json({
          authenticated: true,
          auth: {
            policy: "remote-reachable",
            bootstrapMethods: ["one-time-token"],
          },
          scopes: sessionScopes,
        }),
      );
    }

    return Promise.reject(new Error(`Unexpected request: ${url}`));
  }) satisfies typeof fetch;

  return RpcHttp.layerRemoteHttpClient(fetchFn);
}

// The document the platforms persist, so dropped routes take their records with them.
function layerCatalogStores(catalog: Ref.Ref<ConnectionCatalogDocument>) {
  const update = (change: (document: ConnectionCatalogDocument) => ConnectionCatalogDocument) =>
    Ref.update(catalog, change);
  const read = Ref.get(catalog);
  return Layer.mergeAll(
    Layer.succeed(
      Persistence.ConnectionTargetStore,
      Persistence.ConnectionTargetStore.of({
        list: read.pipe(Effect.map((document) => document.targets)),
        listDisabled: read.pipe(Effect.map((document) => document.disabledEnvironmentIds)),
      }),
    ),
    Layer.succeed(
      Persistence.ConnectionRegistrationStore,
      Persistence.ConnectionRegistrationStore.of({
        register: (registration, routes) =>
          update((document) => registerConnectionInCatalog(document, registration, routes)),
        setRoutes: (environmentId, routes) =>
          update((document) => setRoutesInCatalog(document, environmentId, routes)),
        remove: (environmentId) =>
          update((document) => removeConnectionFromCatalog(document, environmentId)),
        setEnabled: (environmentId, enabled) =>
          update((document) => setConnectionEnabledInCatalog(document, environmentId, enabled)),
      }),
    ),
    Layer.succeed(
      ConnectionProfileStore.ConnectionProfileStore,
      ConnectionProfileStore.make({
        get: (connectionId) =>
          read.pipe(
            Effect.map((document) =>
              Option.fromUndefinedOr(
                document.profiles.find((profile) => profile.connectionId === connectionId),
              ),
            ),
          ),
        put: (profile) =>
          update((document) => ({
            ...document,
            profiles: replaceCatalogValue(
              document.profiles,
              (value) => value.connectionId,
              profile,
            ),
          })),
        remove: (connectionId) =>
          update((document) => ({
            ...document,
            profiles: removeCatalogValue(
              document.profiles,
              (value) => value.connectionId,
              connectionId,
            ),
          })),
      }),
    ),
    Layer.succeed(
      ConnectionCredentialStore.ConnectionCredentialStore,
      ConnectionCredentialStore.make({
        get: (connectionId) =>
          read.pipe(
            Effect.map((document) =>
              Option.fromUndefinedOr(
                document.credentials.find((stored) => stored.connectionId === connectionId)
                  ?.credential,
              ),
            ),
          ),
        put: (connectionId, credential) =>
          update((document) => ({
            ...document,
            credentials: replaceCatalogValue(document.credentials, (value) => value.connectionId, {
              connectionId,
              credential,
            }),
          })),
        remove: (connectionId) =>
          update((document) => ({
            ...document,
            credentials: removeCatalogValue(
              document.credentials,
              (value) => value.connectionId,
              connectionId,
            ),
          })),
      }),
    ),
  );
}

function layerPairingOnboarding(
  options?: Parameters<typeof layerPairingHttp>[1],
  calls: Array<{ readonly url: string; readonly init: RequestInit }> = [],
  provision?: ClientCapabilities.SshEnvironmentGateway["Service"]["provision"],
  catalog?: Ref.Ref<ConnectionCatalogDocument>,
) {
  const stores =
    catalog === undefined
      ? Layer.mergeAll(
          Layer.mock(Persistence.ConnectionTargetStore)({
            list: Effect.succeed([]),
            listDisabled: Effect.succeed([]),
          }),
          Layer.mock(Persistence.ConnectionRegistrationStore)({
            register: () => Effect.void,
            setEnabled: () => Effect.void,
          }),
          Layer.mock(ConnectionProfileStore.ConnectionProfileStore)({}),
          Layer.mock(ConnectionCredentialStore.ConnectionCredentialStore)({
            get: () => Effect.succeedNone,
            put: () => Effect.void,
          }),
        )
      : layerCatalogStores(catalog);
  const dependencies = Layer.mergeAll(
    layerClientPresentation,
    layerPairingHttp(calls, options),
    stores,
    Layer.mock(Persistence.EnvironmentCacheStore)({}),
    Layer.mock(ClientCapabilities.SshEnvironmentGateway)(provision ? { provision } : {}),
    Layer.succeed(
      Connectivity.Connectivity,
      Connectivity.Connectivity.of({ status: Effect.succeed("online"), changes: Stream.never }),
    ),
    Layer.mock(ConnectionDriver.ConnectionDriver)({ connect: () => Effect.never }),
    Layer.succeed(
      ConnectionWakeups.ConnectionWakeups,
      ConnectionWakeups.ConnectionWakeups.of({ changes: Stream.never }),
    ),
  );
  return Onboarding.layer.pipe(
    Layer.provideMerge(EnvironmentRegistry.layer.pipe(Layer.provideMerge(dependencies))),
  );
}

const PAIRED_ENVIRONMENT_ID = EnvironmentId.make("environment-paired");
const PAIRING_INPUT = { host: "remote.example.test", pairingCode: "pairing-token" };
const PAIRED_PROFILE = new BearerConnectionProfile({
  connectionId: "bearer:environment-paired:https://remote.example.test",
  environmentId: PAIRED_ENVIRONMENT_ID,
  label: "Paired environment",
  httpBaseUrl: "https://remote.example.test/",
  wsBaseUrl: "wss://remote.example.test/",
});
const PAIRED_TARGET = new BearerConnectionTarget({
  environmentId: PAIRED_ENVIRONMENT_ID,
  connectionId: PAIRED_PROFILE.connectionId,
  label: "Old label",
});
const PAIRED_CREDENTIAL = new BearerConnectionCredential({ token: "paired-token" });
const LEARNED_ORIGIN = "http://100.100.1.2:3773";
const LEARNED_PROFILE = new BearerConnectionProfile({
  connectionId: `learned:${PAIRED_ENVIRONMENT_ID}:${LEARNED_ORIGIN}@${PAIRED_PROFILE.connectionId}`,
  environmentId: PAIRED_ENVIRONMENT_ID,
  label: "Old label",
  httpBaseUrl: `${LEARNED_ORIGIN}/`,
  wsBaseUrl: "ws://100.100.1.2:3773/",
  learned: true,
  network: "tailscale",
});
const LEARNED_TARGET = new BearerConnectionTarget({
  environmentId: PAIRED_ENVIRONMENT_ID,
  connectionId: LEARNED_PROFILE.connectionId,
  label: "Old label",
});
const UNSUPPORTED_ERROR = new ConnectionBlockedError({
  reason: "unsupported",
  detail: "The saved server used an incompatible orchestration protocol.",
});

const registerSwitchedOffEnvironment = Effect.gen(function* () {
  const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
  const registration = new BearerConnectionRegistration({
    target: new BearerConnectionTarget({
      environmentId: PAIRED_ENVIRONMENT_ID,
      connectionId: PAIRED_PROFILE.connectionId,
      label: "Old label",
    }),
    profile: new BearerConnectionProfile({ ...PAIRED_PROFILE, label: "Old label" }),
    credential: new BearerConnectionCredential({ token: "old-token" }),
  });
  yield* registry.register(registration);
  yield* registry.setEnabled(PAIRED_ENVIRONMENT_ID, false);
  yield* registry.setCompatibility(PAIRED_ENVIRONMENT_ID, UNSUPPORTED_ERROR);
  const entries = yield* SubscriptionRef.get(registry.entries);
  expect(entries.get(PAIRED_ENVIRONMENT_ID)).toEqual({
    target: registration.target,
    profile: Option.some(registration.profile),
    enabled: false,
    unsupportedReason: UNSUPPORTED_ERROR.message,
  });
  return entries;
});

describe("connection onboarding", () => {
  it.effect("prepares a persisted bearer registration from pairing details", () =>
    Effect.gen(function* () {
      const calls: Array<{ readonly url: string; readonly init: RequestInit }> = [];
      const registration = yield* preparePairingRegistration({
        host: "remote.example.test",
        pairingCode: "pairing-token",
      }).pipe(Effect.provide(layerPairingOnboarding(undefined, calls)));

      expect(registration).toMatchObject({
        _tag: "BearerConnectionRegistration",
        target: {
          environmentId: "environment-paired",
          label: "Paired environment",
          connectionId: "bearer:environment-paired:https://remote.example.test",
        },
        profile: {
          environmentId: "environment-paired",
          label: "Paired environment",
          connectionId: "bearer:environment-paired:https://remote.example.test",
          httpBaseUrl: "https://remote.example.test/",
          wsBaseUrl: "wss://remote.example.test/",
        },
        credential: {
          token: "bearer-token",
        },
      });
      expect(calls.map((call) => call.url)).toEqual([
        "https://remote.example.test/.well-known/onicode/environment",
        "https://remote.example.test/oauth/token",
      ]);

      const tokenRequest = calls.find((call) => call.url.endsWith("/oauth/token"));
      const tokenBody =
        tokenRequest?.init.body instanceof Uint8Array
          ? new TextDecoder().decode(tokenRequest.init.body)
          : String(tokenRequest?.init.body);
      const tokenParams = new URLSearchParams(tokenBody);
      expect(tokenParams.get("subject_token")).toBe("pairing-token");
      expect(tokenParams.has("scope")).toBe(false);
      expect(tokenParams.get("client_label")).toBe("Onicode Test");
      expect(tokenParams.get("client_device_type")).toBe("desktop");
      expect(tokenParams.get("client_os")).toBe("Test OS");
    }),
  );

  it.effect("turns a switched-off environment back on, keeping its label, when paired again", () =>
    Effect.gen(function* () {
      const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
      const onboarding = yield* Onboarding.ConnectionOnboarding;
      yield* registerSwitchedOffEnvironment;

      const environmentId = yield* onboarding.registerPairing(PAIRING_INPUT);

      expect(environmentId).toBe(PAIRED_ENVIRONMENT_ID);
      const entries = yield* SubscriptionRef.get(registry.entries);
      const entry = entries.get(environmentId);
      expect(entries.size).toBe(1);
      expect(entry?.enabled).toBe(true);
      expect(entry?.unsupportedReason).toBeUndefined();
      expect(entry?.profile).toEqual(Option.some(PAIRED_PROFILE));
      expect(entry?.target).toEqual(
        new BearerConnectionTarget({
          environmentId,
          connectionId: PAIRED_PROFILE.connectionId,
          label: "Old label",
        }),
      );
    }).pipe(Effect.provide(layerPairingOnboarding())),
  );

  it.effect("refuses a pairing link that claims a saved environment from a new address", () => {
    const calls: Array<{ readonly url: string; readonly init: RequestInit }> = [];
    return Effect.gen(function* () {
      const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
      const onboarding = yield* Onboarding.ConnectionOnboarding;
      const before = yield* registerSwitchedOffEnvironment;

      const error = yield* onboarding
        .registerPairing({ host: "attacker.example.test", pairingCode: "pairing-token" })
        .pipe(Effect.flip);

      expect(error).toMatchObject({ _tag: "ConnectionBlockedError", reason: "configuration" });
      expect(yield* SubscriptionRef.get(registry.entries)).toEqual(before);
      expect(calls.map((call) => call.url)).toEqual([
        "https://attacker.example.test/.well-known/onicode/environment",
      ]);
    }).pipe(Effect.provide(layerPairingOnboarding(undefined, calls)));
  });

  it.effect(
    "refuses to add an SSH route to a saved environment during untargeted onboarding",
    () => {
      const target = {
        alias: "devbox",
        hostname: "devbox.example.test",
        username: "developer",
        port: 22,
      };
      return Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const onboarding = yield* Onboarding.ConnectionOnboarding;
        const before = yield* registerSwitchedOffEnvironment;

        const result = yield* onboarding.registerSsh({ target }).pipe(Effect.result);

        expect(result).toMatchObject({
          _tag: "Failure",
          failure: { _tag: "ConnectionBlockedError", reason: "configuration" },
        });
        expect(yield* SubscriptionRef.get(registry.entries)).toEqual(before);
      }).pipe(
        Effect.provide(
          layerPairingOnboarding(undefined, [], () =>
            Effect.succeed({
              environmentId: PAIRED_ENVIRONMENT_ID,
              label: "Remote development box",
              bootstrap: {
                target,
                httpBaseUrl: "http://127.0.0.1:3201",
                wsBaseUrl: "ws://127.0.0.1:3201",
                pairingToken: "pairing-token",
              },
              bearerToken: "bearer-token",
            }),
          ),
        ),
      );
    },
  );

  it.effect("refuses a new address when the environment is saved during the exchange", () => {
    let reached!: () => void;
    let release!: () => void;
    const exchangeReached = new Promise<void>((resolve) => (reached = resolve));
    const exchangeGate = {
      reached: () => reached(),
      release: new Promise<void>((resolve) => (release = resolve)),
    };
    return Effect.gen(function* () {
      const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
      const onboarding = yield* Onboarding.ConnectionOnboarding;
      const pairing = yield* onboarding
        .registerPairing({ host: "attacker.example.test", pairingCode: "pairing-token" })
        .pipe(Effect.result, Effect.forkChild);
      yield* Effect.promise(() => exchangeReached);
      const before = yield* registerSwitchedOffEnvironment;
      release();

      const outcome = yield* Fiber.join(pairing);

      expect(outcome).toMatchObject({
        _tag: "Failure",
        failure: { _tag: "ConnectionBlockedError", reason: "configuration" },
      });
      expect(yield* SubscriptionRef.get(registry.entries)).toEqual(before);
    }).pipe(Effect.provide(layerPairingOnboarding({ exchangeGate })));
  });

  it.effect("an edited label replaces the saved one", () => {
    const catalog = Ref.makeUnsafe(EMPTY_CONNECTION_CATALOG_DOCUMENT);
    return Effect.gen(function* () {
      const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
      const onboarding = yield* Onboarding.ConnectionOnboarding;
      yield* registerSwitchedOffEnvironment;

      yield* onboarding.updateBearer({
        environmentId: PAIRED_ENVIRONMENT_ID,
        label: "Studio",
        httpBaseUrl: PAIRED_PROFILE.httpBaseUrl,
      });

      const entry = (yield* SubscriptionRef.get(registry.entries)).get(PAIRED_ENVIRONMENT_ID);
      expect(entry === undefined ? [] : connectionRoutes(entry).map((r) => r.target.label)).toEqual(
        ["Studio"],
      );
    }).pipe(Effect.provide(layerPairingOnboarding(undefined, [], undefined, catalog)));
  });

  // A learned tailnet route ranks ahead of the paired one, so the edit form shows its address.
  it.effect.each([
    {
      edit: "a rename",
      httpBaseUrl: LEARNED_PROFILE.httpBaseUrl,
      pairedUrl: PAIRED_PROFILE.httpBaseUrl,
    },
    {
      edit: "an edit to the paired address",
      httpBaseUrl: PAIRED_PROFILE.httpBaseUrl,
      pairedUrl: PAIRED_PROFILE.httpBaseUrl,
    },
    {
      edit: "an edit to another host",
      httpBaseUrl: "https://studio.example.test",
      pairedUrl: "https://studio.example.test/",
    },
  ])("$edit through a learned route keeps the paired route and its one credential", (testCase) => {
    const catalog = Ref.makeUnsafe<ConnectionCatalogDocument>({
      ...EMPTY_CONNECTION_CATALOG_DOCUMENT,
      targets: [LEARNED_TARGET, PAIRED_TARGET],
      profiles: [
        LEARNED_PROFILE,
        new BearerConnectionProfile({ ...PAIRED_PROFILE, label: "Old label" }),
      ],
      credentials: [{ connectionId: PAIRED_PROFILE.connectionId, credential: PAIRED_CREDENTIAL }],
    });
    return Effect.gen(function* () {
      const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
      const onboarding = yield* Onboarding.ConnectionOnboarding;

      yield* onboarding.updateBearer({
        environmentId: PAIRED_ENVIRONMENT_ID,
        label: "Studio",
        httpBaseUrl: testCase.httpBaseUrl,
      });

      const entry = (yield* SubscriptionRef.get(registry.entries)).get(PAIRED_ENVIRONMENT_ID);
      const routes = entry === undefined ? [] : connectionRoutes(entry);
      expect(
        routes.map((route) => [
          route.target.label,
          routeHttpBaseUrl(route),
          isLearned(route),
          route.target._tag === "BearerConnectionTarget" ? route.target.connectionId : null,
        ]),
      ).toEqual([
        ["Studio", LEARNED_PROFILE.httpBaseUrl, true, LEARNED_PROFILE.connectionId],
        ["Studio", testCase.pairedUrl, false, PAIRED_PROFILE.connectionId],
      ]);
      expect(Option.getOrUndefined(routes[0]!.profile)).toMatchObject({ network: "tailscale" });
      const document = yield* Ref.get(catalog);
      expect(document.credentials).toEqual([
        { connectionId: PAIRED_PROFILE.connectionId, credential: PAIRED_CREDENTIAL },
      ]);
      expect(
        document.profiles.find((profile) => profile.connectionId === PAIRED_PROFILE.connectionId),
      ).toMatchObject({ httpBaseUrl: testCase.pairedUrl });
    }).pipe(Effect.provide(layerPairingOnboarding(undefined, [], undefined, catalog)));
  });

  it.effect("adds a new address as a route when pairing targets that environment", () =>
    Effect.gen(function* () {
      const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
      const onboarding = yield* Onboarding.ConnectionOnboarding;
      yield* registerSwitchedOffEnvironment;

      yield* onboarding.registerPairing({
        host: "lan.example.test",
        pairingCode: "pairing-token",
        expectedEnvironmentId: PAIRED_ENVIRONMENT_ID,
      });

      const entry = (yield* SubscriptionRef.get(registry.entries)).get(PAIRED_ENVIRONMENT_ID);
      expect(entry === undefined ? [] : connectionRoutes(entry).map(routeHttpBaseUrl)).toEqual(
        expect.arrayContaining(["https://remote.example.test/", "https://lan.example.test/"]),
      );
    }).pipe(Effect.provide(layerPairingOnboarding())),
  );

  it.effect(
    "re-pairs an outdated self-updatable environment so its connection can be rechecked",
    () =>
      Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const onboarding = yield* Onboarding.ConnectionOnboarding;
        yield* registerSwitchedOffEnvironment;
        yield* registry.setCompatibility(
          PAIRED_ENVIRONMENT_ID,
          new ConnectionBlockedError({
            reason: "unsupported",
            detail: "The saved server needs an update.",
            serverUpdateRequired: true,
          }),
        );
        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(PAIRED_ENVIRONMENT_ID),
        ).toMatchObject({
          enabled: false,
          serverUpdateRequired: true,
        });

        const environmentId = yield* onboarding.registerPairing(PAIRING_INPUT);

        expect(environmentId).toBe(PAIRED_ENVIRONMENT_ID);
        const entry = (yield* SubscriptionRef.get(registry.entries)).get(environmentId);
        expect(entry?.enabled).toBe(true);
        expect(entry?.unsupportedReason).toBeUndefined();
        expect(entry?.serverUpdateRequired).toBeUndefined();
        expect(entry?.profile).toEqual(Option.some(PAIRED_PROFILE));
      }).pipe(
        Effect.provide(
          layerPairingOnboarding({
            protocolVersion: ORCHESTRATION_PROTOCOL_VERSION - 1,
            selfUpdate: true,
          }),
        ),
      ),
  );

  it.effect.each([
    {
      kind: "primary",
      registration: new PrimaryConnectionRegistration({
        target: new PrimaryConnectionTarget({
          environmentId: PAIRED_ENVIRONMENT_ID,
          label: "Host environment",
          httpBaseUrl: "http://127.0.0.1:3773",
          wsBaseUrl: "ws://127.0.0.1:3773",
        }),
      }),
    },
    {
      kind: "bearer",
      registration: new BearerConnectionRegistration({
        target: new BearerConnectionTarget({
          environmentId: PAIRED_ENVIRONMENT_ID,
          connectionId: "platform:wsl",
          label: "WSL",
        }),
        profile: new BearerConnectionProfile({
          ...PAIRED_PROFILE,
          connectionId: "platform:wsl",
          label: "WSL",
          httpBaseUrl: "http://127.0.0.1:4000",
          wsBaseUrl: "ws://127.0.0.1:4000",
        }),
        credential: new BearerConnectionCredential({ token: "platform-token" }),
      }),
    },
  ] as const)(
    "leaves a platform-managed $kind environment untouched when its id is paired",
    ({ registration }) =>
      Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const onboarding = yield* Onboarding.ConnectionOnboarding;
        yield* registry.reconcilePlatform([registration]);
        yield* registry.setCompatibility(PAIRED_ENVIRONMENT_ID, UNSUPPORTED_ERROR);
        const before = yield* SubscriptionRef.get(registry.entries);
        expect(before.get(PAIRED_ENVIRONMENT_ID)).toMatchObject({
          enabled: true,
          unsupportedReason: UNSUPPORTED_ERROR.message,
        });

        const environmentId = yield* onboarding.registerPairing(PAIRING_INPUT);

        expect(environmentId).toBe(PAIRED_ENVIRONMENT_ID);
        expect(yield* SubscriptionRef.get(registry.entries)).toEqual(before);
      }).pipe(Effect.provide(layerPairingOnboarding())),
  );

  it.effect("leaves a switched-off environment unchanged when the pairing exchange fails", () =>
    Effect.gen(function* () {
      const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
      const onboarding = yield* Onboarding.ConnectionOnboarding;
      const before = yield* registerSwitchedOffEnvironment;

      const error = yield* onboarding.registerPairing(PAIRING_INPUT).pipe(Effect.flip);

      expect(error).toMatchObject({
        _tag: "ConnectionBlockedError",
        reason: "authentication",
        traceId: "trace-pairing-test",
      });
      const after = yield* SubscriptionRef.get(registry.entries);
      expect(after).toEqual(before);
      expect(after.get(PAIRED_ENVIRONMENT_ID)?.enabled).toBe(false);
      expect(after.get(PAIRED_ENVIRONMENT_ID)?.unsupportedReason).toBe(UNSUPPORTED_ERROR.message);
    }).pipe(Effect.provide(layerPairingOnboarding({ failExchange: true }))),
  );

  it.effect("rejects an incompatible server without consuming the pairing credential", () =>
    Effect.gen(function* () {
      const calls: Array<{ readonly url: string; readonly init: RequestInit }> = [];
      const error = yield* preparePairingRegistration({
        host: "remote.example.test",
        pairingCode: "pairing-token",
      }).pipe(
        Effect.provide(
          layerPairingOnboarding({ protocolVersion: ORCHESTRATION_PROTOCOL_VERSION + 1 }, calls),
        ),
        Effect.flip,
      );
      expect(error).toMatchObject({ reason: "unsupported" });
      expect(calls.map((call) => call.url)).toEqual([
        "https://remote.example.test/.well-known/onicode/environment",
      ]);
    }),
  );
  it.effect.each([
    { label: "read-only", scopes: ["orchestration:read"] },
    { label: "default", scopes: AuthStandardClientScopes },
  ] as const)("preserves the $label grant when pairing a remote environment", ({ scopes }) =>
    Effect.gen(function* () {
      const registration = yield* preparePairingRegistration({
        host: "remote.example.test",
        pairingCode: "pairing-token",
      });

      const session = yield* fetchRemoteSessionState({
        httpBaseUrl: registration.profile.httpBaseUrl,
        bearerToken: registration.credential.token,
      });

      expect(session.authenticated).toBe(true);
      expect(session.scopes).toEqual(scopes);
    }).pipe(Effect.provide(layerPairingOnboarding({ grantScopes: scopes }))),
  );

  it.effect("refuses to add a route that reaches a different machine, keeping the code", () =>
    Effect.gen(function* () {
      const calls: Array<{ readonly url: string; readonly init: RequestInit }> = [];
      const error = yield* preparePairingRegistration({
        host: "remote.example.test",
        pairingCode: "pairing-token",
        expectedEnvironmentId: EnvironmentId.make("some-other-machine"),
      }).pipe(Effect.provide(layerPairingOnboarding(undefined, calls)), Effect.flip);
      expect(error).toMatchObject({ reason: "configuration" });
      expect(error.message).toContain("different machine");
      expect(calls.map((call) => call.url)).toEqual([
        "https://remote.example.test/.well-known/onicode/environment",
      ]);
    }),
  );

  it.effect("refuses an outdated server that cannot update itself", () =>
    Effect.gen(function* () {
      const calls: Array<{ readonly url: string; readonly init: RequestInit }> = [];
      const error = yield* preparePairingRegistration({
        host: "remote.example.test",
        pairingCode: "pairing-token",
      }).pipe(
        Effect.provide(
          layerPairingOnboarding({ protocolVersion: ORCHESTRATION_PROTOCOL_VERSION - 1 }, calls),
        ),
        Effect.flip,
      );
      expect(error).toMatchObject({ reason: "unsupported" });
      expect(error).not.toHaveProperty("serverUpdateRequired");
      expect(calls.map((call) => call.url)).toEqual([
        "https://remote.example.test/.well-known/onicode/environment",
      ]);
    }),
  );

  it.effect("does not consume a pairing credential when descriptor discovery fails", () =>
    Effect.gen(function* () {
      const calls: Array<{ readonly url: string; readonly init: RequestInit }> = [];

      yield* preparePairingRegistration({
        host: "remote.example.test",
        pairingCode: "pairing-token",
      }).pipe(Effect.provide(layerPairingOnboarding({ failDescriptor: true }, calls)), Effect.flip);

      expect(calls.map((call) => call.url)).toEqual([
        "https://remote.example.test/.well-known/onicode/environment",
      ]);
    }),
  );

  it.effect("updates bearer metadata while preserving the credential and identity", () =>
    Effect.gen(function* () {
      const environmentId = EnvironmentId.make("environment-paired");
      const registration = yield* prepareBearerConnectionUpdate({
        input: {
          environmentId,
          label: "  Renamed environment  ",
          httpBaseUrl: "http://100.65.180.100:3773/path",
        },
        entry: Option.some({
          target: new BearerConnectionTarget({
            environmentId,
            label: "Old label",
            connectionId: "bearer:environment-paired",
          }),
          profile: Option.some(
            new BearerConnectionProfile({
              connectionId: "bearer:environment-paired",
              environmentId,
              label: "Old label",
              httpBaseUrl: "http://old.example.test/",
              wsBaseUrl: "ws://old.example.test/",
            }),
          ),
          enabled: true,
        }),
        credential: Option.some(new BearerConnectionCredential({ token: "bearer-token" })),
      });

      expect(registration).toMatchObject({
        target: {
          environmentId,
          label: "Renamed environment",
          connectionId: "bearer:environment-paired",
        },
        profile: {
          environmentId,
          label: "Renamed environment",
          httpBaseUrl: "http://100.65.180.100:3773/",
          wsBaseUrl: "ws://100.65.180.100:3773/",
        },
        credential: { token: "bearer-token" },
      });
    }),
  );

  it.effect("prepares an SSH registration from the provisioned platform environment", () =>
    Effect.gen(function* () {
      const target = {
        alias: "devbox",
        hostname: "devbox.example.test",
        username: "developer",
        port: 22,
      };
      const registration = yield* prepareSshRegistration({
        target,
      }).pipe(
        Effect.provideService(
          ClientCapabilities.SshEnvironmentGateway,
          ClientCapabilities.SshEnvironmentGateway.of({
            provision: () =>
              Effect.succeed({
                environmentId: EnvironmentId.make("environment-ssh"),
                label: "Remote development box",
                bootstrap: {
                  target,
                  httpBaseUrl: "http://127.0.0.1:3201",
                  wsBaseUrl: "ws://127.0.0.1:3201",
                  pairingToken: "pairing-token",
                },
                bearerToken: "bearer-token",
              }),
            prepare: () => Effect.die("unused"),
            disconnect: () => Effect.die("unused"),
          }),
        ),
      );

      expect(registration).toMatchObject({
        _tag: "SshConnectionRegistration",
        target: {
          environmentId: "environment-ssh",
          label: "Remote development box",
          connectionId: 'ssh:environment-ssh:["devbox","devbox.example.test","developer",22]',
        },
        profile: {
          environmentId: "environment-ssh",
          label: "Remote development box",
          connectionId: 'ssh:environment-ssh:["devbox","devbox.example.test","developer",22]',
          target,
        },
        // Saved so the first connect reuses this pairing instead of adding another.
        credential: { token: "bearer-token" },
      });
    }),
  );
});
