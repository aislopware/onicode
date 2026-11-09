import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

// Sessions came from browsers only while Onicode served a web app; the desktop and mobile apps
// never had a browser family worth recording.
export default Effect.gen(function* () {
  const sql = yield* SqlClient.SqlClient;
  const columns = yield* sql<{ readonly name: string }>`
    SELECT name FROM pragma_table_info('auth_sessions') WHERE name = 'client_browser'
  `;
  if (columns.length > 0) {
    yield* sql`ALTER TABLE auth_sessions DROP COLUMN client_browser`;
  }
});
