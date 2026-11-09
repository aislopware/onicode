import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

// Browser cookie sessions are gone. Their cookies can no longer authenticate, and every remaining
// session is a bearer token, so the column that told the two apart goes too.
export default Effect.gen(function* () {
  const sql = yield* SqlClient.SqlClient;

  yield* sql`DELETE FROM auth_sessions WHERE method <> 'bearer-access-token'`;
  yield* sql`ALTER TABLE auth_sessions DROP COLUMN method`;
});
