import {
  IssueLinkedThreadsResult,
  IssueOperationError,
  threadIssueKeysEqual,
  type IssueLinkedThreadsInput,
  type ThreadIssueKey,
} from "@onicode/contracts";
import * as NodeSqliteClient from "@onicode/shared/nodeSqliteClient";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import * as SqlClient from "effect/sql/SqlClient";

const decodeLinkedThreads = Schema.decodeUnknownEffect(IssueLinkedThreadsResult);

/**
 * The threads an issue is linked to; the rows and filters `linkedThreads` uses for pull requests.
 * `workspaceOf` reads the workspace an issue URL names, for trackers whose keys repeat across
 * workspaces. A link's stored workspace wins over its URL's, which may name a renamed one's old
 * key; a link naming none is kept.
 */
export const listLinkedIssueThreads = Effect.fn("listLinkedIssueThreads")(
  function* (
    issue: IssueLinkedThreadsInput,
    workspaceOf: (url: string) => string | null = () => null,
  ) {
    const sql = yield* SqlClient.SqlClient;
    // The CASE stops SQLite hoisting the side-chat check out of the link loop,
    // where it would re-parse every thread's payload, not just matching ones.
    const rows = yield* sql<{
      id: string;
      projectId: string;
      title: string;
      archivedAt: string | null;
      settled: number;
      tracker: ThreadIssueKey["tracker"];
      host: string;
      scope: string;
      key: string;
      url: string | null;
      workspace: string | null;
    }>`
      SELECT t.thread_id AS id, t.project_id AS "projectId", t.title,
        t.archived_at AS "archivedAt",
        (json_extract(t.payload_json, '$.settledAt') IS NOT NULL
          OR json_extract(t.payload_json, '$.settledOverride') IS 'settled') AS settled,
        json_extract(link.value, '$.tracker') AS tracker,
        json_extract(link.value, '$.host') AS host,
        json_extract(link.value, '$.scope') AS scope,
        json_extract(link.value, '$.key') AS key,
        json_extract(link.value, '$.url') AS url,
        json_extract(link.value, '$.workspace') AS workspace,
        t.updated_at AS "updatedAt"
      FROM orchestration_v2_projection_threads AS t
      JOIN json_each(t.payload_json, '$.issues') AS link
      WHERE t.deleted_at IS NULL
        AND CASE WHEN lower(json_extract(link.value, '$.key')) = ${issue.key.trim().toLowerCase()}
          THEN json_extract(t.payload_json, '$.lineage.sideChat') IS NOT 1
          ELSE 0
        END
      ORDER BY "updatedAt" DESC, id ASC
    `.pipe(NodeSqliteClient.readOnly);
    const workspace =
      issue.workspace?.toLowerCase() ?? (issue.url === undefined ? null : workspaceOf(issue.url));
    const sameWorkspace = (row: { url: string | null; workspace: string | null }) => {
      const linked = row.workspace ?? (row.url === null ? null : workspaceOf(row.url));
      return workspace === null || linked === null || linked.toLowerCase() === workspace;
    };
    // A thread may hold one issue twice, spelled `ENG-7` and `eng-7` before keys compared alike.
    const listed = new Set<string>();
    const threads = [];
    for (const row of rows) {
      if (
        listed.has(row.id) ||
        !threadIssueKeysEqual({ ...row, workspace: undefined }, issue) ||
        !sameWorkspace(row)
      ) {
        continue;
      }
      listed.add(row.id);
      const { id, projectId, title, archivedAt, settled } = row;
      threads.push({ id, projectId, title, archivedAt, settled: settled === 1 });
    }
    return yield* decodeLinkedThreads({ threads });
  },
  Effect.mapError(
    (cause) =>
      new IssueOperationError({
        operation: "linkedThreads",
        detail: "Could not load linked threads.",
        cause,
      }),
  ),
);
