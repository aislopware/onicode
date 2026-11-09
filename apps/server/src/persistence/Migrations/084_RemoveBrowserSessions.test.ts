import { assert, it } from "@effect/vitest";
import * as NodeSqliteClient from "@onicode/shared/nodeSqliteClient";
import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

import { runMigrations } from "../Migrations.ts";

const layer = it.layer(NodeSqliteClient.layer({ filename: ":memory:" }));
const at = "2026-11-01T00:00:00.000Z";

layer("084_RemoveBrowserSessions", (it) => {
  it.effect("deletes browser cookie sessions, keeps bearer sessions, and drops the method", () =>
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      yield* runMigrations({ toMigrationInclusive: 83 });
      for (const [id, method] of [
        ["cookie", "browser-session-cookie"],
        ["bearer", "bearer-access-token"],
      ] as const) {
        yield* sql`
          INSERT INTO auth_sessions (session_id, subject, scopes, method, issued_at, expires_at)
          VALUES (${id}, 'one-time-token', '[]', ${method}, ${at}, ${at})
        `;
      }

      yield* runMigrations({ toMigrationInclusive: 84 });

      const sessions = yield* sql<{ readonly session_id: string }>`
        SELECT session_id FROM auth_sessions
      `;
      assert.deepStrictEqual(
        sessions.map((session) => session.session_id),
        ["bearer"],
      );
      const columns = yield* sql<{ readonly name: string }>`
        SELECT name FROM pragma_table_info('auth_sessions') WHERE name = 'method'
      `;
      assert.deepStrictEqual(columns, []);
    }),
  );
});
