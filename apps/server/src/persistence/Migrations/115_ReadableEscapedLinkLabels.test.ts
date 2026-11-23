import { assert, it } from "@effect/vitest";
import * as NodeSqliteClient from "@onicode/shared/nodeSqliteClient";
import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

import { runMigrations } from "../Migrations.ts";

const createdAt = "2026-11-01T00:00:00.000Z";
const indexed = (sql: SqlClient.SqlClient, phrase: string) =>
  sql<{ readonly message_id: string }>`
    SELECT messages.message_id
    FROM orchestration_v2_message_search AS search
    INNER JOIN orchestration_v2_projection_messages AS messages
      ON messages.rowid = search.rowid
    WHERE orchestration_v2_message_search MATCH ${phrase}
  `.pipe(Effect.map((rows) => rows.map((row) => row.message_id)));

it.layer(NodeSqliteClient.layer({ filename: ":memory:" }))(
  "115_ReadableEscapedLinkLabels",
  (it) => {
    it.effect("re-indexes file mentions whose labels escape brackets as they read", () =>
      Effect.gen(function* () {
        const sql = yield* SqlClient.SqlClient;
        yield* runMigrations({ toMigrationInclusive: 114 });
        const insertMessage = (messageId: string, text: string, searchText: string | null) => sql`
        INSERT INTO orchestration_v2_projection_messages (
          message_id, thread_id, role, streaming, created_at, updated_at, payload_json, search_text
        ) VALUES (
          ${messageId}, 'thread:routes', 'user', 0, ${createdAt}, ${createdAt},
          ${JSON.stringify({ text })}, ${searchText}
        )
      `;
        const route = String.raw`Open [\[id\]\_edit.tsx](app/routes/%5Bid%5D_edit.tsx)`;
        // What earlier builds stored: an escaped bracket kept the label's other escapes too.
        yield* insertMessage("route", route, null);
        yield* insertMessage("plain", "Open the [docs](https://example.com)", null);
        yield* insertMessage("unbalanced", String.raw`See [a\]b.ts](a%5Db.ts)`, null);
        yield* insertMessage(
          "issue",
          String.raw`Fixed in [Issue \#42](https://example.com/42)`,
          null,
        );
        // A row that already reads differently, from a quote's link, is rewritten whole.
        yield* insertMessage(
          "quoted",
          String.raw`[Assistant quote](oni-citation://v1/not-a-citation) and [\[id\].tsx](%5Bid%5D.tsx)`,
          String.raw`Assistant quote and [\[id\].tsx](%5Bid%5D.tsx)`,
        );
        assert.deepEqual(yield* indexed(sql, '"[id]_edit.tsx"'), []);

        assert.deepEqual(yield* runMigrations({ toMigrationInclusive: 115 }), [
          [115, "ReadableEscapedLinkLabels"],
        ]);
        const rows = yield* sql<{
          readonly message_id: string;
          readonly search_text: string | null;
        }>`SELECT message_id, search_text FROM orchestration_v2_projection_messages`;
        const searchText = new Map(rows.map((row) => [row.message_id, row.search_text]));
        assert.equal(searchText.get("route"), "Open [[id]_edit.tsx](app/routes/%5Bid%5D_edit.tsx)");
        assert.isNull(searchText.get("plain"));
        // An unbalanced bracket stays escaped, so the label still ends at its own `](`.
        assert.isNull(searchText.get("unbalanced"));
        assert.equal(searchText.get("issue"), "Fixed in [Issue #42](https://example.com/42)");
        assert.equal(
          searchText.get("quoted"),
          "[Assistant quote](oni-citation://v1/not-a-citation) and [[id].tsx](%5Bid%5D.tsx)",
        );
        assert.deepEqual(yield* indexed(sql, '"[id]_edit.tsx"'), ["route"]);
      }),
    );
  },
);
