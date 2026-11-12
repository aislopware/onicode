import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

export default Effect.gen(function* () {
  const sql = yield* SqlClient.SqlClient;

  // Grants are gateway-sealed push tokens; the tokens themselves never reach this database.
  yield* sql`
    CREATE TABLE IF NOT EXISTS push_devices (
      device_id TEXT PRIMARY KEY,
      session_id TEXT NOT NULL,
      platform TEXT NOT NULL,
      label TEXT NOT NULL,
      gateway_url TEXT NOT NULL,
      preferences_json TEXT NOT NULL,
      notification_grant TEXT,
      notification_grant_expires_at INTEGER,
      live_activity_start_grant TEXT,
      live_activity_start_grant_expires_at INTEGER,
      live_activity_update_grant TEXT,
      live_activity_update_grant_expires_at INTEGER,
      live_activity_settled INTEGER NOT NULL DEFAULT 0,
      registered_at TEXT NOT NULL,
      last_delivered_at TEXT,
      last_failure TEXT
    ) WITHOUT ROWID
  `;
});
