import { AuthSessionId, PushDeviceId, PushDevicePreferences } from "@onicode/contracts";
import { PushPlatform, PushSealedToken } from "@onicode/contracts/pushGateway";
import * as NodeSqliteClient from "@onicode/shared/nodeSqliteClient";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as SqlClient from "effect/sql/SqlClient";
import * as SqlSchema from "effect/sql/SqlSchema";

import { PersistenceDecodeError, PersistenceSqlError } from "../persistence/Errors.ts";

export type PushDevicesError = PersistenceSqlError | PersistenceDecodeError;

export const PushDeviceGrantKind = Schema.Literals([
  "notification",
  "liveActivityStart",
  "liveActivityUpdate",
]);
export type PushDeviceGrantKind = typeof PushDeviceGrantKind.Type;

const StoredGrant = Schema.NullOr(PushSealedToken);
const StoredExpiry = Schema.NullOr(Schema.Int);

export const PushDeviceRecord = Schema.Struct({
  deviceId: PushDeviceId,
  sessionId: AuthSessionId,
  platform: PushPlatform,
  label: Schema.String,
  gatewayUrl: Schema.String,
  preferences: Schema.fromJsonString(PushDevicePreferences),
  notificationGrant: StoredGrant,
  notificationGrantExpiresAt: StoredExpiry,
  liveActivityStartGrant: StoredGrant,
  liveActivityStartGrantExpiresAt: StoredExpiry,
  liveActivityUpdateGrant: StoredGrant,
  liveActivityUpdateGrantExpiresAt: StoredExpiry,
  /** When this stretch of work started a Live Activity. Null when work may start one. */
  liveActivityStartedAt: StoredExpiry,
  registeredAt: Schema.DateTimeUtcFromString,
  lastDeliveredAt: Schema.NullOr(Schema.DateTimeUtcFromString),
  lastFailure: Schema.NullOr(Schema.String),
});
export type PushDeviceRecord = typeof PushDeviceRecord.Type;

const Registration = PushDeviceRecord.mapFields(
  ({
    liveActivityUpdateGrant: _grant,
    liveActivityUpdateGrantExpiresAt: _expiresAt,
    liveActivityStartedAt: _startedAt,
    lastDeliveredAt: _deliveredAt,
    lastFailure: _failure,
    ...fields
  }) => fields,
);
export type PushDeviceRegistration = typeof Registration.Type;

export class PushDevices extends Context.Service<
  PushDevices,
  {
    /** Replaces a device's registration. Its Live Activity survives unless the gateway changed. */
    readonly register: (input: PushDeviceRegistration) => Effect.Effect<void, PushDevicesError>;
    /** False when the device is not registered. */
    readonly setGrant: (input: {
      readonly deviceId: PushDeviceId;
      readonly kind: PushDeviceGrantKind;
      readonly grant: { readonly grant: string; readonly expiresAt: number } | null;
    }) => Effect.Effect<boolean, PushDevicesError>;
    readonly setLiveActivityStartedAt: (input: {
      readonly deviceId: PushDeviceId;
      readonly at: number | null;
    }) => Effect.Effect<void, PushDevicesError>;
    readonly recordDelivery: (input: {
      readonly deviceId: PushDeviceId;
      readonly at: string;
      readonly failure: string | null;
    }) => Effect.Effect<void, PushDevicesError>;
    readonly remove: (deviceId: PushDeviceId) => Effect.Effect<void, PushDevicesError>;
    /** A device whose registering session ended is as good as gone, so it is not found. */
    readonly get: (input: {
      readonly deviceId: PushDeviceId;
      readonly now: string;
    }) => Effect.Effect<Option.Option<PushDeviceRecord>, PushDevicesError>;
    /** Every device whose registering session can still sign in. */
    readonly list: (
      now: string,
    ) => Effect.Effect<ReadonlyArray<PushDeviceRecord>, PushDevicesError>;
    /** Deletes the devices `list` would never return again. */
    readonly prune: (now: string) => Effect.Effect<void, PushDevicesError>;
  }
>()("onicode/push/PushDevices") {}

const GRANT_COLUMNS = {
  notification: "notification_grant",
  liveActivityStart: "live_activity_start_grant",
  liveActivityUpdate: "live_activity_update_grant",
} as const satisfies Record<PushDeviceGrantKind, string>;

const sqlError = (operation: string) => (cause: unknown) =>
  Schema.isSchemaError(cause)
    ? PersistenceDecodeError.fromSchemaError(operation, cause)
    : new PersistenceSqlError({ operation, cause });

const make = Effect.gen(function* () {
  const sql = yield* SqlClient.SqlClient;

  const columns = sql`
    device_id AS "deviceId",
    session_id AS "sessionId",
    platform AS "platform",
    label AS "label",
    gateway_url AS "gatewayUrl",
    preferences_json AS "preferences",
    notification_grant AS "notificationGrant",
    notification_grant_expires_at AS "notificationGrantExpiresAt",
    live_activity_start_grant AS "liveActivityStartGrant",
    live_activity_start_grant_expires_at AS "liveActivityStartGrantExpiresAt",
    live_activity_update_grant AS "liveActivityUpdateGrant",
    live_activity_update_grant_expires_at AS "liveActivityUpdateGrantExpiresAt",
    live_activity_started_at AS "liveActivityStartedAt",
    registered_at AS "registeredAt",
    last_delivered_at AS "lastDeliveredAt",
    last_failure AS "lastFailure"
  `;

  const insert = SqlSchema.void({
    Request: Registration,
    execute: (row) => sql`
      INSERT INTO push_devices (
        device_id,
        session_id,
        platform,
        label,
        gateway_url,
        preferences_json,
        notification_grant,
        notification_grant_expires_at,
        live_activity_start_grant,
        live_activity_start_grant_expires_at,
        registered_at
      )
      VALUES (
        ${row.deviceId},
        ${row.sessionId},
        ${row.platform},
        ${row.label},
        ${row.gatewayUrl},
        ${row.preferences},
        ${row.notificationGrant},
        ${row.notificationGrantExpiresAt},
        ${row.liveActivityStartGrant},
        ${row.liveActivityStartGrantExpiresAt},
        ${row.registeredAt}
      )
      ON CONFLICT (device_id) DO UPDATE SET
        session_id = excluded.session_id,
        platform = excluded.platform,
        label = excluded.label,
        preferences_json = excluded.preferences_json,
        notification_grant = excluded.notification_grant,
        notification_grant_expires_at = excluded.notification_grant_expires_at,
        live_activity_start_grant = excluded.live_activity_start_grant,
        live_activity_start_grant_expires_at = excluded.live_activity_start_grant_expires_at,
        live_activity_update_grant = CASE
          WHEN push_devices.gateway_url = excluded.gateway_url
          THEN push_devices.live_activity_update_grant
        END,
        live_activity_update_grant_expires_at = CASE
          WHEN push_devices.gateway_url = excluded.gateway_url
          THEN push_devices.live_activity_update_grant_expires_at
        END,
        live_activity_started_at = CASE
          WHEN push_devices.gateway_url = excluded.gateway_url
          THEN push_devices.live_activity_started_at
        END,
        gateway_url = excluded.gateway_url,
        registered_at = excluded.registered_at,
        last_failure = NULL
    `,
  });

  const sessionIsLive = (now: string) => sql`
    EXISTS (
      SELECT 1
      FROM auth_sessions
      WHERE auth_sessions.session_id = push_devices.session_id
        AND auth_sessions.revoked_at IS NULL
        AND auth_sessions.expires_at > ${now}
    )
  `;

  const findOne = SqlSchema.findOneOption({
    Request: Schema.Struct({ deviceId: PushDeviceId, now: Schema.String }),
    Result: PushDeviceRecord,
    execute: ({ deviceId, now }) => sql`
      SELECT ${columns}
      FROM push_devices
      WHERE device_id = ${deviceId} AND ${sessionIsLive(now)}
    `,
  });

  const findLive = SqlSchema.findAll({
    Request: Schema.String,
    Result: PushDeviceRecord,
    execute: (now) => sql`
      SELECT ${columns}
      FROM push_devices
      WHERE ${sessionIsLive(now)}
      ORDER BY registered_at, device_id
    `,
  });

  return PushDevices.of({
    register: Effect.fn("PushDevices.register")(function (input) {
      return insert(input).pipe(Effect.mapError(sqlError("registerPushDevice")));
    }),

    setGrant: Effect.fn("PushDevices.setGrant")(function ({ deviceId, kind, grant }) {
      const column = sql(GRANT_COLUMNS[kind]);
      const expiry = sql(`${GRANT_COLUMNS[kind]}_expires_at`);
      return sql<{ readonly device_id: string }>`
        UPDATE push_devices
        SET ${column} = ${grant?.grant ?? null}, ${expiry} = ${grant?.expiresAt ?? null}
        WHERE device_id = ${deviceId}
        RETURNING device_id
      `.pipe(
        Effect.map((rows) => rows.length > 0),
        Effect.mapError(sqlError("setPushDeviceGrant")),
      );
    }),

    setLiveActivityStartedAt: Effect.fn("PushDevices.setLiveActivityStartedAt")(function (input) {
      return sql`
        UPDATE push_devices
        SET live_activity_started_at = ${input.at}
        WHERE device_id = ${input.deviceId}
      `.pipe(Effect.asVoid, Effect.mapError(sqlError("setPushLiveActivityStartedAt")));
    }),

    recordDelivery: Effect.fn("PushDevices.recordDelivery")(function ({ deviceId, at, failure }) {
      return sql`
        UPDATE push_devices
        SET
          last_delivered_at = CASE WHEN ${failure} IS NULL THEN ${at} ELSE last_delivered_at END,
          last_failure = ${failure}
        WHERE device_id = ${deviceId}
      `.pipe(Effect.asVoid, Effect.mapError(sqlError("recordPushDelivery")));
    }),

    remove: Effect.fn("PushDevices.remove")(function (deviceId) {
      return sql`DELETE FROM push_devices WHERE device_id = ${deviceId}`.pipe(
        Effect.asVoid,
        Effect.mapError(sqlError("removePushDevice")),
      );
    }),

    get: Effect.fn("PushDevices.get")(function (input) {
      return findOne(input).pipe(
        NodeSqliteClient.readOnly,
        Effect.mapError(sqlError("getPushDevice")),
      );
    }),

    list: Effect.fn("PushDevices.list")(function (now) {
      return findLive(now).pipe(
        NodeSqliteClient.readOnly,
        Effect.mapError(sqlError("listPushDevices")),
      );
    }),

    prune: Effect.fn("PushDevices.prune")(function (now) {
      return sql`DELETE FROM push_devices WHERE NOT ${sessionIsLive(now)}`.pipe(
        Effect.asVoid,
        Effect.mapError(sqlError("prunePushDevices")),
      );
    }),
  });
});

export const layer = Layer.effect(PushDevices, make);
