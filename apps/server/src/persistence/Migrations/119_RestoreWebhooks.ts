import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

/** The secret half of each workflow hook's URL. */
export const workflowHooks = Effect.gen(function* () {
  const sql = yield* SqlClient.SqlClient;
  yield* sql`
    CREATE TABLE IF NOT EXISTS workflow_hooks (
      hook_id TEXT PRIMARY KEY,
      token TEXT NOT NULL
    )
  `;
});

// Webhooks came back after 083 dropped them: this recreates the 057 and 067 schema for scheduled
// tasks, plus the hook secrets workflows keep.
export default Effect.gen(function* () {
  const sql = yield* SqlClient.SqlClient;

  // Kept out of schedule_json so they never decode into the read model.
  yield* sql`ALTER TABLE scheduled_tasks ADD COLUMN webhook_token TEXT`;
  yield* sql`ALTER TABLE scheduled_tasks ADD COLUMN webhook_secret TEXT`;
  yield* sql`ALTER TABLE scheduled_tasks ADD COLUMN last_run_webhook_delivery_id TEXT`;

  yield* sql`
    CREATE TABLE IF NOT EXISTS scheduled_task_webhook_deliveries (
      delivery_id TEXT PRIMARY KEY,
      task_id TEXT NOT NULL,
      received_at TEXT NOT NULL,
      method TEXT NOT NULL,
      query TEXT NOT NULL,
      headers_json TEXT NOT NULL,
      body TEXT NOT NULL,
      body_bytes INTEGER NOT NULL,
      body_truncated INTEGER NOT NULL,
      outcome TEXT NOT NULL,
      signature_verified INTEGER NOT NULL,
      missing_fields_json TEXT NOT NULL,
      rendered_prompt TEXT,
      error TEXT
    )
  `;
  yield* sql`
    CREATE INDEX IF NOT EXISTS idx_scheduled_task_webhook_deliveries_task
    ON scheduled_task_webhook_deliveries(task_id, received_at)
  `;

  yield* sql`CREATE TABLE IF NOT EXISTS scheduled_task_webhook_replay_keys (
    task_id TEXT NOT NULL REFERENCES scheduled_tasks(task_id) ON DELETE CASCADE,
    replay_key TEXT NOT NULL,
    delivery_id TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (task_id, replay_key)
  )`;
  yield* sql`CREATE INDEX IF NOT EXISTS idx_webhook_replay_expiry
    ON scheduled_task_webhook_replay_keys(expires_at)`;
  yield* sql`CREATE INDEX IF NOT EXISTS idx_webhook_replay_delivery
    ON scheduled_task_webhook_replay_keys(delivery_id)`;
  yield* sql`CREATE TABLE IF NOT EXISTS scheduled_task_webhook_pending (
    delivery_id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES scheduled_tasks(task_id) ON DELETE CASCADE,
    coalesce_key TEXT NOT NULL,
    due_at INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    task_json TEXT NOT NULL,
    request_json TEXT NOT NULL,
    delivery_ids_json TEXT NOT NULL
  )`;
  yield* sql`CREATE INDEX IF NOT EXISTS idx_webhook_pending_due
    ON scheduled_task_webhook_pending(state, due_at)`;
  yield* sql`CREATE UNIQUE INDEX IF NOT EXISTS idx_webhook_pending_key
    ON scheduled_task_webhook_pending(task_id, coalesce_key) WHERE state = 'pending'`;

  yield* workflowHooks;
});
