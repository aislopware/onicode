import * as Effect from "effect/Effect";
import * as SqlClient from "effect/sql/SqlClient";

import { readableSearchText } from "../../orchestration-v2/messageSearchIndex.ts";

export default Effect.gen(function* () {
  const sql = yield* SqlClient.SqlClient;
  // search_text kept a link label's escapes other than `*` and `_`, so `[id]_edit.tsx`
  // never matched. The update trigger re-indexes each row whose readable text changes.
  // JSON doubles the backslash, so the raw check skips parsing messages with no escape.
  const pageSize = 500;
  let afterMessageId = "";
  while (true) {
    const rows = yield* sql<{
      readonly message_id: string;
      readonly text: string;
      readonly search_text: string | null;
    }>`
      SELECT message_id, json_extract(payload_json, '$.text') AS text, search_text
      FROM orchestration_v2_projection_messages
      WHERE message_id > ${afterMessageId}
        AND payload_json LIKE '%\\\\%'
        AND payload_json LIKE '%](%'
        AND CASE WHEN json_valid(payload_json) THEN json_type(payload_json, '$.text') END = 'text'
      ORDER BY message_id
      LIMIT ${pageSize}
    `;
    for (const row of rows) {
      const searchText = readableSearchText(row.text);
      if (searchText === row.search_text) continue;
      yield* sql`
        UPDATE orchestration_v2_projection_messages
        SET search_text = ${searchText}
        WHERE message_id = ${row.message_id}
      `;
    }
    if (rows.length < pageSize) break;
    afterMessageId = rows.at(-1)!.message_id;
  }
});
