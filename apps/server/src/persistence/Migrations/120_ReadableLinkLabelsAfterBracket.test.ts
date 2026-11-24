import { assert, it } from "@effect/vitest";
import * as NodeSqliteClient from "@onicode/shared/nodeSqliteClient";
import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

import { runMigrations } from "../Migrations.ts";

const createdAt = "2026-11-01T00:00:00.000Z";

it.layer(NodeSqliteClient.layer({ filename: ":memory:" }))(
  "120_ReadableLinkLabelsAfterBracket",
  (it) => {
    it.effect("re-indexes a file mention that follows an unclosed bracket", () =>
      Effect.gen(function* () {
        const sql = yield* SqlClient.SqlClient;
        yield* runMigrations({ toMigrationInclusive: 119 });
        const text = String.raw`Compare [ this with [a\[1\].ts](a%5B1%5D.ts)`;
        // Earlier builds read the label from the unclosed `[`, kept its escapes, and stored nothing.
        yield* sql`
          INSERT INTO orchestration_v2_projection_messages (
            message_id, thread_id, role, streaming, created_at, updated_at, payload_json, search_text
          ) VALUES (
            'after-bracket', 'thread:routes', 'user', 0, ${createdAt}, ${createdAt},
            ${JSON.stringify({ text })}, NULL
          )
        `;

        assert.deepEqual(yield* runMigrations({ toMigrationInclusive: 120 }), [
          [120, "ReadableLinkLabelsAfterBracket"],
        ]);
        const rows = yield* sql<{ readonly search_text: string | null }>`
          SELECT search_text FROM orchestration_v2_projection_messages
        `;
        assert.equal(rows[0]?.search_text, "Compare [ this with [a[1].ts](a%5B1%5D.ts)");
        const indexed = yield* sql<{ readonly message_id: string }>`
          SELECT messages.message_id
          FROM orchestration_v2_message_search AS search
          INNER JOIN orchestration_v2_projection_messages AS messages
            ON messages.rowid = search.rowid
          WHERE orchestration_v2_message_search MATCH '"a[1].ts"'
        `;
        assert.deepEqual(
          indexed.map((row) => row.message_id),
          ["after-bracket"],
        );
      }),
    );
  },
);
