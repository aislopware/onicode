// @effect-diagnostics-next-line nodeBuiltinImport:off -- An alert's id is one synchronous hash.
import * as NodeCrypto from "node:crypto";

import {
  type AuthSessionId,
  type OrchestrationV2DomainEvent,
  type PushDevice,
  type PushDeviceId,
  type PushGrant,
  type PushRegisterDeviceRequest,
  type PushRegistrationInfo,
  type PushTestResult,
  type ThreadId,
} from "@onicode/contracts";
import {
  type PushActivityState,
  type PushMessage,
  type PushNotification,
} from "@onicode/contracts/pushGateway";
import { type AgentAwarenessState, projectThreadAwarenessV2 } from "@onicode/shared/agentAwareness";
import { makeDrainableWorker } from "@onicode/shared/DrainableWorker";
import { turnItemUpdateCanEndBackgroundWork } from "@onicode/shared/orchestrationV2PendingBackgroundWork";
import {
  PUSH_ACTIVITY_KEEPALIVE_MS,
  PUSH_ACTIVITY_KEEPALIVE_RETRY_MS,
  PUSH_ANDROID_ACTIVITY_KEEPALIVE_MS,
  pushActivityIdentity,
  pushActivityCounts,
  pushActivityState,
  pushAlertAllowed,
  type PushAlertKind,
  pushAlertKind,
  pushNotificationForState,
} from "@onicode/shared/pushActivity";
import * as Clock from "effect/Clock";
import * as Config from "effect/Config";
import * as Context from "effect/Context";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Result from "effect/Result";
import * as Schedule from "effect/Schedule";
import * as Schema from "effect/Schema";
import type * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";

import { builtInPushGatewayUrls } from "../config.ts";
import * as ServerEnvironment from "../environment/ServerEnvironment.ts";
import type { AgentAwarenessThread } from "../orchestration-v2/ProjectionStore.ts";
import * as ThreadManagement from "../orchestration-v2/ThreadManagementService.ts";
import * as ProjectService from "../project/ProjectService.ts";
import { forkParked } from "../serverActivation.ts";
import * as PushDevices from "./PushDevices.ts";
import * as PushGatewayClient from "./PushGatewayClient.ts";
import * as PushSigningKey from "./PushSigningKey.ts";

// A run can end moments before its continuation starts; wait before calling it finished.
const CONFIRM_DELAY_MS = 5_000;
// iOS ends a Live Activity eight hours after it starts, whatever its work is doing. A start may
// take the gateway's five minutes to reach the phone, and a replacement that arrived before the
// old card ended would make two live ones.
const LIVE_ACTIVITY_LIFETIME_MS = 8 * 60 * 60_000 + 5 * 60_000;

export class PushGatewayNotAllowedError extends Schema.TaggedError<PushGatewayNotAllowedError>()(
  "PushGatewayNotAllowedError",
  { gatewayUrl: Schema.String },
) {
  override get message(): string {
    return `This environment does not deliver through the push gateway ${this.gatewayUrl}.`;
  }
}

export class PushDeviceNotFoundError extends Schema.TaggedError<PushDeviceNotFoundError>()(
  "PushDeviceNotFoundError",
  { deviceId: Schema.String },
) {
  override get message(): string {
    return `No push device ${this.deviceId} is registered with this environment.`;
  }
}

export class PushDeviceNotOwnedError extends Schema.TaggedError<PushDeviceNotOwnedError>()(
  "PushDeviceNotOwnedError",
  { deviceId: Schema.String },
) {
  override get message(): string {
    return `Push device ${this.deviceId} was registered by another session.`;
  }
}

/** Who is asking, and what they may do with devices other sessions registered. */
export interface PushDeviceCaller {
  readonly sessionId: AuthSessionId;
  readonly seesAllDevices: boolean;
  readonly managesAllDevices: boolean;
}

export class PushNotifications extends Context.Service<
  PushNotifications,
  {
    readonly registrationInfo: PushRegistrationInfo;
    readonly listDevices: (
      caller: PushDeviceCaller,
    ) => Effect.Effect<ReadonlyArray<PushDevice>, PushDevices.PushDevicesError>;
    readonly registerDevice: (input: {
      readonly caller: PushDeviceCaller;
      readonly deviceId: PushDeviceId;
      readonly request: PushRegisterDeviceRequest;
    }) => Effect.Effect<
      void,
      PushGatewayNotAllowedError | PushDeviceNotOwnedError | PushDevices.PushDevicesError
    >;
    readonly registerLiveActivity: (input: {
      readonly caller: PushDeviceCaller;
      readonly deviceId: PushDeviceId;
      readonly grant: PushGrant;
    }) => Effect.Effect<
      void,
      PushDeviceNotFoundError | PushDeviceNotOwnedError | PushDevices.PushDevicesError
    >;
    readonly unregisterDevice: (input: {
      readonly deviceId: PushDeviceId;
      readonly caller: PushDeviceCaller;
    }) => Effect.Effect<void, PushDeviceNotOwnedError | PushDevices.PushDevicesError>;
    readonly sendTest: (input: {
      readonly deviceId: PushDeviceId;
      readonly caller: PushDeviceCaller;
    }) => Effect.Effect<
      PushTestResult,
      PushDeviceNotFoundError | PushDeviceNotOwnedError | PushDevices.PushDevicesError
    >;
    readonly start: () => Effect.Effect<void, never, Scope.Scope>;
    /** Resolves once every queued change has been delivered or given up on. */
    readonly drain: Effect.Effect<void>;
  }
>()("onicode/push/PushNotifications") {}

const TRACKED_EVENTS: ReadonlySet<string> = new Set([
  "thread.created",
  "thread.archived",
  "thread.unarchived",
  "thread.deleted",
  "thread.metadata-updated",
  "thread.settled",
  "thread.visited",
  "thread.marked-unread",
  "thread.model-selection-updated",
  "thread.provider-switched",
  "run.created",
  "run.updated",
  "runtime-request.updated",
  "subagent.updated",
  "provider-thread.updated",
] satisfies ReadonlyArray<OrchestrationV2DomainEvent["type"]>);

/** Whether an event can change what a phone shows for its thread. */
function changesPushActivity(event: OrchestrationV2DomainEvent): boolean {
  if (event.type === "turn-item.updated") return turnItemUpdateCanEndBackgroundWork(event.payload);
  if (event.type === "thread.created" && event.payload.historyOrigin === "v1_import") return false;
  return TRACKED_EVENTS.has(event.type);
}

/** The origin a gateway is addressed by, or null for a URL no delivery may go to. */
export function pushGatewayOrigin(value: string): string | null {
  if (!URL.canParse(value)) return null;
  const url = new URL(value);
  const loopback = url.hostname === "localhost" || url.hostname === "127.0.0.1";
  if (url.protocol !== "https:" && !(url.protocol === "http:" && loopback)) return null;
  return url.origin;
}

const isLive = (state: AgentAwarenessState | undefined) =>
  state !== undefined && state.phase !== "completed" && state.phase !== "failed";

/** What a phone shows for one thread. */
const shownAs = (state: AgentAwarenessState | undefined | null) =>
  state == null || state.seen
    ? null
    : [state.phase, state.threadTitle, state.projectTitle, state.modelTitle].join("\n");

type Work =
  | { readonly _tag: "thread"; readonly threadId: ThreadId }
  /**
   * Bring devices up to date, resending to `deviceId` even what it was already sent.
   * `activityOff` when that device just registered with its activity turned off.
   */
  | { readonly _tag: "sync"; readonly deviceId?: PushDeviceId; readonly activityOff?: boolean }
  /** Publish again because time passed, not because a thread changed. */
  | { readonly _tag: "refresh" }
  /** Take the activity off a device that was just removed. */
  | { readonly _tag: "end"; readonly device: PushDevices.PushDeviceRecord };

interface Alert {
  readonly kind: PushAlertKind;
  readonly notification: PushNotification;
}

interface Activity {
  readonly state: PushActivityState | null;
  readonly expiresAt: number | undefined;
  readonly identity: string;
  readonly counts: string;
}

const make = Effect.gen(function* () {
  const devices = yield* PushDevices.PushDevices;
  const gateway = yield* PushGatewayClient.PushGatewayClient;
  const signingKey = yield* PushSigningKey.PushSigningKey;
  const serverEnvironment = yield* ServerEnvironment.ServerEnvironment;
  const threads = yield* ThreadManagement.ThreadManagementService;
  const projects = yield* ProjectService.ProjectService;
  const scope = yield* Effect.scope;
  const environmentId = yield* serverEnvironment.getEnvironmentId;
  const gatewayUrls = (yield* Config.String("ONICODE_PUSH_GATEWAY_URLS").pipe(
    // The repository names no gateway: one serves a single app's builds. A build of that app
    // bakes its own in, and an install can still name another.
    Config.withDefault(builtInPushGatewayUrls),
    Effect.orDie,
  ))
    .split(",")
    .map((value) => pushGatewayOrigin(value.trim()))
    .filter((origin) => origin !== null);

  // With no device registered nothing is tracked, so an idle install does no work per event.
  let tracking = false;
  const states = new Map<ThreadId, AgentAwarenessState>();
  const confirmDeadlines = new Map<ThreadId, number>();
  /** The activity each device last received, to skip what would look the same. */
  const delivered = new Map<PushDeviceId, Activity>();
  /**
   * When each device with work under way next needs a publish though nothing changed: to refresh
   * its card, or to send a start that had to wait.
   */
  const refreshDueAt = new Map<PushDeviceId, number>();
  /** Set while thread changes wait for the queue to empty before the activity is published. */
  let activityStale = false;
  let expiry: Fiber.Fiber<void> | undefined;
  let keepalive: Fiber.Fiber<void> | undefined;

  const nowIso = DateTime.now.pipe(Effect.map(DateTime.formatIso));

  const project = Effect.fn("PushNotifications.project")(function* (thread: AgentAwarenessThread) {
    const owner = yield* projects.getById(thread.projectId);
    return Option.isNone(owner)
      ? null
      : projectThreadAwarenessV2({ environmentId, project: owner.value, thread });
  });

  const seed = Effect.fn("PushNotifications.seed")(function* () {
    // Track first: a change that lands during the read is queued, and re-read after it.
    tracking = true;
    const snapshot = yield* threads.getAgentAwarenessSnapshot();
    states.clear();
    confirmDeadlines.clear();
    for (const thread of snapshot.threads) {
      if (thread.archivedAt !== null) continue;
      const state = yield* project(thread);
      if (state !== null) states.set(thread.id, state);
    }
  });

  const stopTracking = Effect.sync(() => {
    tracking = false;
    states.clear();
    confirmDeadlines.clear();
    delivered.clear();
    refreshDueAt.clear();
    activityStale = false;
  });

  const send = Effect.fn("PushNotifications.send")(function* (
    device: PushDevices.PushDeviceRecord,
    kind: PushDevices.PushDeviceGrantKind,
    grant: string,
    content: Pick<PushMessage, "notification" | "activity">,
  ) {
    const { deviceId } = device;
    const dropGrant = devices.setGrant({ deviceId, kind, grant: null });
    const result = yield* gateway
      .deliver({
        gatewayUrl: device.gatewayUrl,
        grant,
        message: { deviceId, environmentId, ...content },
      })
      .pipe(
        // An alert is worth a second try; a missed activity update is replaced by the next one.
        Effect.retry({
          times: content.notification === undefined ? 0 : 1,
          schedule: Schedule.spaced("2 seconds"),
          while: (error) => error.retryable,
        }),
        Effect.result,
      );
    const at = yield* nowIso;
    if (Result.isFailure(result)) {
      const error = result.failure;
      yield* Effect.logWarning("push delivery failed", {
        deviceId,
        kind,
        reason: error.reason,
        detail: error.detail,
      });
      if (error.reason === "grant") yield* dropGrant;
      yield* devices.recordDelivery({ deviceId, at, failure: error.message });
      return { outcome: "failed", detail: error.message } as const;
    }
    const outcome = result.success;
    if (outcome === "unregistered") yield* dropGrant;
    yield* devices.recordDelivery({
      deviceId,
      at,
      failure: outcome === "unregistered" ? "The device no longer accepts pushes." : null,
    });
    return { outcome } as const;
  });

  const usable = (grant: string | null, expiresAt: number | null, now: number) =>
    grant !== null && expiresAt !== null && expiresAt > now ? grant : null;

  const deliverTo = Effect.fn("PushNotifications.deliverTo")(function* (
    device: PushDevices.PushDeviceRecord,
    input: {
      readonly alert: Alert | null;
      /** Absent when only the alert is going out. */
      readonly activity: Activity | undefined;
      readonly now: number;
    },
  ) {
    const { now } = input;
    const { deviceId } = device;
    const notificationGrant = usable(
      device.notificationGrant,
      device.notificationGrantExpiresAt,
      now,
    );
    const notification =
      input.alert !== null &&
      notificationGrant !== null &&
      pushAlertAllowed(device.preferences, input.alert.kind)
        ? input.alert.notification
        : undefined;
    if (!device.preferences.activity) refreshDueAt.delete(deviceId);
    const refreshDue = refreshDueAt.get(deviceId);
    const activity =
      input.activity !== undefined &&
      device.preferences.activity &&
      (delivered.get(deviceId)?.identity !== input.activity.identity ||
        (refreshDue !== undefined && now >= refreshDue))
        ? input.activity
        : undefined;

    if (device.platform === "android") {
      // With nothing to send through, a refresh left due would wake the keepalive every minute.
      if (notificationGrant === null) refreshDueAt.delete(deviceId);
      if (notificationGrant === null || (notification === undefined && activity === undefined)) {
        return;
      }
      const result = yield* send(device, "notification", notificationGrant, {
        ...(notification === undefined ? {} : { notification }),
        ...(activity === undefined
          ? {}
          : {
              activity: {
                state: activity.state,
                ...(activity.expiresAt === undefined ? {} : { expiresAt: activity.expiresAt }),
              },
            }),
      });
      const working = (activity?.state?.activeCount ?? 0) > 0;
      if (activity !== undefined && result.outcome === "delivered") {
        delivered.set(deviceId, activity);
        if (working) refreshDueAt.set(deviceId, now + PUSH_ANDROID_ACTIVITY_KEEPALIVE_MS);
        else refreshDueAt.delete(deviceId);
      } else if (working && result.outcome === "failed") {
        // Work that goes on unchanged publishes nothing that would send this again.
        refreshDueAt.set(deviceId, now);
      }
      return;
    }

    if (notification !== undefined && notificationGrant !== null) {
      yield* send(device, "notification", notificationGrant, { notification });
    }
    if (activity === undefined) return;
    const { state } = activity;
    const working = state !== null && state.activeCount > 0;
    const startedAt = device.liveActivityStartedAt;
    // Nothing a start put on screen outlives it, so past that age the start holds nothing back.
    const outlived = startedAt !== null && now - startedAt >= LIVE_ACTIVITY_LIFETIME_MS;
    // The stretch of work is over, so the next one may start an activity again.
    if (startedAt !== null && (!working || outlived)) {
      yield* devices.setLiveActivityStartedAt({ deviceId, at: null });
    }
    // APNs keeps accepting updates for a card iOS has ended, so its age is what says it is gone.
    const replacing = working && outlived && device.liveActivityUpdateGrant !== null;
    if (replacing) yield* devices.setGrant({ deviceId, kind: "liveActivityUpdate", grant: null });
    const updateGrant = replacing
      ? null
      : usable(device.liveActivityUpdateGrant, device.liveActivityUpdateGrantExpiresAt, now);
    if (updateGrant !== null) {
      // APNs holds a low-priority update back, sometimes for minutes, so a count must not ride one.
      const urgent = delivered.get(deviceId)?.counts !== activity.counts;
      const result = yield* send(device, "liveActivityUpdate", updateGrant, {
        activity: { state, ...(urgent ? { urgent } : {}) },
      });
      if (result.outcome === "delivered") {
        delivered.set(deviceId, activity);
        if (working) {
          refreshDueAt.set(deviceId, now + PUSH_ACTIVITY_KEEPALIVE_MS);
        } else {
          refreshDueAt.delete(deviceId);
          // The gateway ended the activity, so its token is spent.
          yield* devices.setGrant({ deviceId, kind: "liveActivityUpdate", grant: null });
        }
      } else if (working && result.outcome === "failed") {
        // Whatever was due waits for something else, such as the end of a start's hold.
        refreshDueAt.set(deviceId, now);
      }
      // A card swiped away is gone, and its start goes on holding back another one below.
      if (result.outcome !== "unregistered") return;
    }
    // With no card, nothing is shown to match, so every publish comes back here.
    refreshDueAt.delete(deviceId);
    delivered.delete(deviceId);
    if (!working) {
      delivered.set(deviceId, activity);
      return;
    }
    const startGrant = usable(
      device.liveActivityStartGrant,
      device.liveActivityStartGrantExpiresAt,
      now,
    );
    if (startGrant === null) return;
    // One start per stretch of work: the app answers with an update grant, or ends the card.
    // It is kept in the database because a restart must not spend iOS's start budget again.
    if (startedAt !== null && !outlived) {
      // Work that goes on unchanged publishes nothing, so the end of the hold needs a wake-up.
      refreshDueAt.set(deviceId, startedAt + LIVE_ACTIVITY_LIFETIME_MS);
      return;
    }
    const result = yield* send(device, "liveActivityStart", startGrant, { activity: { state } });
    if (result.outcome === "delivered") {
      // Read again: the gateway counts the push's five minutes from when it took the push.
      const at = yield* Clock.currentTimeMillis;
      yield* devices.setLiveActivityStartedAt({ deviceId, at });
      refreshDueAt.set(deviceId, at + LIVE_ACTIVITY_LIFETIME_MS);
    } else if (result.outcome === "failed") {
      refreshDueAt.set(deviceId, now);
    }
    // Otherwise iOS has retired the token: it started nothing, so the app's next one may.
  });

  /** Best effort: a removed device keeps a card nothing would ever update. */
  const endActivity = Effect.fn("PushNotifications.endActivity")(function* (
    device: PushDevices.PushDeviceRecord,
  ) {
    delivered.delete(device.deviceId);
    refreshDueAt.delete(device.deviceId);
    const now = yield* Clock.currentTimeMillis;
    const grant =
      device.platform === "android"
        ? usable(device.notificationGrant, device.notificationGrantExpiresAt, now)
        : usable(device.liveActivityUpdateGrant, device.liveActivityUpdateGrantExpiresAt, now);
    if (grant === null) return;
    yield* gateway
      .deliver({
        gatewayUrl: device.gatewayUrl,
        grant,
        message: { deviceId: device.deviceId, environmentId, activity: { state: null } },
      })
      .pipe(
        Effect.catch((error) =>
          Effect.logWarning("push activity could not be ended for a removed device", {
            deviceId: device.deviceId,
            reason: error.reason,
          }),
        ),
      );
  });

  let enqueue: (work: Work) => Effect.Effect<void> = () => Effect.void;

  const currentActivity = Effect.fn("PushNotifications.currentActivity")(function* (now: number) {
    const { label } = yield* serverEnvironment.getDescriptor;
    const shown = pushActivityState({ title: label, states: [...states.values()], nowMs: now });
    const state = shown?.state ?? null;

    if (expiry !== undefined) yield* Fiber.interrupt(expiry);
    expiry =
      shown?.expiresAt === undefined
        ? undefined
        : yield* Effect.sleep(Math.max(0, shown.expiresAt - now) + 1_000).pipe(
            Effect.andThen(Effect.suspend(() => enqueue({ _tag: "refresh" }))),
            Effect.forkIn(scope),
          );
    return {
      state,
      expiresAt: shown?.expiresAt,
      identity: pushActivityIdentity(state),
      counts: pushActivityCounts(state),
    } satisfies Activity;
  });

  /** Sends an alert, and the activity too unless `alertOnly` leaves it for a later publish. */
  const publish = Effect.fn("PushNotifications.publish")(function* (
    alert: Alert | null,
    alertOnly = false,
  ) {
    const now = yield* Clock.currentTimeMillis;
    const registered = yield* devices.list(yield* nowIso);
    if (registered.length === 0) {
      yield* stopTracking;
      return;
    }
    const activity = alertOnly ? undefined : yield* currentActivity(now);
    if (!alertOnly) activityStale = false;
    // A device whose session ended is no longer listed, and must not keep the keepalive awake.
    const known = new Set(registered.map((device) => device.deviceId));
    for (const deviceId of [...refreshDueAt.keys(), ...delivered.keys()]) {
      if (known.has(deviceId)) continue;
      refreshDueAt.delete(deviceId);
      delivered.delete(deviceId);
    }

    yield* Effect.forEach(
      registered,
      (device) =>
        deliverTo(device, { alert, activity, now }).pipe(
          // One device's failure must not cost the others their push.
          Effect.catch((error) =>
            Effect.logWarning("push device state could not be saved", {
              deviceId: device.deviceId,
              error,
            }),
          ),
        ),
      { concurrency: 4, discard: true },
    );

    // A phone calls a card out of date, or drops it, when nothing reaches it while agents work.
    if (keepalive !== undefined) yield* Fiber.interrupt(keepalive);
    const nextRefresh = Math.min(...refreshDueAt.values());
    keepalive =
      refreshDueAt.size === 0
        ? undefined
        : yield* Effect.sleep(
            // A delivery that keeps failing must not turn this into a tight loop.
            Math.max(PUSH_ACTIVITY_KEEPALIVE_RETRY_MS, nextRefresh - now),
          ).pipe(
            Effect.andThen(Effect.suspend(() => enqueue({ _tag: "refresh" }))),
            Effect.forkIn(scope),
          );
  });

  const processThread = Effect.fn("PushNotifications.processThread")(function* (
    threadId: ThreadId,
  ) {
    if (!tracking) return null;
    const now = yield* Clock.currentTimeMillis;
    const found = (yield* threads.getAgentAwarenessSnapshot(threadId)).threads[0];
    const thread = found === undefined || found.archivedAt !== null ? undefined : found;
    const next = thread === undefined ? null : yield* project(thread);
    const previous = states.get(threadId);

    if (isLive(previous) && (next === null || next.phase === "completed")) {
      const deadline = confirmDeadlines.get(threadId);
      if (deadline === undefined) {
        confirmDeadlines.set(threadId, now + CONFIRM_DELAY_MS);
        yield* Effect.sleep(CONFIRM_DELAY_MS).pipe(
          Effect.andThen(Effect.suspend(() => enqueue({ _tag: "thread", threadId }))),
          Effect.forkIn(scope),
        );
        return null;
      }
      if (now < deadline) return null;
    }
    confirmDeadlines.delete(threadId);

    // A thread that settles as it finishes shows nothing, but its result is still news.
    const settled =
      next === null && isLive(previous) && thread?.settledOverride === "settled"
        ? yield* project({ ...thread, settledOverride: null })
        : null;
    const news = next ?? (settled?.phase === "completed" ? settled : null);
    const kind =
      news === null
        ? null
        : pushAlertKind({
            previous: previous?.phase ?? null,
            next: news.phase,
            endedAtMs:
              thread?.latestRunCompletedAt == null
                ? null
                : DateTime.toEpochMillis(thread.latestRunCompletedAt),
            nowMs: now,
          });

    if (next === null) states.delete(threadId);
    else states.set(threadId, next);
    if (kind === null && shownAs(previous) === shownAs(next)) return null;
    activityStale = true;
    const notification =
      kind === null || news === null
        ? null
        : pushNotificationForState(
            news,
            NodeCrypto.createHash("sha256")
              .update(`${threadId}.${news.phase}.${now}`)
              .digest("base64url"),
          );
    return kind === null || notification === null ? null : { kind, notification };
  });

  const queuedThreads = new Set<ThreadId>();
  const worker = yield* makeDrainableWorker((work: Work) =>
    Effect.gen(function* () {
      switch (work._tag) {
        case "thread": {
          queuedThreads.delete(work.threadId);
          const alert = yield* processThread(work.threadId);
          // Threads that change together cost each device one activity update, not one each.
          const alertOnly = queuedThreads.size > 0;
          if (alert === null && (alertOnly || !activityStale)) return;
          return yield* publish(alert, alertOnly);
        }
        case "end":
          return yield* endActivity(work.device);
        case "sync":
          if (work.deviceId !== undefined) delivered.delete(work.deviceId);
          if (work.deviceId !== undefined && work.activityOff) {
            // The app ends its card with the setting, so turning it back on starts from nothing.
            // Done in turn here, so a start still on its way cannot record itself afterwards.
            const { deviceId } = work;
            yield* devices.setGrant({ deviceId, kind: "liveActivityUpdate", grant: null });
            yield* devices.setLiveActivityStartedAt({ deviceId, at: null });
          }
          if (!tracking) yield* seed().pipe(Effect.tapCause(() => stopTracking));
          return yield* publish(null);
        case "refresh":
          return yield* tracking ? publish(null) : Effect.void;
      }
    }),
  );
  enqueue = (work) =>
    Effect.suspend(() => {
      if (work._tag !== "thread") return worker.enqueue(work);
      // One queued read covers every event a thread emits before it runs.
      if (queuedThreads.has(work.threadId)) return Effect.void;
      queuedThreads.add(work.threadId);
      return worker.enqueue(work);
    }).pipe(Effect.uninterruptible);

  const owned = Effect.fn("PushNotifications.owned")(function* (
    deviceId: PushDeviceId,
    caller: PushDeviceCaller,
  ) {
    const device = yield* devices.get({ deviceId, now: yield* nowIso });
    if (
      Option.isSome(device) &&
      device.value.sessionId !== caller.sessionId &&
      !caller.managesAllDevices
    ) {
      return yield* new PushDeviceNotOwnedError({ deviceId });
    }
    return device;
  });

  return PushNotifications.of({
    registrationInfo: { environmentKey: signingKey.publicKey, gatewayUrls },

    listDevices: Effect.fn("PushNotifications.listDevices")(function* (caller) {
      const registered = yield* devices.list(yield* nowIso);
      const visible = caller.seesAllDevices
        ? registered
        : registered.filter((device) => device.sessionId === caller.sessionId);
      return visible.map((device) => ({
        deviceId: device.deviceId,
        platform: device.platform,
        label: device.label,
        preferences: device.preferences,
        alerts: device.notificationGrant !== null,
        registeredAt: device.registeredAt,
        lastDeliveredAt: device.lastDeliveredAt,
        lastFailure: device.lastFailure,
        current: device.sessionId === caller.sessionId,
      }));
    }),

    registerDevice: Effect.fn("PushNotifications.registerDevice")(function* (input) {
      const { request } = input;
      const gatewayUrl = pushGatewayOrigin(request.gatewayUrl);
      if (gatewayUrl === null || !gatewayUrls.includes(gatewayUrl)) {
        return yield* new PushGatewayNotAllowedError({ gatewayUrl: request.gatewayUrl });
      }
      // Registering over another session's device would silence it.
      yield* owned(input.deviceId, input.caller);
      const liveActivityStart = request.platform === "ios" ? request.liveActivityStart : null;
      yield* devices.register({
        deviceId: input.deviceId,
        sessionId: input.caller.sessionId,
        platform: request.platform,
        label: request.label,
        gatewayUrl,
        preferences: request.preferences,
        notificationGrant: request.notification?.grant ?? null,
        notificationGrantExpiresAt: request.notification?.expiresAt ?? null,
        liveActivityStartGrant: liveActivityStart?.grant ?? null,
        liveActivityStartGrantExpiresAt: liveActivityStart?.expiresAt ?? null,
        registeredAt: yield* DateTime.now,
      });
      yield* enqueue({
        _tag: "sync",
        deviceId: input.deviceId,
        activityOff: !request.preferences.activity,
      });
    }),

    registerLiveActivity: Effect.fn("PushNotifications.registerLiveActivity")(function* (input) {
      const device = yield* owned(input.deviceId, input.caller);
      if (Option.isNone(device)) {
        return yield* new PushDeviceNotFoundError({ deviceId: input.deviceId });
      }
      const known = yield* devices.setGrant({
        deviceId: input.deviceId,
        kind: "liveActivityUpdate",
        grant: input.grant,
      });
      if (!known) return yield* new PushDeviceNotFoundError({ deviceId: input.deviceId });
      yield* enqueue({ _tag: "sync", deviceId: input.deviceId });
    }),

    unregisterDevice: Effect.fn("PushNotifications.unregisterDevice")(function* (input) {
      const device = yield* owned(input.deviceId, input.caller);
      yield* devices.remove(input.deviceId);
      if (Option.isSome(device)) yield* enqueue({ _tag: "end", device: device.value });
      yield* enqueue({ _tag: "refresh" });
    }),

    sendTest: Effect.fn("PushNotifications.sendTest")(function* (input) {
      const device = yield* owned(input.deviceId, input.caller);
      if (Option.isNone(device)) {
        return yield* new PushDeviceNotFoundError({ deviceId: input.deviceId });
      }
      const now = yield* Clock.currentTimeMillis;
      const grant = usable(
        device.value.notificationGrant,
        device.value.notificationGrantExpiresAt,
        now,
      );
      if (grant === null) {
        return { outcome: "failed", detail: "This device has not allowed notifications." } as const;
      }
      const { label } = yield* serverEnvironment.getDescriptor;
      return yield* send(device.value, "notification", grant, {
        notification: {
          id: `test_${now}`,
          title: "Onicode",
          body: `Notifications from ${label} are working.`.slice(0, 240),
          environmentId,
          threadId: "",
          deepLink: "/",
        },
      });
    }),

    start: Effect.fn("PushNotifications.start")(function* () {
      yield* forkParked(
        Stream.runForEach(threads.streamDomainEvents, (event) =>
          tracking && changesPushActivity(event)
            ? enqueue({ _tag: "thread", threadId: event.threadId })
            : Effect.void,
        ),
      );
      // Devices registered before a restart get the current picture, with nothing to alert about.
      const now = yield* nowIso;
      yield* forkParked(
        devices.prune(now).pipe(
          Effect.andThen(devices.list(now)),
          Effect.flatMap((registered) =>
            registered.length === 0 ? Effect.void : enqueue({ _tag: "sync" }),
          ),
          Effect.catch((error) =>
            Effect.logWarning("push devices could not be read at startup", { error }),
          ),
        ),
      );
    }),

    drain: worker.drain,
  });
});

export const layer = Layer.effect(PushNotifications, make);
