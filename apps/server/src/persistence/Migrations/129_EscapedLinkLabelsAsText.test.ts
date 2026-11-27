import { assert, it } from "@effect/vitest";
import * as NodeSqliteClient from "@onicode/shared/nodeSqliteClient";
import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

import { runMigrations } from "../Migrations.ts";

const createdAt = "2026-11-01T00:00:00.000Z";

it.layer(NodeSqliteClient.layer({ filename: ":memory:" }))("129_EscapedLinkLabelsAsText", (it) => {
  it.effect("re-indexes a file mention whose label nests or leaves open a bracket", () =>
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      yield* runMigrations({ toMigrationInclusive: 128 });
      const text = String.raw`Open [\[\[...slug\]\].tsx](app/%5B%5B...slug%5D%5D.tsx) and [foo\[bar.md](foo%5Bbar.md)`;
      // Earlier builds kept those brackets escaped, so the text read as stored.
      yield* sql`
        INSERT INTO orchestration_v2_projection_messages (
          message_id, thread_id, role, streaming, created_at, updated_at, payload_json, search_text
        ) VALUES (
          'nested', 'thread:routes', 'user', 0, ${createdAt}, ${createdAt},
          ${JSON.stringify({ text })}, NULL
        )
      `;

      assert.deepEqual(yield* runMigrations({ toMigrationInclusive: 129 }), [
        [129, "EscapedLinkLabelsAsText"],
      ]);
      const rows = yield* sql<{ readonly search_text: string | null }>`
        SELECT search_text FROM orchestration_v2_projection_messages
      `;
      assert.equal(rows[0]?.search_text, "Open [[...slug]].tsx and foo[bar.md");
      for (const phrase of ['"[[...slug]].tsx"', '"foo[bar.md"']) {
        const indexed = yield* sql<{ readonly message_id: string }>`
          SELECT messages.message_id
          FROM orchestration_v2_message_search AS search
          INNER JOIN orchestration_v2_projection_messages AS messages
            ON messages.rowid = search.rowid
          WHERE orchestration_v2_message_search MATCH ${phrase}
        `;
        assert.deepEqual(
          indexed.map((row) => row.message_id),
          ["nested"],
        );
      }
    }),
  );
});
