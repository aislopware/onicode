import { assert, it } from "@effect/vitest";
import * as NodeSqliteClient from "@onicode/shared/nodeSqliteClient";
import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

import { runMigrations } from "../Migrations.ts";

const at = "2026-11-20T00:00:00.000Z";

it.layer(NodeSqliteClient.layer({ filename: ":memory:" }))("122_WorkflowSkippedRuns", (it) => {
  it.effect("clears input_json only on pre-existing top-level skipped runs and indexes them", () =>
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      yield* runMigrations({ toMigrationInclusive: 121 });
      const rows = [
        {
          runId: "top-skipped",
          status: "skipped",
          parentRunId: null,
          inputJson: '{"event":"ping"}',
        },
        {
          runId: "child-skipped",
          status: "skipped",
          parentRunId: "completed",
          inputJson: '{"child":1}',
        },
        {
          runId: "completed",
          status: "completed",
          parentRunId: null,
          inputJson: '{"done":true}',
        },
      ] as const;
      for (const row of rows) {
        yield* sql`
          INSERT INTO workflow_runs (
            run_id, workflow, version, engine_tag, idempotency_key, title, status,
            started_at, updated_at, ended_at, input_json, parent_run_id
          ) VALUES (
            ${row.runId}, 'intake', 1, 'intake@1', ${row.runId}, ${row.runId}, ${row.status},
            ${at}, ${at}, ${at}, ${row.inputJson}, ${row.parentRunId}
          )
        `;
      }

      assert.deepEqual(yield* runMigrations({ toMigrationInclusive: 122 }), [
        [122, "WorkflowSkippedRuns"],
      ]);

      const after = yield* sql<{ readonly run_id: string; readonly input_json: string | null }>`
        SELECT run_id, input_json FROM workflow_runs ORDER BY run_id ASC
      `;
      assert.deepEqual(after, [
        { run_id: "child-skipped", input_json: '{"child":1}' },
        { run_id: "completed", input_json: '{"done":true}' },
        { run_id: "top-skipped", input_json: null },
      ]);

      const plan = yield* sql<{ readonly detail: string }>`
        EXPLAIN QUERY PLAN
        SELECT run_id FROM workflow_runs
        WHERE workflow = 'intake' AND status = 'skipped' AND parent_run_id IS NULL
        ORDER BY started_at DESC
      `;
      assert.isTrue(plan.some((step) => step.detail.includes("workflow_runs_skipped")));
    }),
  );
});
