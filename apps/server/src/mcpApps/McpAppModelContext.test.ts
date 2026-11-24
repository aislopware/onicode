import { RunId, ThreadId, TurnItemId } from "@onicode/contracts";
import { assert, it } from "@effect/vitest";
import * as NodeSqliteClient from "@onicode/shared/nodeSqliteClient";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as SqlClient from "effect/sql/SqlClient";

import { runMigrations } from "../persistence/Migrations.ts";
import * as McpAppModelContext from "./McpAppModelContext.ts";

const database = NodeSqliteClient.layer({ filename: ":memory:" });
const layer = it.layer(
  Layer.merge(database, McpAppModelContext.layer.pipe(Layer.provide(database))),
);

const threadId = ThreadId.make("thread-context");
const forkId = ThreadId.make("thread-context-fork");

const insertAppItem = (itemId: string, runId: string, runStatus: string) =>
  Effect.gen(function* () {
    const sql = yield* SqlClient.SqlClient;
    yield* sql`
      INSERT INTO orchestration_v2_projection_runs
        (run_id, thread_id, ordinal, provider, status, requested_at, payload_json)
      VALUES (${runId}, ${threadId}, ${runId.length}, 'codex', ${runStatus}, '2026-01-01', '{}')
    `;
    yield* sql`
      INSERT INTO orchestration_v2_projection_turn_items
        (turn_item_id, thread_id, run_id, ordinal, type, status, updated_at, payload_json)
      VALUES (${itemId}, ${threadId}, ${runId}, 0, 'dynamic_tool', 'completed', '2026-01-01', '{}')
    `;
  });

layer("McpAppModelContext", (it) => {
  it.effect("sends only apps still in the thread's history, including a fork's", () =>
    Effect.gen(function* () {
      yield* runMigrations();
      const store = yield* McpAppModelContext.McpAppModelContext;
      yield* insertAppItem("item-kept", "run-a", "completed");
      yield* insertAppItem("item-rolled-back", "run-bb", "rolled_back");
      const set = (thread: ThreadId, itemId: string, text: string) =>
        store.set({
          threadId: thread,
          itemId: TurnItemId.make(itemId),
          server: "todos",
          tool: "list_todos",
          text,
        });
      yield* set(threadId, "item-kept", "kept");
      yield* set(threadId, "item-rolled-back", "from a rolled-back run");
      yield* set(threadId, "item-deleted", "from an item that is gone");

      yield* set(forkId, "item-kept", "from the fork");
      yield* set(forkId, "item-rolled-back", "inherited before the rollback");

      const texts = (thread: ThreadId, runId: string) =>
        store
          .forThread(thread, RunId.make(runId))
          .pipe(Effect.map((entries) => entries.map((entry) => entry.text)));
      assert.deepEqual(yield* texts(threadId, "run-a"), ["kept"]);
      assert.deepEqual(yield* texts(forkId, "run-fork"), [
        "from the fork",
        "inherited before the rollback",
      ]);

      yield* set(threadId, "item-kept", "");
      assert.deepEqual(yield* texts(threadId, "run-a"), []);
    }),
  );

  it.effect("drops pre-/clear MCP app notes, but not for a restarted scheduled fresh run", () =>
    Effect.gen(function* () {
      yield* runMigrations();
      const sql = yield* SqlClient.SqlClient;
      const store = yield* McpAppModelContext.McpAppModelContext;
      const clearedThread = ThreadId.make("thread-cleared-context");

      yield* sql`
        INSERT INTO orchestration_v2_projection_runs
          (run_id, thread_id, ordinal, provider, status, requested_at, payload_json)
        VALUES
          ('run-before-clear', ${clearedThread}, 1, 'codex', 'completed', '2026-01-01', '{"startedAt":"2026-01-01"}'),
          ('run-clear', ${clearedThread}, 2, 'codex', 'completed', '2026-01-02', '{"freshContext":true,"startedAt":"2026-01-02","userMessageId":"msg-clear"}'),
          ('run-after-clear', ${clearedThread}, 3, 'codex', 'completed', '2026-01-03', '{"startedAt":"2026-01-03"}'),
          ('run-scheduled-fresh', ${clearedThread}, 4, 'codex', 'completed', '2026-01-04', '{"freshContext":true,"startedAt":"2026-01-04","userMessageId":"msg-restart"}')
      `;
      yield* sql`
        INSERT INTO orchestration_v2_projection_turn_items
          (turn_item_id, thread_id, run_id, ordinal, type, status, updated_at, payload_json)
        VALUES
          ('item-before-clear', ${clearedThread}, 'run-before-clear', 1, 'dynamic_tool', 'completed', '2026-01-01', '{}'),
          ('item-after-clear', ${clearedThread}, 'run-after-clear', 2, 'dynamic_tool', 'completed', '2026-01-03', '{}'),
          ('item-scheduled-prompt', ${clearedThread}, 'run-scheduled-fresh', 3, 'user_message', 'completed', '2026-01-04', '{"messageId":"msg-scheduled","scheduledTaskId":"sched-1"}'),
          ('item-restart-prompt', ${clearedThread}, 'run-scheduled-fresh', 4, 'user_message', 'completed', '2026-01-04', '{"messageId":"msg-restart"}')
      `;

      yield* store.set({
        threadId: clearedThread,
        itemId: TurnItemId.make("item-before-clear"),
        server: "todos",
        tool: "list_todos",
        text: "stale pre-clear note",
      });
      yield* store.set({
        threadId: clearedThread,
        itemId: TurnItemId.make("item-after-clear"),
        server: "todos",
        tool: "list_todos",
        text: "fresh post-clear note",
      });

      const entries = yield* store.forThread(clearedThread, RunId.make("run-after-clear"));
      assert.deepEqual(
        entries.map((entry) => entry.text),
        ["fresh post-clear note"],
      );
    }),
  );

  it.effect("keeps a scheduled fresh session's notes apart from the main conversation", () =>
    Effect.gen(function* () {
      yield* runMigrations();
      const sql = yield* SqlClient.SqlClient;
      const store = yield* McpAppModelContext.McpAppModelContext;
      const scheduledThread = ThreadId.make("thread-scheduled-context");

      yield* sql`
        INSERT INTO orchestration_v2_projection_runs
          (run_id, thread_id, ordinal, provider, provider_thread_id, status, requested_at, payload_json)
        VALUES
          ('run-main', ${scheduledThread}, 1, 'codex', 'pt-main', 'completed', '2026-01-01', '{"startedAt":"2026-01-01"}'),
          ('run-scheduled', ${scheduledThread}, 2, 'codex', 'pt-scheduled', 'completed', '2026-01-02', '{"freshContext":true,"startedAt":"2026-01-02"}'),
          ('run-main-later', ${scheduledThread}, 3, 'codex', 'pt-main', 'running', '2026-01-03', '{"startedAt":"2026-01-03"}')
      `;
      yield* sql`
        INSERT INTO orchestration_v2_projection_provider_threads
          (provider_thread_id, thread_id, provider, status, first_run_ordinal, updated_at, payload_json)
        VALUES
          ('pt-main', ${scheduledThread}, 'codex', 'active', 1, '2026-01-03', '{}'),
          ('pt-scheduled', ${scheduledThread}, 'codex', 'active', 2, '2026-01-02', '{}')
      `;
      yield* sql`
        INSERT INTO orchestration_v2_projection_turn_items
          (turn_item_id, thread_id, run_id, ordinal, type, status, updated_at, payload_json)
        VALUES
          ('item-main-app', ${scheduledThread}, 'run-main', 1, 'dynamic_tool', 'completed', '2026-01-01', '{}'),
          ('item-session-prompt', ${scheduledThread}, 'run-scheduled', 2, 'user_message', 'completed', '2026-01-02', '{"scheduledTaskId":"sched-1"}'),
          ('item-scheduled-app', ${scheduledThread}, 'run-scheduled', 3, 'dynamic_tool', 'completed', '2026-01-02', '{}')
      `;
      for (const [itemId, text] of [
        ["item-main-app", "main note"],
        ["item-scheduled-app", "scheduled note"],
      ] as const) {
        yield* store.set({
          threadId: scheduledThread,
          itemId: TurnItemId.make(itemId),
          server: "todos",
          tool: "list_todos",
          text,
        });
      }

      const texts = (runId: string) =>
        store
          .forThread(scheduledThread, RunId.make(runId))
          .pipe(Effect.map((entries) => entries.map((entry) => entry.text)));
      assert.deepEqual(yield* texts("run-main-later"), ["main note"]);
      assert.deepEqual(yield* texts("run-scheduled"), ["scheduled note"]);

      // Another thread's run with a matching first_run_ordinal must not select this thread's scheduled session.
      const otherThread = ThreadId.make("thread-scheduled-other");
      yield* sql`
        INSERT INTO orchestration_v2_projection_runs
          (run_id, thread_id, ordinal, provider, provider_thread_id, status, requested_at, payload_json)
        VALUES
          ('run-other-thread', ${otherThread}, 2, 'codex', 'pt-other', 'running', '2026-01-03', '{"startedAt":"2026-01-03"}')
      `;
      yield* sql`
        INSERT INTO orchestration_v2_projection_provider_threads
          (provider_thread_id, thread_id, provider, status, first_run_ordinal, updated_at, payload_json)
        VALUES
          ('pt-other', ${otherThread}, 'codex', 'active', 2, '2026-01-03', '{}')
      `;
      assert.deepEqual(yield* texts("run-other-thread"), ["main note"]);
    }),
  );
});
