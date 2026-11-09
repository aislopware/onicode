import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

// Guest links are gone. Their events carried the whole thread, so they replay as metadata updates
// and the event sequence stays gapless; the stored share leaves every thread payload.
export default Effect.gen(function* () {
  const sql = yield* SqlClient.SqlClient;

  yield* sql`
    UPDATE orchestration_events
    SET event_type = 'thread.metadata-updated',
      payload_json = json_remove(payload_json, '$.guestShare')
    WHERE event_type = 'thread.guest-share-updated'
  `;
  yield* sql`
    UPDATE orchestration_v2_projection_threads
    SET payload_json = json_remove(payload_json, '$.guestShare')
    WHERE json_type(payload_json, '$.guestShare') IS NOT NULL
  `;
});
