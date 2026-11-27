import { OrchestrationV2TurnItem, RunId, ThreadId, TurnItemId } from "@onicode/contracts";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";
import * as SqlClient from "effect/sql/SqlClient";

export class TurnItemPositionStoreError extends Schema.TaggedError<TurnItemPositionStoreError>()(
  "TurnItemPositionStoreError",
  {
    threadId: ThreadId,
    turnItemId: TurnItemId,
    cause: Schema.optional(Schema.Defect()),
  },
) {}

const isTurnItemPositionStoreError = Schema.is(TurnItemPositionStoreError);

export interface TurnItemPositionReservation {
  readonly threadId: ThreadId;
  readonly runId: RunId;
  /** In report order. */
  readonly turnItemIds: ReadonlyArray<TurnItemId>;
}

export interface TurnItemPositionStoreV2Shape {
  readonly allocate: (input: {
    readonly threadId: ThreadId;
    readonly turnItemId: TurnItemId;
    readonly runId: RunId | null;
    readonly runOrdinal?: number;
    // `run_start` takes the run's unused first slot, ahead of its items, and appends once it is taken.
    readonly placement?: "append" | "run_start";
    // Keep an existing position even when it lies outside the run's range.
    readonly keepExisting?: boolean;
  }) => Effect.Effect<number, TurnItemPositionStoreError>;
  /**
   * Places the items that have no position yet at the end of their run's range, in order.
   * Does nothing until the run is projected, so no item is fixed in the runless range.
   */
  readonly reserve: (
    input: TurnItemPositionReservation,
  ) => Effect.Effect<void, TurnItemPositionStoreError>;
  readonly normalize: (
    item: OrchestrationV2TurnItem,
    runOrdinal?: number,
  ) => Effect.Effect<OrchestrationV2TurnItem, TurnItemPositionStoreError>;
}

export class TurnItemPositionStoreV2 extends Context.Service<
  TurnItemPositionStoreV2,
  TurnItemPositionStoreV2Shape
>()("onicode/orchestration-v2/TurnItemPositionStore/TurnItemPositionStoreV2") {}

export const layer: Layer.Layer<TurnItemPositionStoreV2, never, SqlClient.SqlClient> = Layer.effect(
  TurnItemPositionStoreV2,
  Effect.gen(function* () {
    const sql = yield* SqlClient.SqlClient;

    const allocate: TurnItemPositionStoreV2Shape["allocate"] =
      /* This helper runs for individual orchestration events; the enclosing operation owns the span. */
      Effect.fnUntraced(
        function* ({
          threadId,
          turnItemId,
          runId,
          runOrdinal: suppliedRunOrdinal,
          placement = "append",
          keepExisting = false,
        }) {
          const existing = yield* sql<{ readonly ordinal: number }>`
          SELECT ordinal
          FROM orchestration_v2_turn_item_positions
          WHERE thread_id = ${threadId} AND turn_item_id = ${turnItemId}
          LIMIT 1
        `;
          // A kept position needs no run range, so skip the run lookup on that per-event path.
          const runRows =
            runId === null ||
            suppliedRunOrdinal !== undefined ||
            (keepExisting && existing[0] !== undefined)
              ? []
              : yield* sql<{ readonly ordinal: number }>`
                SELECT ordinal
                FROM orchestration_v2_projection_runs
                WHERE thread_id = ${threadId} AND run_id = ${runId}
                LIMIT 1
              `;
          const runOrdinal = suppliedRunOrdinal ?? runRows[0]?.ordinal ?? null;
          const lowerBound = runOrdinal === null ? 0 : runOrdinal * 1_000_000;
          const upperBound = runOrdinal === null ? 999_999 : lowerBound + 999_999;
          const current = existing[0]?.ordinal;
          if (current !== undefined) {
            // A steer that missed its turn starts the next run; it moves into that run's range.
            if (
              keepExisting ||
              runOrdinal === null ||
              (current >= lowerBound && current <= upperBound)
            ) {
              return current;
            }
            yield* sql`
            UPDATE orchestration_v2_turn_item_positions
            SET ordinal = (
              SELECT COALESCE(MAX(ordinal), ${lowerBound}) + 1
              FROM orchestration_v2_turn_item_positions
              WHERE thread_id = ${threadId}
                AND ordinal >= ${lowerBound}
                AND ordinal <= ${upperBound}
            )
            WHERE thread_id = ${threadId} AND turn_item_id = ${turnItemId}
          `;
          } else {
            yield* sql`
            INSERT INTO orchestration_v2_turn_item_positions (thread_id, turn_item_id, ordinal)
            SELECT
              ${threadId},
              ${turnItemId},
              ${
                placement === "run_start"
                  ? sql`CASE WHEN COALESCE(MIN(ordinal), ${upperBound}) > ${lowerBound} THEN ${lowerBound} ELSE MAX(ordinal) + 1 END`
                  : sql`COALESCE(MAX(ordinal), ${lowerBound}) + 1`
              }
            FROM orchestration_v2_turn_item_positions
            WHERE thread_id = ${threadId}
              AND ordinal >= ${lowerBound}
              AND ordinal <= ${upperBound}
            ON CONFLICT(thread_id, turn_item_id) DO NOTHING
          `;
          }
          const rows = yield* sql<{ readonly ordinal: number }>`
          SELECT ordinal
          FROM orchestration_v2_turn_item_positions
          WHERE thread_id = ${threadId} AND turn_item_id = ${turnItemId}
          LIMIT 1
        `;
          const ordinal = rows[0]?.ordinal;
          if (ordinal === undefined) {
            return yield* new TurnItemPositionStoreError({
              threadId,
              turnItemId,
              cause: "Position allocation returned no row.",
            });
          }
          return ordinal;
        },
        (effect, { threadId, turnItemId }) =>
          effect.pipe(
            Effect.mapError((cause) =>
              isTurnItemPositionStoreError(cause)
                ? cause
                : new TurnItemPositionStoreError({ threadId, turnItemId, cause }),
            ),
          ),
      );

    // Runs only before a rare session-scoped write; the enclosing write owns the span.
    const reserve: TurnItemPositionStoreV2Shape["reserve"] = Effect.fnUntraced(function* ({
      threadId,
      runId,
      turnItemIds,
    }) {
      const first = turnItemIds[0];
      if (first === undefined) return;
      const failed = (turnItemId: TurnItemId) => (cause: unknown) =>
        new TurnItemPositionStoreError({ threadId, turnItemId, cause });
      const runRows = yield* sql<{ readonly ordinal: number }>`
        SELECT ordinal
        FROM orchestration_v2_projection_runs
        WHERE thread_id = ${threadId} AND run_id = ${runId}
        LIMIT 1
      `.pipe(Effect.mapError(failed(first)));
      const runOrdinal = runRows[0]?.ordinal;
      if (runOrdinal === undefined) return;
      const placed = yield* sql<{ readonly turn_item_id: string }>`
        SELECT turn_item_id
        FROM orchestration_v2_turn_item_positions
        WHERE thread_id = ${threadId} AND turn_item_id IN ${sql.in(turnItemIds)}
      `.pipe(Effect.mapError(failed(first)));
      const placedIds = new Set(placed.map((row) => row.turn_item_id));
      const lowerBound = runOrdinal * 1_000_000;
      for (const turnItemId of turnItemIds) {
        if (placedIds.has(turnItemId)) continue;
        yield* sql`
          INSERT INTO orchestration_v2_turn_item_positions (thread_id, turn_item_id, ordinal)
          SELECT ${threadId}, ${turnItemId}, COALESCE(MAX(ordinal), ${lowerBound}) + 1
          FROM orchestration_v2_turn_item_positions
          WHERE thread_id = ${threadId}
            AND ordinal >= ${lowerBound}
            AND ordinal <= ${lowerBound + 999_999}
          ON CONFLICT(thread_id, turn_item_id) DO NOTHING
        `.pipe(Effect.mapError(failed(turnItemId)));
      }
    });

    return TurnItemPositionStoreV2.of({
      allocate,
      reserve,
      // Per-event normalization shares the enclosing orchestration span.
      normalize: Effect.fnUntraced(function* (item, runOrdinal) {
        return yield* allocate({
          threadId: item.threadId,
          turnItemId: item.id,
          runId: item.runId,
          // Queued steers keep their submission order until their run starts (see Orchestrator).
          // Only a user message can change runs, so other items keep their position without a lookup.
          keepExisting: item.status === "pending" || item.type !== "user_message",
          ...(runOrdinal === undefined ? {} : { runOrdinal }),
        }).pipe(Effect.map((ordinal) => (item.ordinal === ordinal ? item : { ...item, ordinal })));
      }),
    });
  }),
);
