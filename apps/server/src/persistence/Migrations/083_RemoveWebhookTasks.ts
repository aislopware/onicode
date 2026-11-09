import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

// Webhook tasks are gone. Their tasks can no longer run, and the columns held each hook's token
// and signing secret, so both leave the database rather than lingering unused.
export default Effect.gen(function* () {
  const sql = yield* SqlClient.SqlClient;

  yield* sql`DELETE FROM scheduled_tasks WHERE json_extract(schedule_json, '$.type') = 'webhook'`;
  yield* sql`DROP INDEX IF EXISTS idx_webhook_pending_due`;
  yield* sql`DROP INDEX IF EXISTS idx_webhook_pending_key`;
  yield* sql`DROP TABLE IF EXISTS scheduled_task_webhook_pending`;
  yield* sql`DROP INDEX IF EXISTS idx_webhook_replay_expiry`;
  yield* sql`DROP INDEX IF EXISTS idx_webhook_replay_delivery`;
  yield* sql`DROP TABLE IF EXISTS scheduled_task_webhook_replay_keys`;
  yield* sql`DROP INDEX IF EXISTS idx_scheduled_task_webhook_deliveries_task`;
  yield* sql`DROP TABLE IF EXISTS scheduled_task_webhook_deliveries`;
  yield* sql`ALTER TABLE scheduled_tasks DROP COLUMN webhook_token`;
  yield* sql`ALTER TABLE scheduled_tasks DROP COLUMN webhook_secret`;
  yield* sql`ALTER TABLE scheduled_tasks DROP COLUMN last_run_webhook_delivery_id`;
});
