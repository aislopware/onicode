import { assert, it } from "@effect/vitest";
import * as NodeSqliteClient from "@onicode/shared/nodeSqliteClient";
import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

import { runMigrations } from "../Migrations.ts";

const layer = it.layer(NodeSqliteClient.layer({ filename: ":memory:" }));
const at = "2026-11-01T00:00:00.000Z";

layer("083_RemoveWebhookTasks", (it) => {
  it.effect("deletes webhook tasks and their storage, keeping other tasks", () =>
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      yield* runMigrations({ toMigrationInclusive: 82 });
      for (const [id, schedule] of [
        ["hook", { type: "webhook" }],
        ["nightly", { type: "interval", everyMs: 86_400_000 }],
      ] as const) {
        yield* sql`
          INSERT INTO scheduled_tasks (
            task_id, title, prompt, enabled, schedule_json, project_id, workspace_strategy_json,
            model_selection_json, runtime_mode, interaction_mode, created_by, creation_source,
            created_at, updated_at, last_run_status, run_count, webhook_token, webhook_secret
          ) VALUES (
            ${id}, ${id}, 'Run it', 1, ${JSON.stringify(schedule)}, 'project-1', '{}', '{}',
            'full-access', 'default', 'user', 'user', ${at}, ${at}, 'never', 0,
            ${id === "hook" ? "token" : null}, ${id === "hook" ? "secret" : null}
          )
        `;
      }

      yield* runMigrations({ toMigrationInclusive: 83 });

      const tasks = yield* sql<{ readonly task_id: string }>`SELECT task_id FROM scheduled_tasks`;
      assert.deepStrictEqual(
        tasks.map((task) => task.task_id),
        ["nightly"],
      );
      const columns = yield* sql<{ readonly name: string }>`
        SELECT name FROM pragma_table_info('scheduled_tasks') WHERE name LIKE '%webhook%'
      `;
      assert.deepStrictEqual(columns, []);
      const tables = yield* sql<{ readonly name: string }>`
        SELECT name FROM sqlite_master WHERE name LIKE '%webhook%'
      `;
      assert.deepStrictEqual(tables, []);
    }),
  );
});
