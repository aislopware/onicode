import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

export default Effect.gen(function* () {
  const sql = yield* SqlClient.SqlClient;
  const columns = yield* sql<{ readonly name: string }>`PRAGMA table_info(push_devices)`;
  const has = (name: string) => columns.some((column) => column.name === name);
  // The flag recorded no time, so a device it held back starts free to show an activity again.
  if (!has("live_activity_started_at")) {
    yield* sql`ALTER TABLE push_devices ADD COLUMN live_activity_started_at INTEGER`;
  }
  if (has("live_activity_settled")) {
    yield* sql`ALTER TABLE push_devices DROP COLUMN live_activity_settled`;
  }
});
