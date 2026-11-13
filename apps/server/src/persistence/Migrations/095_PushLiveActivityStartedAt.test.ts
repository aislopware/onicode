import { assert, it } from "@effect/vitest";
import * as NodeSqliteClient from "@onicode/shared/nodeSqliteClient";
import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

import { runMigrations } from "../Migrations.ts";

const layer = it.layer(NodeSqliteClient.layer({ filename: ":memory:" }));

layer("095_PushLiveActivityStartedAt", (it) => {
  it.effect("frees a device the old flag held back from a new Live Activity", () =>
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      yield* runMigrations({ toMigrationInclusive: 94 });
      yield* sql`
        INSERT INTO push_devices
          (device_id, session_id, platform, label, gateway_url, preferences_json,
           live_activity_start_grant, live_activity_settled, registered_at)
        VALUES ('phone', 'session', 'ios', 'iPhone', 'https://push.example', '{}',
          'grant_start', 1, '2026-11-13')
      `;
      yield* runMigrations();
      assert.deepStrictEqual(yield* sql`SELECT * FROM push_devices`, [
        {
          device_id: "phone",
          session_id: "session",
          platform: "ios",
          label: "iPhone",
          gateway_url: "https://push.example",
          preferences_json: "{}",
          notification_grant: null,
          notification_grant_expires_at: null,
          live_activity_start_grant: "grant_start",
          live_activity_start_grant_expires_at: null,
          live_activity_update_grant: null,
          live_activity_update_grant_expires_at: null,
          registered_at: "2026-11-13",
          last_delivered_at: null,
          last_failure: null,
          live_activity_started_at: null,
        },
      ]);
    }),
  );
});
