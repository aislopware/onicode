import { assert, it } from "@effect/vitest";
import * as NodeSqliteClient from "@onicode/shared/nodeSqliteClient";
import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

import { runMigrations } from "../Migrations.ts";

const layer = it.layer(NodeSqliteClient.layer({ filename: ":memory:" }));
const at = "2026-11-01T00:00:00.000Z";
const share = { id: "share-1", host: "node.ts.net", createdAt: at, expiresAt: at, messages: [] };

layer("082_RemoveGuestShares", (it) => {
  it.effect("replays guest share events as metadata updates and drops stored shares", () =>
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      yield* runMigrations({ toMigrationInclusive: 81 });
      const insertEvent = (version: number, eventType: string, payload: object) => sql`
        INSERT INTO orchestration_events (
          event_id, aggregate_kind, stream_id, stream_version, event_type, occurred_at,
          command_id, actor_kind, payload_json, metadata_json
        ) VALUES (
          ${`event-${version}`}, 'thread', 'thread-1', ${version}, ${eventType}, ${at},
          ${`command-${version}`}, 'client', ${JSON.stringify(payload)}, '{}'
        )
      `;
      yield* insertEvent(1, "thread.metadata-updated", { id: "thread-1", title: "Before" });
      yield* insertEvent(2, "thread.guest-share-updated", {
        id: "thread-1",
        title: "Shared",
        guestShare: share,
      });
      for (const [id, payload] of [
        ["shared", { id: "shared", guestShare: share }],
        ["plain", { id: "plain" }],
      ] as const) {
        yield* sql`
          INSERT INTO orchestration_v2_projection_threads (
            thread_id, project_id, title, default_provider, runtime_mode, interaction_mode,
            created_at, updated_at, payload_json
          ) VALUES (
            ${id}, 'project-1', ${id}, 'codex', 'full-access', 'default', ${at}, ${at},
            ${JSON.stringify(payload)}
          )
        `;
      }

      yield* runMigrations({ toMigrationInclusive: 82 });

      const events = yield* sql<{ readonly event_type: string; readonly payload_json: string }>`
        SELECT event_type, payload_json FROM orchestration_events ORDER BY sequence
      `;
      assert.deepStrictEqual(
        events.map((event) => [event.event_type, JSON.parse(event.payload_json)]),
        [
          ["thread.metadata-updated", { id: "thread-1", title: "Before" }],
          ["thread.metadata-updated", { id: "thread-1", title: "Shared" }],
        ],
      );
      const threads = yield* sql<{ readonly payload_json: string }>`
        SELECT payload_json FROM orchestration_v2_projection_threads ORDER BY thread_id
      `;
      assert.deepStrictEqual(
        threads.map((row) => JSON.parse(row.payload_json)),
        [{ id: "plain" }, { id: "shared" }],
      );
    }),
  );
});
