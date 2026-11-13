import { assert, describe, it } from "@effect/vitest";
import {
  AuthSessionId,
  EnvironmentId,
  type OrchestrationV2DomainEvent,
  type OrchestrationV2ThreadShell,
  ProjectId,
  ProviderInstanceId,
  type PushDeviceId,
  type PushRegisterDeviceRequest,
  RuntimeRequestId,
  ThreadId,
} from "@onicode/contracts";
import type { PushMessage } from "@onicode/contracts/pushGateway";
import * as ConfigProvider from "effect/ConfigProvider";
import * as Context from "effect/Context";
import * as DateTime from "effect/DateTime";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Layer from "effect/Layer";
import * as Queue from "effect/Queue";
import * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";
import * as SqlClient from "effect/sql/SqlClient";
import * as TestClock from "effect/testing/TestClock";

import * as ServerEnvironment from "../environment/ServerEnvironment.ts";
import * as ThreadManagement from "../orchestration-v2/ThreadManagementService.ts";
import * as SqlitePersistence from "../persistence/Sqlite.ts";
import * as ProjectService from "../project/ProjectService.ts";
import * as PushDevices from "./PushDevices.ts";
import * as PushGatewayClient from "./PushGatewayClient.ts";
import * as PushNotifications from "./PushNotifications.ts";
import * as PushSigningKey from "./PushSigningKey.ts";

const THREAD = ThreadId.make("thread-1");
const PROJECT = ProjectId.make("project-1");
const SESSION = AuthSessionId.make("session-phone");
const PHONE = "device-phone-1" as PushDeviceId;
const callerOf = (sessionId: AuthSessionId): PushNotifications.PushDeviceCaller => ({
  sessionId,
  seesAllDevices: false,
  managesAllDevices: false,
});
const GATEWAY = "https://push.example.com";
const EPOCH = "1970-01-01T00:00:00.000Z";

function shell(overrides: Partial<OrchestrationV2ThreadShell> = {}): OrchestrationV2ThreadShell {
  return {
    id: THREAD,
    projectId: PROJECT,
    title: "Fix the build",
    providerInstanceId: ProviderInstanceId.make("codex"),
    modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "test-model" },
    runtimeMode: "full-access",
    interactionMode: "default",
    worktreePath: null,
    activeProviderThreadId: null,
    lineage: { rootThreadId: THREAD, parentThreadId: null, relationshipToParent: null },
    forkedFrom: null,
    createdBy: "user",
    creationSource: "web",
    activeRunId: null,
    latestVisibleMessage: null,
    hasActionableProposedPlan: false,
    itemCount: 0,
    visibleItemCount: 0,
    lastVisitedAt: null,
    deletedAt: null,
    branch: null,
    linkedPullRequest: null,
    status: "running",
    activityRunStatus: null,
    pendingRuntimeRequest: null,
    pendingBackgroundTasks: [],
    latestRunId: null,
    latestRunRequestedAt: null,
    latestRunStartedAt: null,
    latestRunCompletedAt: null,
    latestUserMessageAt: null,
    createdAt: DateTime.makeUnsafe(EPOCH),
    updatedAt: DateTime.makeUnsafe(EPOCH),
    archivedAt: null,
    settledOverride: null,
    settledAt: null,
    snoozedUntil: null,
    snoozedAt: null,
    pinnedAt: null,
    ...overrides,
  };
}

const approval = {
  id: RuntimeRequestId.make("request-1"),
  kind: "command",
  createdAt: DateTime.makeUnsafe(EPOCH),
} satisfies OrchestrationV2ThreadShell["pendingRuntimeRequest"];

const grant = (name: string, expiresAt = 60 * 24 * 60 * 60_000) => ({
  grant: `grant_${name}`,
  expiresAt,
});

const registration = (
  overrides: Partial<PushRegisterDeviceRequest> = {},
): PushRegisterDeviceRequest => ({
  platform: "android",
  label: "Pixel",
  gatewayUrl: GATEWAY,
  notification: grant("notification"),
  liveActivityStart: null,
  preferences: {
    notifyOnApproval: true,
    notifyOnInput: true,
    notifyOnCompletion: true,
    notifyOnFailure: true,
    activity: true,
  },
  ...overrides,
});

interface Delivery {
  readonly grant: string;
  readonly alert: string | undefined;
  /** The number of running agents, `null` for an ended activity, `undefined` when not sent. */
  readonly active: number | null | undefined;
}

/** A point the service stops at, once `hold` is called, until the test lets it through. */
function makeGate() {
  let held: { reached: Deferred.Deferred<void>; release: Deferred.Deferred<void> } | undefined;
  return {
    pass: Effect.suspend(() =>
      held === undefined
        ? Effect.void
        : Deferred.succeed(held.reached, undefined).pipe(
            Effect.andThen(Deferred.await(held.release)),
          ),
    ),
    hold: Effect.gen(function* () {
      held = { reached: yield* Deferred.make<void>(), release: yield* Deferred.make<void>() };
      return held;
    }),
  };
}

const setup = Effect.fn(function* (options?: {
  readonly outcome?: (delivery: Delivery) => "delivered" | "unregistered" | "failed";
  readonly onDeliver?: Effect.Effect<void>;
  /** Runs after the snapshot of every thread is read and before the service sees it. */
  readonly onSnapshot?: Effect.Effect<void>;
}) {
  const deliveries: Array<Delivery> = [];
  const messages: Array<PushMessage> = [];
  const threads = new Map<ThreadId, OrchestrationV2ThreadShell>([[THREAD, shell()]]);
  const events = yield* Queue.unbounded<OrchestrationV2DomainEvent>();
  const pulls = yield* Queue.unbounded<void>();

  const stored = yield* Layer.build(
    PushDevices.layer.pipe(Layer.provideMerge(SqlitePersistence.layerMemory)),
  );
  const layer = PushNotifications.layer.pipe(
    Layer.provide(Layer.succeedContext(stored)),
    Layer.provide([
      ConfigProvider.layer(ConfigProvider.fromEnv({ env: { ONICODE_PUSH_GATEWAY_URLS: GATEWAY } })),
      Layer.succeed(
        PushGatewayClient.PushGatewayClient,
        PushGatewayClient.PushGatewayClient.of({
          deliver: Effect.fnUntraced(function* (input) {
            const delivery: Delivery = {
              grant: input.grant,
              alert: input.message.notification?.body,
              active:
                input.message.activity === undefined
                  ? undefined
                  : (input.message.activity.state?.activeCount ?? null),
            };
            deliveries.push(delivery);
            messages.push(input.message);
            yield* options?.onDeliver ?? Effect.void;
            const outcome = options?.outcome?.(delivery) ?? "delivered";
            if (outcome !== "failed") return outcome;
            return yield* new PushGatewayClient.PushDeliveryError({
              reason: "provider",
              retryable: false,
              detail: "The provider is down.",
            });
          }),
        }),
      ),
      Layer.succeed(
        PushSigningKey.PushSigningKey,
        PushSigningKey.PushSigningKey.of({ publicKey: "A".repeat(43), sign: () => "signature" }),
      ),
      Layer.mock(ServerEnvironment.ServerEnvironment)({
        getEnvironmentId: Effect.succeed(EnvironmentId.make("env-1")),
        getDescriptor: Effect.succeed({
          environmentId: EnvironmentId.make("env-1"),
          label: "mac-mini",
          platform: { os: "darwin", arch: "arm64" },
          serverVersion: "0.0.0-test",
          capabilities: { repositoryIdentity: false },
        }),
      }),
      Layer.mock(ProjectService.ProjectService)({
        getById: () =>
          Effect.succeedSome({
            id: PROJECT,
            title: "onicode",
            workspaceRoot: "/workspace",
            defaultModelSelection: null,
            scripts: [],
            createdAt: EPOCH,
            updatedAt: EPOCH,
            deletedAt: null,
          }),
      }),
      Layer.mock(ThreadManagement.ThreadManagementService)({
        getAgentAwarenessSnapshot: (threadId) =>
          Effect.sync(() => ({
            snapshotSequence: 1,
            threads: [...threads.values()]
              .filter((thread) => threadId === undefined || thread.id === threadId)
              .map((thread) => ({ ...thread, lastVisitedAt: thread.lastVisitedAt ?? null })),
          })).pipe(
            Effect.tap(() =>
              threadId === undefined ? (options?.onSnapshot ?? Effect.void) : Effect.void,
            ),
          ),
        streamDomainEvents: Stream.fromEffectRepeat(
          Queue.offer(pulls, undefined).pipe(Effect.andThen(Queue.take(events))),
        ),
      }),
    ]),
  );
  const sql = Context.get(stored, SqlClient.SqlClient);
  const devices = Context.get(stored, PushDevices.PushDevices);

  yield* sql`
    INSERT INTO auth_sessions (
      session_id, subject, scopes, client_device_type, issued_at, expires_at
    )
    VALUES (${SESSION}, 'owner', '[]', 'mobile', ${EPOCH}, '2100-01-01T00:00:00.000Z')
  `;

  let running = yield* Scope.make();
  yield* Effect.addFinalizer(() => Scope.close(running, Exit.void));
  const boot = Effect.gen(function* () {
    const service = Context.get(
      yield* Layer.buildWithScope(layer, running),
      PushNotifications.PushNotifications,
    );
    yield* service.start().pipe(Scope.provide(running));
    yield* Queue.take(pulls);
    yield* service.drain;
    return service;
  });
  let push = yield* boot;
  /** Stops the service and starts another over the same database, as a server restart does. */
  const restart = Effect.gen(function* () {
    yield* Scope.close(running, Exit.void);
    running = yield* Scope.make();
    push = yield* boot;
    return push;
  });

  /** Lets the service see a thread change without waiting for it to act. */
  const emit = Effect.fn(function* (
    overrides: Partial<OrchestrationV2ThreadShell>,
    type: OrchestrationV2DomainEvent["type"] = "run.updated",
  ) {
    threads.set(overrides.id ?? THREAD, shell(overrides));
    // The service reads only an event's type and thread id.
    yield* Queue.offer(events, {
      type,
      threadId: overrides.id ?? THREAD,
    } as unknown as OrchestrationV2DomainEvent);
    yield* Queue.take(pulls);
  });
  /** Changes a thread the way the orchestrator would, and waits for the service to act on it. */
  const change = Effect.fn(function* (
    overrides: Partial<OrchestrationV2ThreadShell>,
    type?: OrchestrationV2DomainEvent["type"],
  ) {
    yield* emit(overrides, type);
    yield* push.drain;
  });
  const register = Effect.fn(function* (
    request = registration(),
    deviceId = PHONE,
    sessionId = SESSION,
  ) {
    yield* push.registerDevice({ caller: callerOf(sessionId), deviceId, request });
    yield* push.drain;
  });
  /** What was delivered since the last call. */
  const taken = () => deliveries.splice(0);

  return { push, devices, sql, emit, change, register, restart, taken, messages };
});

const finishedAt = (millis: number) => ({
  status: "completed" as const,
  latestRunCompletedAt: DateTime.makeUnsafe(millis),
});

describe("PushNotifications", () => {
  it.effect("does nothing per event until a device registers", () =>
    Effect.gen(function* () {
      const { change, register, taken, messages } = yield* setup();

      yield* change({ pendingRuntimeRequest: approval });
      assert.deepStrictEqual(taken(), []);

      // Registering shows what is already going on, and does not alert about it.
      yield* register();
      assert.deepStrictEqual(taken(), [
        { grant: "grant_notification", alert: undefined, active: 1 },
      ]);
      assert.include(messages[0], { deviceId: PHONE, environmentId: "env-1" });
      assert.strictEqual(messages[0]?.activity?.state?.title, "mac-mini");
    }),
  );

  it.effect("alerts an Android device on the transitions it asked for, in one message", () =>
    Effect.gen(function* () {
      let outcome: "failed" | "delivered" = "delivered";
      const { push, change, register, taken, messages } = yield* setup({ outcome: () => outcome });
      yield* register(
        registration({
          preferences: { ...registration().preferences, notifyOnCompletion: false },
        }),
      );
      taken();

      yield* change({ pendingRuntimeRequest: approval });
      assert.deepStrictEqual(taken(), [
        { grant: "grant_notification", alert: "Approval needed · onicode", active: 1 },
      ]);
      // APNs collapses on this id and caps it at 64 bytes.
      assert.match(messages.at(-1)?.notification?.id ?? "", /^[\w-]{43}$/);

      // Nothing visible changed, so nothing is sent.
      yield* change({ pendingRuntimeRequest: approval, itemCount: 4 });
      yield* change({});
      assert.deepStrictEqual(taken(), [
        { grant: "grant_notification", alert: undefined, active: 1 },
      ]);

      // A finished run waits for its continuation before it counts as finished.
      yield* change(finishedAt(1_000));
      assert.deepStrictEqual(taken(), []);
      yield* TestClock.adjust("5 seconds");
      yield* push.drain;
      assert.deepStrictEqual(taken(), [
        { grant: "grant_notification", alert: undefined, active: 0 },
      ]);

      // Finished work leaves the phone once it is old.
      yield* TestClock.adjust("16 minutes");
      yield* push.drain;
      assert.deepStrictEqual(taken(), [
        { grant: "grant_notification", alert: undefined, active: null },
      ]);

      // Android drops a card two hours after its last update, so unchanged work is sent again.
      const working = [{ grant: "grant_notification", alert: undefined, active: 1 }];
      // The first one may fail, with nothing changing afterwards to send it again.
      outcome = "failed";
      yield* change({});
      assert.deepStrictEqual(taken(), working);
      outcome = "delivered";
      yield* TestClock.adjust("1 minute");
      yield* push.drain;
      assert.deepStrictEqual(taken(), working);
      yield* TestClock.adjust("49 minutes");
      yield* push.drain;
      assert.deepStrictEqual(taken(), []);
      yield* TestClock.adjust("1 minute");
      yield* push.drain;
      assert.deepStrictEqual(taken(), working);
    }),
  );

  it.effect(
    "starts one Live Activity per stretch of work and updates it through its own grant",
    () =>
      Effect.gen(function* () {
        const { push, change, register, restart, taken } = yield* setup();
        yield* register(
          registration({ platform: "ios", label: "iPhone", liveActivityStart: grant("start") }),
        );
        assert.deepStrictEqual(taken(), [{ grant: "grant_start", alert: undefined, active: 1 }]);

        // Until the app answers the start with an update grant, there is nothing to update.
        yield* change({ title: "Renamed" });
        assert.deepStrictEqual(taken(), []);

        yield* push.registerLiveActivity({
          caller: callerOf(SESSION),
          deviceId: PHONE,
          grant: grant("update"),
        });
        yield* push.drain;
        assert.deepStrictEqual(taken(), [{ grant: "grant_update", alert: undefined, active: 1 }]);

        // Work that goes on unchanged is sent again before iOS calls the card out of date.
        yield* TestClock.adjust("7 minutes");
        yield* push.drain;
        assert.deepStrictEqual(taken(), []);
        yield* TestClock.adjust("1 minute");
        yield* push.drain;
        assert.deepStrictEqual(taken(), [{ grant: "grant_update", alert: undefined, active: 1 }]);

        yield* change({ pendingRuntimeRequest: approval });
        assert.deepStrictEqual(taken(), [
          { grant: "grant_notification", alert: "Approval needed · onicode", active: undefined },
          { grant: "grant_update", alert: undefined, active: 1 },
        ]);

        yield* change({ status: "failed", latestRunCompletedAt: DateTime.makeUnsafe(8 * 60_000) });
        assert.deepStrictEqual(taken(), [
          { grant: "grant_notification", alert: "Agent failed · onicode", active: undefined },
          { grant: "grant_update", alert: undefined, active: 0 },
        ]);

        // The ended activity's grant is spent, so new work starts a new one.
        yield* change({});
        assert.deepStrictEqual(taken(), [{ grant: "grant_start", alert: undefined, active: 1 }]);

        // A restart in the middle of that work must not spend another start.
        const restarted = yield* restart;
        assert.deepStrictEqual(taken(), []);

        yield* restarted.registerLiveActivity({
          caller: callerOf(SESSION),
          deviceId: PHONE,
          grant: grant("update"),
        });
        yield* restarted.drain;
        assert.deepStrictEqual(taken(), [{ grant: "grant_update", alert: undefined, active: 1 }]);

        // Removing the device takes its card off the phone.
        yield* restarted.unregisterDevice({ deviceId: PHONE, caller: callerOf(SESSION) });
        yield* restarted.drain;
        assert.deepStrictEqual(taken(), [
          { grant: "grant_update", alert: undefined, active: null },
        ]);
      }),
  );

  it.effect("tries a Live Activity start again after one that failed, not one swiped away", () =>
    Effect.gen(function* () {
      let outcome: "failed" | "delivered" | "unregistered" = "unregistered";
      const { push, change, register, taken } = yield* setup({ outcome: () => outcome });
      const started = [{ grant: "grant_start", alert: undefined, active: 1 }];
      const ios = registration({
        platform: "ios",
        label: "iPhone",
        liveActivityStart: grant("start"),
      });
      yield* register(ios);
      assert.deepStrictEqual(taken(), started);

      // A token iOS retired started nothing, so the one the app registers next gets its try.
      outcome = "failed";
      yield* register(ios);
      assert.deepStrictEqual(taken(), started);

      outcome = "delivered";
      yield* change({ title: "Renamed" });
      assert.deepStrictEqual(taken(), started);

      outcome = "unregistered";
      yield* push.registerLiveActivity({
        caller: callerOf(SESSION),
        deviceId: PHONE,
        grant: grant("update"),
      });
      yield* push.drain;
      assert.deepStrictEqual(taken(), [{ grant: "grant_update", alert: undefined, active: 1 }]);

      outcome = "delivered";
      yield* change({ title: "Renamed again" });
      assert.deepStrictEqual(taken(), []);

      // iOS ends a card eight hours after its start, so that start holds nothing back any more,
      // and work that goes on unchanged gets its new card without having to change first.
      yield* TestClock.adjust("484 minutes");
      yield* push.drain;
      assert.deepStrictEqual(taken(), []);
      yield* TestClock.adjust("1 minute");
      yield* push.drain;
      assert.deepStrictEqual(taken(), started);

      // A first update that fails is tried again in a minute, not when the start's hold ends.
      const updated = [{ grant: "grant_update", alert: undefined, active: 1 }];
      outcome = "failed";
      yield* push.registerLiveActivity({
        caller: callerOf(SESSION),
        deviceId: PHONE,
        grant: grant("update"),
      });
      yield* push.drain;
      assert.deepStrictEqual(taken(), updated);
      outcome = "delivered";
      yield* TestClock.adjust("1 minute");
      yield* push.drain;
      assert.deepStrictEqual(taken(), updated);
      // A card that reached that age has ended on the phone, whatever APNs still accepts for it.
      yield* TestClock.adjust("488 minutes");
      yield* push.drain;
      // The keepalive goes on refreshing the card until the first one past that age replaces it.
      const sent = taken();
      assert.deepStrictEqual(sent.at(-1), started[0]);
      assert.deepStrictEqual(
        sent.filter((delivery) => delivery.grant === "grant_start"),
        started,
      );

      // The app may never answer a start, as when it was closed: the next one still comes.
      yield* TestClock.adjust("485 minutes");
      yield* push.drain;
      assert.deepStrictEqual(taken(), started);
    }),
  );

  it.effect("starts a Live Activity again once it is turned back on", () =>
    Effect.gen(function* () {
      const gate = makeGate();
      const { push, register, taken } = yield* setup({ onDeliver: gate.pass });
      const ios = (activity: boolean) =>
        registration({
          platform: "ios",
          label: "iPhone",
          liveActivityStart: grant("start"),
          preferences: { ...registration().preferences, activity },
        });
      yield* register(ios(true));
      yield* push.registerLiveActivity({
        caller: callerOf(SESSION),
        deviceId: PHONE,
        grant: grant("update"),
      });
      yield* push.drain;
      taken();

      // The app ends its card when the setting goes off, so the update grant names nothing.
      yield* register(ios(false));
      assert.deepStrictEqual(taken(), []);
      const started = [{ grant: "grant_start", alert: undefined, active: 1 }];
      yield* register(ios(true));
      assert.deepStrictEqual(taken(), started);

      // A start still on its way when the setting goes off must not hold back the next one.
      yield* register(ios(false));
      const held = yield* gate.hold;
      const caller = callerOf(SESSION);
      yield* push.registerDevice({ caller, deviceId: PHONE, request: ios(true) });
      yield* Deferred.await(held.reached);
      yield* push.registerDevice({ caller, deviceId: PHONE, request: ios(false) });
      yield* Deferred.succeed(held.release, undefined);
      yield* push.drain;
      assert.deepStrictEqual(taken(), started);
      yield* register(ios(true));
      assert.deepStrictEqual(taken(), started);
    }),
  );

  it.effect("publishes the activity once for threads that change together", () =>
    Effect.gen(function* () {
      const gate = makeGate();
      const { push, emit, register, taken } = yield* setup({ onDeliver: gate.pass });
      yield* register();
      taken();

      const held = yield* gate.hold;
      yield* emit({ pendingRuntimeRequest: approval });
      yield* Deferred.await(held.reached);
      yield* emit({ id: ThreadId.make("thread-2") });
      yield* emit({ id: ThreadId.make("thread-3") });
      yield* Deferred.succeed(held.release, undefined);
      yield* push.drain;

      assert.deepStrictEqual(taken(), [
        { grant: "grant_notification", alert: "Approval needed · onicode", active: 1 },
        { grant: "grant_notification", alert: undefined, active: 3 },
      ]);
    }),
  );

  it.effect("acts on a change that lands while the first registration reads the threads", () =>
    Effect.gen(function* () {
      const gate = makeGate();
      const { push, emit, taken } = yield* setup({ onSnapshot: gate.pass });

      const held = yield* gate.hold;
      yield* push.registerDevice({
        caller: callerOf(SESSION),
        deviceId: PHONE,
        request: registration(),
      });
      yield* Deferred.await(held.reached);
      yield* emit({ pendingRuntimeRequest: approval });
      yield* Deferred.succeed(held.release, undefined);
      yield* push.drain;

      assert.deepStrictEqual(taken(), [
        { grant: "grant_notification", alert: undefined, active: 1 },
        { grant: "grant_notification", alert: "Approval needed · onicode", active: 1 },
      ]);
    }),
  );

  it.effect("alerts about a thread that settles as it finishes", () =>
    Effect.gen(function* () {
      const { push, change, register, taken } = yield* setup();
      yield* register();
      taken();

      yield* change({ ...finishedAt(0), settledOverride: "settled" });
      yield* TestClock.adjust("5 seconds");
      yield* push.drain;
      assert.deepStrictEqual(taken(), [
        { grant: "grant_notification", alert: "Agent finished · onicode", active: null },
      ]);
    }),
  );

  it.effect("drops finished work once someone opens it, and still alerts when they watch", () =>
    Effect.gen(function* () {
      const { push, change, register, taken } = yield* setup();
      yield* register();
      taken();

      yield* change(finishedAt(0));
      yield* TestClock.adjust("5 seconds");
      yield* push.drain;
      assert.deepStrictEqual(taken(), [
        { grant: "grant_notification", alert: "Agent finished · onicode", active: 0 },
      ]);
      yield* change(
        { ...finishedAt(0), lastVisitedAt: DateTime.makeUnsafe(0) },
        "thread.visit-recorded",
      );
      assert.deepStrictEqual(taken(), [
        { grant: "grant_notification", alert: undefined, active: null },
      ]);
      // Marking it unread brings the result back, and is not news a second time.
      yield* change(finishedAt(0), "thread.marked-unread");
      assert.deepStrictEqual(taken(), [
        { grant: "grant_notification", alert: undefined, active: 0 },
      ]);

      // A thread open on another screen is seen the moment it finishes.
      yield* change({});
      taken();
      yield* change({ ...finishedAt(5_000), lastVisitedAt: DateTime.makeUnsafe(5_000) });
      yield* TestClock.adjust("5 seconds");
      yield* push.drain;
      assert.deepStrictEqual(taken(), [
        { grant: "grant_notification", alert: "Agent finished · onicode", active: null },
      ]);
    }),
  );

  it.effect("forgets a grant the provider no longer knows, and a device whose session ended", () =>
    Effect.gen(function* () {
      const { push, devices, sql, change, register, taken } = yield* setup({
        outcome: (delivery) => (delivery.alert === undefined ? "delivered" : "unregistered"),
      });
      yield* register();
      yield* change({ pendingRuntimeRequest: approval });
      taken();

      const [device] = yield* push.listDevices(callerOf(SESSION));
      assert.include(device, {
        alerts: false,
        current: true,
        lastFailure: "The device no longer accepts pushes.",
      });
      yield* change({});
      yield* change({ pendingRuntimeRequest: approval });
      assert.deepStrictEqual(taken(), []);

      yield* register();
      taken();
      yield* sql`UPDATE auth_sessions SET revoked_at = ${EPOCH} WHERE session_id = ${SESSION}`;
      yield* change({});
      assert.deepStrictEqual(taken(), []);
      const manager = { ...callerOf(SESSION), seesAllDevices: true, managesAllDevices: true };
      assert.deepStrictEqual(yield* push.listDevices(manager), []);
      assert.strictEqual(
        (yield* Effect.flip(push.sendTest({ deviceId: PHONE, caller: manager })))._tag,
        "PushDeviceNotFoundError",
      );
      assert.deepStrictEqual(taken(), []);

      // The same phone can pair again: a dead session's device is nobody's.
      const again = AuthSessionId.make("session-again");
      yield* sql`
        INSERT INTO auth_sessions (
          session_id, subject, scopes, client_device_type, issued_at, expires_at
        )
        VALUES (${again}, 'owner', '[]', 'mobile', ${EPOCH}, '2100-01-01T00:00:00.000Z')
      `;
      yield* register(registration(), PHONE, again);
      assert.strictEqual((yield* push.listDevices(callerOf(again)))[0]?.current, true);

      yield* sql`UPDATE auth_sessions SET revoked_at = ${EPOCH} WHERE session_id = ${again}`;
      yield* devices.prune(EPOCH);
      assert.deepStrictEqual(yield* sql`SELECT device_id FROM push_devices`, []);
    }),
  );

  it.effect("registers only through an allowed gateway and guards other sessions' devices", () =>
    Effect.gen(function* () {
      const { push, register, taken } = yield* setup();
      const owner = callerOf(SESSION);
      const stranger = callerOf(AuthSessionId.make("session-other"));
      const tagOf = <A, E extends { readonly _tag: string }>(effect: Effect.Effect<A, E>) =>
        Effect.flip(effect).pipe(Effect.map((error) => error._tag));

      assert.strictEqual(
        yield* tagOf(
          push.registerDevice({
            caller: owner,
            deviceId: PHONE,
            request: registration({ gatewayUrl: "https://other.example.com" }),
          }),
        ),
        "PushGatewayNotAllowedError",
      );
      assert.strictEqual(
        yield* tagOf(
          push.registerLiveActivity({ caller: owner, deviceId: PHONE, grant: grant("update") }),
        ),
        "PushDeviceNotFoundError",
      );

      yield* register();
      taken();
      assert.deepStrictEqual(yield* push.listDevices(stranger), []);
      // Registering over someone's device would silence it.
      assert.strictEqual(
        yield* tagOf(
          push.registerDevice({ caller: stranger, deviceId: PHONE, request: registration() }),
        ),
        "PushDeviceNotOwnedError",
      );
      assert.strictEqual(
        yield* tagOf(
          push.registerLiveActivity({ caller: stranger, deviceId: PHONE, grant: grant("update") }),
        ),
        "PushDeviceNotOwnedError",
      );
      assert.strictEqual(
        yield* tagOf(push.unregisterDevice({ deviceId: PHONE, caller: stranger })),
        "PushDeviceNotOwnedError",
      );
      assert.strictEqual(
        yield* tagOf(push.sendTest({ deviceId: PHONE, caller: stranger })),
        "PushDeviceNotOwnedError",
      );
      assert.deepStrictEqual(yield* push.sendTest({ deviceId: PHONE, caller: owner }), {
        outcome: "delivered",
      });
      assert.deepStrictEqual(taken(), [
        {
          grant: "grant_notification",
          alert: "Notifications from mac-mini are working.",
          active: undefined,
        },
      ]);

      yield* push.unregisterDevice({
        deviceId: PHONE,
        caller: { ...stranger, managesAllDevices: true },
      });
      yield* push.drain;
      assert.deepStrictEqual(taken(), [
        { grant: "grant_notification", alert: undefined, active: null },
      ]);
      assert.deepStrictEqual(yield* push.listDevices(owner), []);
      assert.strictEqual(
        yield* tagOf(push.sendTest({ deviceId: PHONE, caller: owner })),
        "PushDeviceNotFoundError",
      );
    }),
  );
});

describe("pushGatewayOrigin", () => {
  it.each([
    ["https://push.example.com/", "https://push.example.com"],
    ["https://push.example.com/v1?x=1", "https://push.example.com"],
    ["http://localhost:8787", "http://localhost:8787"],
    ["http://push.example.com", null],
    ["javascript:alert(1)", null],
    ["push.example.com", null],
  ])("%s is delivered to %s", (value, origin) => {
    assert.strictEqual(PushNotifications.pushGatewayOrigin(value), origin);
  });
});
