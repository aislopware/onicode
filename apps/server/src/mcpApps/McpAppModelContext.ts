import type { RunId, ThreadId, TurnItemId } from "@onicode/contracts";
import * as Context from "effect/Context";
import * as NodeSqliteClient from "@onicode/shared/nodeSqliteClient";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";
import * as SqlClient from "effect/sql/SqlClient";

export const MCP_APP_MODEL_CONTEXT_MAX_BYTES = 16 * 1024;

export class McpAppModelContextError extends Schema.TaggedError<McpAppModelContextError>()(
  "McpAppModelContextError",
  { cause: Schema.optional(Schema.Defect()) },
) {}

export interface McpAppModelContextEntry {
  readonly itemId: string;
  readonly server: string;
  readonly tool: string;
  readonly text: string;
}

export class McpAppModelContext extends Context.Service<
  McpAppModelContext,
  {
    /** Empty text removes the app's context. */
    readonly set: (input: {
      readonly threadId: ThreadId;
      readonly itemId: TurnItemId;
      readonly server: string;
      readonly tool: string;
      readonly text: string;
    }) => Effect.Effect<void, McpAppModelContextError>;
    /** The notes of the conversation `runId` continues. */
    readonly forThread: (
      threadId: ThreadId,
      runId: RunId,
    ) => Effect.Effect<ReadonlyArray<McpAppModelContextEntry>, McpAppModelContextError>;
  }
>()("onicode/mcpApps/McpAppModelContext") {}

/** The run's notes as a provider turn or steer sends them; a failed read sends none. */
export const forProvider = Effect.fn("McpAppModelContext.forProvider")(function* (
  modelContext: McpAppModelContext["Service"],
  threadId: ThreadId,
  runId: RunId,
) {
  const entries = yield* modelContext
    .forThread(threadId, runId)
    .pipe(
      Effect.catch((cause) =>
        Effect.logWarning("Failed to read MCP app model context.", { cause }).pipe(
          Effect.as([] as ReadonlyArray<McpAppModelContextEntry>),
        ),
      ),
    );
  return entries.map((entry) => ({
    // The item id alone is unique and needs no escaping; server and
    // tool names are free text that would break the tag Codex wraps
    // the context in.
    key: `mcp_app_${entry.itemId.replace(/[^\w.-]/g, "_")}`,
    text: entry.text,
  }));
});

export const layer = Layer.effect(
  McpAppModelContext,
  Effect.gen(function* () {
    const sql = yield* SqlClient.SqlClient;
    const fail = (cause: unknown) => new McpAppModelContextError({ cause });

    const set = Effect.fn("McpAppModelContext.set")(function* (input: {
      readonly threadId: ThreadId;
      readonly itemId: TurnItemId;
      readonly server: string;
      readonly tool: string;
      readonly text: string;
    }) {
      if (input.text.trim() === "") {
        yield* sql`
          DELETE FROM mcp_app_model_context
          WHERE thread_id = ${input.threadId} AND item_id = ${input.itemId}
        `.pipe(Effect.mapError(fail));
        return;
      }
      const updatedAt = DateTime.formatIso(yield* DateTime.now);
      yield* sql`
        INSERT INTO mcp_app_model_context (thread_id, item_id, server, tool, text, updated_at)
        VALUES (${input.threadId}, ${input.itemId}, ${input.server}, ${input.tool}, ${input.text}, ${updatedAt})
        ON CONFLICT(thread_id, item_id) DO UPDATE SET text = excluded.text, updated_at = excluded.updated_at
      `.pipe(Effect.mapError(fail));
    });

    const forThread = Effect.fn("McpAppModelContext.forThread")(function* (
      threadId: ThreadId,
      runId: RunId,
    ) {
      // Match by id across fork lineage; later source rollbacks leave the fork's copy intact,
      // while a started /clear (fresh, not scheduled) run on this thread drops older notes. A
      // scheduled fresh session sees only its own notes, and the main conversation none of them.
      return yield* sql<McpAppModelContextEntry>`
        WITH scheduled AS (
          -- Mirrors scheduledFreshRunOrdinals: a run's first prompt says if a schedule sent it.
          SELECT s.ordinal
          FROM orchestration_v2_projection_runs AS s
          WHERE s.thread_id = ${threadId}
            AND json_extract(s.payload_json, '$.freshContext') = 1
            AND (
              SELECT json_extract(ti.payload_json, '$.scheduledTaskId')
              FROM orchestration_v2_projection_turn_items AS ti
              WHERE ti.run_id = s.run_id AND ti.type = 'user_message'
              ORDER BY ti.ordinal
              LIMIT 1
            ) IS NOT NULL
        ),
        target AS (
          -- Mirrors scheduledConversationStart: a session's provider threads name its first run.
          SELECT (
            SELECT pt.first_run_ordinal
            FROM orchestration_v2_projection_runs AS r
            JOIN orchestration_v2_projection_provider_threads AS pt
              ON pt.provider_thread_id = r.provider_thread_id
            WHERE r.run_id = ${runId}
              AND r.thread_id = ${threadId}
              AND pt.first_run_ordinal IN (SELECT ordinal FROM scheduled)
          ) AS session
        ),
        boundary AS (
          SELECT COALESCE(MAX(b.ordinal), 0) AS min_ordinal
          FROM orchestration_v2_projection_runs AS b
          WHERE b.thread_id = ${threadId}
            AND json_extract(b.payload_json, '$.freshContext') = 1
            AND b.status <> 'queued'
            -- Mirrors startedFreshRun: a run that never started cleared nothing.
            AND NOT (
              b.status IN ('cancelled', 'rolled_back')
              AND json_extract(b.payload_json, '$.startedAt') IS NULL
              AND json_extract(b.payload_json, '$.restartContinuationOfRunId') IS NULL
            )
            -- Mirrors conversationRuns: the run starts afresh even before its session is known.
            AND (b.ordinal NOT IN (SELECT ordinal FROM scheduled) OR b.run_id = ${runId})
        )
        SELECT context.item_id AS "itemId", context.server, context.tool, context.text
        FROM mcp_app_model_context AS context
        CROSS JOIN target
        CROSS JOIN boundary
        JOIN orchestration_v2_projection_turn_items AS item
          ON item.turn_item_id = context.item_id
        LEFT JOIN orchestration_v2_projection_runs AS run
          ON run.run_id = item.run_id
        LEFT JOIN orchestration_v2_projection_provider_threads AS session_thread
          ON run.thread_id = context.thread_id
          AND session_thread.provider_thread_id = run.provider_thread_id
          AND session_thread.first_run_ordinal IN (SELECT ordinal FROM scheduled)
        WHERE context.thread_id = ${threadId}
          AND (
            run.status IS NULL
            OR run.status <> 'rolled_back'
            OR run.thread_id <> context.thread_id
          )
          AND CASE
            WHEN target.session IS NOT NULL THEN session_thread.first_run_ordinal = target.session
            ELSE session_thread.first_run_ordinal IS NULL
              AND (
                boundary.min_ordinal = 0
                OR (run.thread_id = context.thread_id AND run.ordinal >= boundary.min_ordinal)
              )
          END
        ORDER BY context.updated_at, context.item_id
      `.pipe(NodeSqliteClient.readOnly, Effect.mapError(fail));
    });

    return McpAppModelContext.of({ set, forThread });
  }),
);

export const layerEmpty = Layer.succeed(
  McpAppModelContext,
  McpAppModelContext.of({
    set: Effect.fn("McpAppModelContext.set")(function () {
      return Effect.void;
    }),
    forThread: Effect.fn("McpAppModelContext.forThread")(function () {
      return Effect.succeed([]);
    }),
  }),
);
