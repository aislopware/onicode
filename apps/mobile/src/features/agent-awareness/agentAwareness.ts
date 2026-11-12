import type { PreparedConnection } from "@onicode/client-runtime/connection";
import { enabledEnvironmentIds } from "@onicode/client-runtime/state/connections";
import type { EnvironmentId, PushTestResult } from "@onicode/contracts";
import * as Clock from "effect/Clock";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import { AsyncResult, Atom } from "effect/reactivity";
import * as Application from "expo-application";
import Constants from "expo-constants";
import * as Device from "expo-device";
import * as Notifications from "expo-notifications";
import type { LiveActivity } from "expo-widgets";
import { Platform } from "react-native";

import { subscribeAppActive } from "../../lib/appState";
import { coalescedRun } from "../../lib/coalescedRun";
import { runtime } from "../../lib/runtime";
import { environmentCatalog } from "../../connection/catalog";
import type { AgentNotificationPreferences } from "../../persistence/mobile-preferences";
import * as MobileStorage from "../../persistence/mobile-storage";
import { appAtomRegistry } from "../../state/atom-registry";
import { mobilePreferencesAtom } from "../../state/preferences";
import { environmentSession } from "../../state/session";
import type { AgentActivityProps } from "../../widgets/AgentActivity";
import { getAgentLiveActivities, observeAgentLiveActivityStartToken } from "./agentLiveActivity";
import {
  clearAndroidAgentNotifications,
  clearAndroidEnvironmentNotifications,
  configureAndroidAgentNotifications,
  supportsAndroidAgentNotifications,
} from "./androidNotifications";
import { nativeDeviceKeys } from "./deviceAttestation";
import { makeEnvironmentPushClient } from "./environmentPush";
import { makePushGatewayClient, pushGatewayUrl } from "./pushGateway";
import {
  grantIsFresh,
  makePushRegistrar,
  planPushSync,
  type EnvironmentPushStatus,
  type PushRegistrar,
} from "./pushRegistration";
import { loadPushRegistration, savePushRegistration } from "./registrationStore";

export const DEFAULT_AGENT_NOTIFICATION_PREFERENCES: AgentNotificationPreferences = {
  enabled: false,
  notifyOnApproval: true,
  notifyOnInput: true,
  notifyOnCompletion: true,
  notifyOnFailure: true,
  activity: true,
};

export type AgentAwarenessEnvironmentStatus = EnvironmentPushStatus | { readonly _tag: "pending" };

export interface AgentAwarenessStatus {
  /** Null when this build can register; otherwise why it cannot. */
  readonly unavailable: string | null;
  /** A device-wide failure, such as a push token or attestation the platform refused. */
  readonly error: string | null;
  readonly environments: ReadonlyMap<EnvironmentId, AgentAwarenessEnvironmentStatus>;
}

class PushTokenError extends Schema.TaggedError<PushTokenError>()("PushTokenError", {
  cause: Schema.Defect(),
}) {
  override get message(): string {
    return "Could not get a push token.";
  }
}

const PREPARED_WAIT_MS = 20_000;
const LIVE_ACTIVITY_RECONCILE_MS = 15_000;
/** How long an activity the environment cannot update yet may stay up while registering retries. */
const UNREGISTERED_ACTIVITY_GRACE_MS = 5 * 60_000;

function unavailableReason(): string | null {
  if (Platform.OS !== "ios" && Platform.OS !== "android") return "Not available on this device.";
  if (Constants.expoConfig?.extra?.iosPersonalTeamBuild === true) {
    return "This build is signed without push notifications.";
  }
  if (pushGatewayUrl() === null) return "This build names no push gateway.";
  if (nativeDeviceKeys() === null) return "This build has no push support.";
  if (Platform.OS === "android" && !supportsAndroidAgentNotifications()) {
    return "This build has no push support.";
  }
  if (!Application.applicationId) return "This build has no application id.";
  return null;
}

export const agentAwarenessStatusAtom = Atom.make<AgentAwarenessStatus>({
  unavailable: unavailableReason(),
  error: null,
  environments: new Map(),
}).pipe(Atom.keepAlive, Atom.withLabel("mobile:agent-awareness:status"));

function readAgentNotificationPreferences(): AgentNotificationPreferences {
  const preferences = appAtomRegistry.get(mobilePreferencesAtom);
  return AsyncResult.isSuccess(preferences)
    ? (preferences.value.agentNotifications ?? DEFAULT_AGENT_NOTIFICATION_PREFERENCES)
    : DEFAULT_AGENT_NOTIFICATION_PREFERENCES;
}

function updateStatus(update: (status: AgentAwarenessStatus) => AgentAwarenessStatus): void {
  appAtomRegistry.set(
    agentAwarenessStatusAtom,
    update(appAtomRegistry.get(agentAwarenessStatusAtom)),
  );
}

function setEnvironmentStatus(
  environmentId: EnvironmentId,
  status: AgentAwarenessEnvironmentStatus | null,
): void {
  updateStatus((current) => {
    const environments = new Map(current.environments);
    if (status === null) environments.delete(environmentId);
    else environments.set(environmentId, status);
    return { ...current, environments };
  });
}

const decodeActivityEnvironment = Schema.decodeUnknownOption(
  Schema.fromJsonString(Schema.Struct({ environmentId: Schema.String })),
);

interface ObservedActivity {
  readonly environmentId: EnvironmentId;
  grant:
    | { readonly token: string; readonly issuedAt: number; readonly expiresAt: number }
    | undefined;
  readonly observedAt: number;
  readonly unsubscribe: () => void;
}

interface Context {
  readonly registrar: PushRegistrar;
  readonly deviceId: string;
}

let context: Promise<Context | null> | null = null;
const lastPrepared = new Map<EnvironmentId, PreparedConnection>();
const preparedWaiters = new Map<EnvironmentId, Set<(prepared: PreparedConnection) => void>>();
const observedActivities = new Map<string, ObservedActivity>();
let liveActivityStartToken: string | null = null;
let notificationTokenSeen: string | null = null;

const loadContext = Effect.fn("AgentAwareness.loadContext")(function* () {
  const keys = nativeDeviceKeys();
  const appId = Application.applicationId;
  const gatewayUrl = pushGatewayUrl();
  if (
    keys === null ||
    !appId ||
    gatewayUrl === null ||
    (Platform.OS !== "ios" && Platform.OS !== "android")
  ) {
    return null;
  }
  // The keychain keeps this id through a reinstall and off a backup restored to another phone.
  const deviceId = yield* (yield* MobileStorage.MobileStorage).loadOrCreateAgentAwarenessDeviceId;
  const gateway = yield* makePushGatewayClient(gatewayUrl);
  const registrar = yield* makePushRegistrar({
    config: {
      platform: Platform.OS,
      appId,
      gatewayUrl,
      deviceId,
      label: (Device.deviceName ?? Device.modelName ?? "Phone").slice(0, 120) || "Phone",
      allowUnattested: Constants.expoConfig?.extra?.appVariant === "development",
    },
    gateway,
    keys,
    load: loadPushRegistration,
    save: savePushRegistration,
  });
  return { registrar, deviceId };
});

function getContext(): Promise<Context | null> {
  context ??= runtime.runPromise(loadContext()).catch((error: unknown) => {
    context = null;
    updateStatus((current) => ({ ...current, error: String(error) }));
    return null;
  });
  return context;
}

function waitForPrepared(environmentId: EnvironmentId): Promise<PreparedConnection | null> {
  const known = lastPrepared.get(environmentId);
  if (known) return Promise.resolve(known);
  return new Promise((resolve) => {
    const waiters = preparedWaiters.get(environmentId) ?? new Set();
    preparedWaiters.set(environmentId, waiters);
    const done = (prepared: PreparedConnection | null) => {
      clearTimeout(timer);
      waiters.delete(done);
      resolve(prepared);
    };
    const timer = setTimeout(() => done(null), PREPARED_WAIT_MS);
    waiters.add(done);
  });
}

const notificationToken = Effect.fn("AgentAwareness.notificationToken")(function* () {
  const permissions = yield* Effect.promise(() => Notifications.getPermissionsAsync());
  if (!permissions.granted) return null;
  const token = yield* Effect.tryPromise({
    try: () => Notifications.getDevicePushTokenAsync(),
    catch: (cause) => new PushTokenError({ cause }),
  });
  notificationTokenSeen = typeof token.data === "string" ? token.data : null;
  return notificationTokenSeen !== null && notificationTokenSeen.length > 0
    ? notificationTokenSeen
    : null;
});

function environmentClient(prepared: PreparedConnection) {
  return makeEnvironmentPushClient(prepared);
}

function connectedEnvironments() {
  const catalog = appAtomRegistry.get(environmentCatalog.catalogValueAtom);
  return {
    ready: catalog.isReady,
    saved: new Set(catalog.entries.keys()),
    connected: Array.from(enabledEnvironmentIds(catalog)),
  };
}

const syncAll = Effect.fn("AgentAwareness.syncAll")(function* (input: Context) {
  const preferences = readAgentNotificationPreferences();
  const environments = connectedEnvironments();
  if (!environments.ready) return;
  const plan = planPushSync({
    enabled: preferences.enabled,
    saved: environments.saved,
    connected: environments.connected,
    known: yield* input.registrar.knownEnvironmentIds,
  });

  for (const environmentId of plan.unregister) {
    const prepared = lastPrepared.get(environmentId);
    yield* input.registrar.unregisterEnvironment({
      environmentId,
      server: prepared ? yield* environmentClient(prepared) : null,
    });
    clearAndroidEnvironmentNotifications(environmentId);
    setEnvironmentStatus(environmentId, null);
  }
  if (Platform.OS === "android") {
    if (preferences.enabled)
      configureAndroidAgentNotifications(input.deviceId, preferences.activity);
    else clearAndroidAgentNotifications();
  }
  if (plan.sync.length === 0) {
    updateStatus((current) => ({ ...current, error: null, environments: new Map() }));
    return;
  }

  const token = yield* notificationToken().pipe(
    Effect.tapError((error) =>
      Effect.sync(() => updateStatus((current) => ({ ...current, error: error.message }))),
    ),
    Effect.orElseSucceed(() => null),
  );
  const { enabled: _enabled, ...devicePreferences } = preferences;
  for (const environmentId of plan.sync) {
    const prepared = lastPrepared.get(environmentId);
    if (!prepared) {
      setEnvironmentStatus(environmentId, { _tag: "unreachable", message: "Not connected." });
      continue;
    }
    if (!appAtomRegistry.get(agentAwarenessStatusAtom).environments.has(environmentId)) {
      setEnvironmentStatus(environmentId, { _tag: "pending" });
    }
    const status = yield* input.registrar.syncEnvironment({
      environmentId,
      server: yield* environmentClient(prepared),
      tokens: { notification: token, liveActivityStart: liveActivityStartToken },
      preferences: devicePreferences,
    });
    setEnvironmentStatus(environmentId, status);
  }
  if (token !== null) updateStatus((current) => ({ ...current, error: null }));
});

const runSync = coalescedRun(async () => {
  if (appAtomRegistry.get(agentAwarenessStatusAtom).unavailable !== null) return;
  const loaded = await getContext();
  if (loaded === null) return;
  await runtime.runPromise(
    syncAll(loaded).pipe(
      Effect.catch((error) =>
        Effect.sync(() => updateStatus((current) => ({ ...current, error: error.message }))),
      ),
    ),
  );
  await reconcile();
});

function requestSync(): void {
  void runSync();
}

async function endActivity(activity: LiveActivity<AgentActivityProps>): Promise<void> {
  observedActivities.get(activity.getId())?.unsubscribe();
  observedActivities.delete(activity.getId());
  await activity.end("immediate").catch(() => undefined);
}

const registerActivityToken = Effect.fn("AgentAwareness.registerActivityToken")(function* (
  input: Context,
  environmentId: EnvironmentId,
  token: string,
) {
  const prepared = yield* Effect.promise(() => waitForPrepared(environmentId));
  if (prepared === null) return "retry" as const;
  const issuedAt = yield* Clock.currentTimeMillis;
  const expiresAt = yield* input.registrar.registerLiveActivity({
    environmentId,
    server: yield* environmentClient(prepared),
    token,
  });
  return expiresAt === null ? ("refused" as const) : { token, issuedAt, expiresAt };
});

/** An activity the environment can never update would go stale on the Lock Screen. */
async function keepActivityRegistered(
  input: Context,
  activity: LiveActivity<AgentActivityProps>,
  observed: ObservedActivity,
  token: string | null,
): Promise<void> {
  if (token === null) return;
  if (grantIsFresh(observed.grant, token, Date.now())) return;
  const outcome = await runtime
    .runPromise(registerActivityToken(input, observed.environmentId, token))
    .catch(() => "retry" as const);
  if (outcome === "retry") {
    // Ending it now would cost the whole stretch of work: the environment starts one per stretch.
    const waited = Date.now() - observed.observedAt;
    if (observed.grant === undefined && waited > UNREGISTERED_ACTIVITY_GRACE_MS) {
      await endActivity(activity);
    }
    return;
  }
  if (outcome === "refused") {
    await endActivity(activity);
    return;
  }
  observed.grant = outcome;
}

const reconcile = coalescedRun(async () => {
  const loaded = await getContext();
  if (loaded !== null) await reconcileLiveActivities(loaded);
});

async function reconcileLiveActivities(input: Context): Promise<void> {
  if (Platform.OS !== "ios") return;
  const preferences = readAgentNotificationPreferences();
  const { ready, saved } = connectedEnvironments();
  if (!ready) return;
  const activities = getAgentLiveActivities();
  const live = new Set(activities.map((activity) => activity.getId()));
  for (const [id, observed] of observedActivities) {
    if (!live.has(id)) {
      observed.unsubscribe();
      observedActivities.delete(id);
    }
  }
  await Promise.all(
    activities.map(async (activity) => {
      let observed = observedActivities.get(activity.getId());
      if (!observed) {
        const props = await activity.getProps().catch(() => null);
        const decoded = props === null ? Option.none() : decodeActivityEnvironment(props);
        if (Option.isNone(decoded)) {
          await endActivity(activity);
          return;
        }
        const subscription = activity.addPushTokenListener((event) => {
          const current = observedActivities.get(activity.getId());
          if (current && event.pushToken) {
            void keepActivityRegistered(input, activity, current, event.pushToken);
          }
        });
        observed = {
          environmentId: decoded.value.environmentId as EnvironmentId,
          grant: undefined,
          observedAt: Date.now(),
          unsubscribe: () => subscription.remove(),
        };
        observedActivities.set(activity.getId(), observed);
      }
      if (!preferences.enabled || !preferences.activity || !saved.has(observed.environmentId)) {
        await endActivity(activity);
        return;
      }
      const token = await activity.getPushToken().catch(() => null);
      await keepActivityRegistered(input, activity, observed, token);
    }),
  );
}

function observePreparedConnections(): () => void {
  const subscriptions = new Map<EnvironmentId, () => void>();
  let synced = "";
  const watch = () => {
    const { ready, saved, connected } = connectedEnvironments();
    for (const [environmentId, unsubscribe] of subscriptions) {
      if (!saved.has(environmentId)) {
        unsubscribe();
        subscriptions.delete(environmentId);
      }
    }
    for (const environmentId of saved) {
      if (subscriptions.has(environmentId)) continue;
      subscriptions.set(
        environmentId,
        appAtomRegistry.subscribe(
          environmentSession.preparedConnectionValueAtom(environmentId),
          (prepared) => {
            if (Option.isNone(prepared)) return;
            const isNew = !lastPrepared.has(environmentId);
            lastPrepared.set(environmentId, prepared.value);
            for (const waiter of preparedWaiters.get(environmentId) ?? []) waiter(prepared.value);
            if (isNew) requestSync();
          },
          { immediate: true },
        ),
      );
    }
    // The catalog also changes with every connection's state; only its membership matters here.
    const membership = JSON.stringify([ready, [...saved].sort(), [...connected].sort()]);
    if (membership === synced) return;
    synced = membership;
    requestSync();
  };
  const unsubscribeCatalog = appAtomRegistry.subscribe(environmentCatalog.catalogValueAtom, watch, {
    immediate: true,
  });
  return () => {
    unsubscribeCatalog();
    for (const unsubscribe of subscriptions.values()) unsubscribe();
    subscriptions.clear();
  };
}

/**
 * iOS wakes a suspended app in the background when a push starts a Live Activity, with nothing
 * for JS to listen to. The timer runs in any app state so that wake-up registers the activity.
 */
function reconcileLiveActivitiesPeriodically(): () => void {
  if (Platform.OS !== "ios") return () => undefined;
  const timer = setInterval(() => void reconcile(), LIVE_ACTIVITY_RECONCILE_MS);
  return () => clearInterval(timer);
}

function observeNotificationPreferences(): () => void {
  let synced = JSON.stringify(readAgentNotificationPreferences());
  return appAtomRegistry.subscribe(mobilePreferencesAtom, () => {
    const next = JSON.stringify(readAgentNotificationPreferences());
    if (next === synced) return;
    synced = next;
    requestSync();
  });
}

/** App-lifetime push registration with every saved environment. */
export function installAgentAwareness(): () => void {
  if (appAtomRegistry.get(agentAwarenessStatusAtom).unavailable !== null) return () => undefined;
  const disposers = [
    observePreparedConnections(),
    observeNotificationPreferences(),
    subscribeAppActive(requestSync),
    observeAgentLiveActivityStartToken((token) => {
      if (token === liveActivityStartToken) return;
      liveActivityStartToken = token;
      requestSync();
    }),
    (() => {
      // Reading the token during a sync fires this listener too, so only a new token counts.
      const subscription = Notifications.addPushTokenListener((token) => {
        if (token.data === notificationTokenSeen) return;
        notificationTokenSeen = typeof token.data === "string" ? token.data : null;
        requestSync();
      });
      return () => subscription.remove();
    })(),
    reconcileLiveActivitiesPeriodically(),
  ];
  return () => {
    for (const dispose of disposers) dispose();
  };
}

export async function sendAgentNotificationTest(
  environmentId: EnvironmentId,
): Promise<PushTestResult> {
  const loaded = await getContext();
  const prepared = lastPrepared.get(environmentId);
  if (loaded === null || !prepared) {
    return { outcome: "failed", detail: "Not connected to this environment." };
  }
  return runtime.runPromise(
    Effect.flatMap(environmentClient(prepared), (client) => client.sendTest(loaded.deviceId)).pipe(
      Effect.catch((error) =>
        Effect.succeed<PushTestResult>({ outcome: "failed", detail: error.message }),
      ),
    ),
  );
}
