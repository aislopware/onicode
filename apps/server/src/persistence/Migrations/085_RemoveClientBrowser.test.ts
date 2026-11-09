import { assert, it } from "@effect/vitest";
import * as NodeSqliteClient from "@onicode/shared/nodeSqliteClient";
import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

import { runMigrations } from "../Migrations.ts";

const layer = it.layer(NodeSqliteClient.layer({ filename: ":memory:" }));
const at = "2026-11-01T00:00:00.000Z";

layer("085_RemoveClientBrowser", (it) => {
  it.effect("drops the browser family and keeps the session", () =>
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      yield* runMigrations({ toMigrationInclusive: 84 });
      yield* sql`
        INSERT INTO auth_sessions (session_id, subject, scopes, client_browser, issued_at, expires_at)
        VALUES ('session', 'one-time-token', '[]', 'Chrome', ${at}, ${at})
      `;

      yield* runMigrations({ toMigrationInclusive: 85 });

      const sessions = yield* sql<{ readonly session_id: string }>`
        SELECT session_id FROM auth_sessions
      `;
      assert.deepStrictEqual(
        sessions.map((session) => session.session_id),
        ["session"],
      );
      const columns = yield* sql<{ readonly name: string }>`
        SELECT name FROM pragma_table_info('auth_sessions') WHERE name = 'client_browser'
      `;
      assert.deepStrictEqual(columns, []);
    }),
  );
});
